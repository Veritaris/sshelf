//! Host detail pane: where the host is defined, metadata editing, Connect,
//! and the read-only `ssh -G` panel (SPEC.md §4, §10).

use std::collections::{HashMap, HashSet};

use egui::RichText;
use sshconf::{ConfigSet, HostEntry, HostList};
use sshelf_core::core::{AppData, Command, ConfigSnapshot, RouteHop, SshInfo};
use sshelf_core::health::{Health, HealthSnapshot};
use sshelf_core::hints;
use sshelf_core::metadata::{Color, HostMeta};
use sshelf_core::ssh::Resolved;
use sshelf_core::storage::LoadStatus;

use crate::icons;
use crate::theme::Palette;
use crate::util;

/// `ssh -G` keys shown first, in this order.
const IMPORTANT: &[&str] = &[
    "hostname",
    "user",
    "port",
    "proxyjump",
    "proxycommand",
    "identityfile",
    "identitiesonly",
    "identityagent",
    "forwardagent",
    "localforward",
    "remoteforward",
    "dynamicforward",
    "serveraliveinterval",
];

#[derive(Default)]
pub struct HostDetail {
    /// Host the draft belongs to and the metadata it started from.
    for_alias: Option<String>,
    base: HostMeta,
    draft: Draft,
    /// Alias → (config generation, `ssh -G` result).
    resolved: HashMap<String, (u64, Result<Resolved, String>)>,
    requested: HashSet<(String, u64)>,
    filter: String,
    /// "New tunnel" was clicked for this host; the app opens the editor.
    new_tunnel: Option<String>,
    /// "Edit" / "Rename…" was clicked for this host.
    action: Option<DetailAction>,
    /// Alias → (config generation, jump hosts on the way).
    routes: HashMap<String, (u64, Result<Vec<RouteHop>, String>)>,
    routes_requested: HashSet<(String, u64)>,
}

/// Actions the hosts view carries out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailAction {
    Edit(String),
    Rename(String),
    /// A hop on the route was clicked: show that host.
    Select(String),
}

#[derive(Default, Clone, PartialEq)]
struct Draft {
    favorite: bool,
    color: Option<[u8; 3]>,
    tags: String,
    notes: String,
}

impl Draft {
    fn from_meta(meta: &HostMeta) -> Self {
        Self {
            favorite: meta.favorite,
            color: meta.color.map(|c| c.0),
            tags: meta.tags.join(", "),
            notes: meta.notes.clone(),
        }
    }

    fn to_meta(&self) -> HostMeta {
        let mut tags: Vec<String> = Vec::new();
        for tag in self
            .tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            if !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        }
        HostMeta {
            tags,
            notes: self.notes.trim_end().to_string(),
            favorite: self.favorite,
            color: self.color.map(Color),
        }
    }
}

impl HostDetail {
    /// The host "New tunnel" was clicked for, once.
    pub fn take_new_tunnel(&mut self) -> Option<String> {
        self.new_tunnel.take()
    }

    /// "New tunnel…" from somewhere else (the host list's menu).
    pub fn request_new_tunnel(&mut self, alias: String) {
        self.new_tunnel = Some(alias);
    }

    pub fn take_action(&mut self) -> Option<DetailAction> {
        self.action.take()
    }

    pub fn on_route(
        &mut self,
        alias: String,
        generation: u64,
        result: Result<Vec<RouteHop>, String>,
    ) {
        let newer = self
            .routes
            .get(&alias)
            .is_none_or(|(known, _)| generation >= *known);
        if newer {
            self.routes.insert(alias, (generation, result));
        }
    }

    pub fn on_resolved(
        &mut self,
        alias: String,
        generation: u64,
        result: Result<Resolved, String>,
    ) {
        let newer = self
            .resolved
            .get(&alias)
            .is_none_or(|(known, _)| generation >= *known);
        if newer {
            self.resolved.insert(alias, (generation, result));
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostEntry,
        config: &ConfigSnapshot,
        data: &AppData,
        ssh: Option<&SshInfo>,
        health: Option<&HealthSnapshot>,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some(set) = config.set() else {
            return commands;
        };
        let generation = config.generation;
        let read_only = config.read_only(host.at.file);
        let alias = host.primary().to_string();
        self.sync_draft(&alias, data.hosts.get(host).cloned().unwrap_or_default());
        let current = self
            .resolved
            .get(&alias)
            .is_some_and(|(g, _)| *g == generation);
        if !current && self.requested.insert((alias.clone(), generation)) {
            commands.push(Command::Resolve(alias.clone()));
        }
        // The route needs `ssh -G` per hop: only for hosts that jump.
        let resolved = self
            .resolved
            .get(&alias)
            .filter(|(g, _)| *g == generation)
            .and_then(|(_, r)| r.as_ref().ok());
        let jumps = resolved
            .and_then(|r| r.get("proxyjump"))
            .is_some_and(|j| !j.eq_ignore_ascii_case("none"));
        if jumps && self.routes_requested.insert((alias.clone(), generation)) {
            commands.push(Command::ResolveRoute(alias.clone()));
        }
        let proxy_command = resolved
            .and_then(|r| r.get("proxycommand"))
            .filter(|c| !c.eq_ignore_ascii_case("none"))
            .map(str::to_string);
        let route = jumps.then(|| {
            self.routes
                .get(&alias)
                .filter(|(g, _)| *g == generation)
                .map(|(_, r)| r.clone())
        });
        let hosts = config.hosts();

        egui::ScrollArea::vertical()
            .id_salt("host_detail")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.header(ui, host, (set, read_only), data, ssh, &mut commands);
                if let Some(select) = route_line(ui, &alias, route, proxy_command, hosts) {
                    self.action = Some(DetailAction::Select(select));
                }
                health_line(ui, &alias, health, ssh, &mut commands);
                ui.separator();
                self.metadata(ui, &alias, data, &mut commands);
                ui.separator();
                self.effective(ui, &alias, current, &mut commands);
            });
        commands
    }

    /// Resets the draft when another host is selected, or when the saved
    /// metadata changed and there are no unsaved edits.
    fn sync_draft(&mut self, alias: &str, meta: HostMeta) {
        let other_host = self.for_alias.as_deref() != Some(alias);
        let dirty = self.draft.to_meta() != self.base;
        if other_host || (!dirty && meta != self.base) {
            self.for_alias = Some(alias.to_string());
            self.draft = Draft::from_meta(&meta);
            self.base = meta;
            self.filter.clear();
        }
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostEntry,
        (set, read_only): (&ConfigSet, Option<&str>),
        data: &AppData,
        ssh: Option<&SshInfo>,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(RichText::new(host.primary()).color(p.accent).strong());
            if host.aliases.len() > 1 {
                ui.label(RichText::new(host.aliases[1..].join(" ")).color(p.dim));
            }
        });

        let (short, full) = util::location(set, host.at);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Defined at").color(p.dim));
            ui.label(short).on_hover_text(full);
        });
        for &at in &host.also_in {
            let (short, full) = util::location(set, at);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Also configured at").color(p.dim));
                ui.label(short).on_hover_text(full);
            });
        }
        if host.conditional {
            ui.colored_label(
                p.yellow,
                "Included inside a Host/Match block: it only applies when that block matches too.",
            );
        }
        if host.has_wildcards {
            ui.label(
                RichText::new("The defining block also has wildcard patterns.")
                    .small()
                    .color(p.dim),
            );
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let tools_error = match ssh.map(|s| &s.tools) {
                Some(Ok(_)) => None,
                Some(Err(e)) => Some(e.clone()),
                None => Some("still detecting ssh".to_string()),
            };
            let button = ui
                .add_enabled(
                    tools_error.is_none(),
                    egui::Button::new((icons::atom(icons::SQUARE_TERMINAL), "Connect")),
                )
                .on_hover_text("Open a terminal running ssh for this host")
                .on_disabled_hover_text(tools_error.unwrap_or_default());
            if button.clicked() {
                commands.push(Command::Connect(host.primary().to_string()));
            }
            if ui
                .button("+ New tunnel")
                .on_hover_text("Create a tunnel through this host")
                .clicked()
            {
                self.new_tunnel = Some(host.primary().to_string());
            }
            let last = match data.history.last_launch(host) {
                Some(time) => format!("Last launched from sshelf {}", util::ago(time)),
                None => "Never launched from sshelf".to_string(),
            };
            ui.label(RichText::new(last).color(p.dim))
                .on_hover_text("Only sessions started from sshelf can be tracked.");
        });
        ui.horizontal(|ui| {
            let why = read_only.map(|r| format!("Read-only: {r}"));
            let edit = ui
                .add_enabled(why.is_none(), egui::Button::new("Edit"))
                .on_hover_text("Edit this host's block in the config")
                .on_disabled_hover_text(why.clone().unwrap_or_default());
            if edit.clicked() {
                self.action = Some(DetailAction::Edit(host.primary().to_string()));
            }
            let rename = ui
                .add_enabled(why.is_none(), egui::Button::new("Rename…"))
                .on_hover_text("Rename this host in the config and in sshelf's data")
                .on_disabled_hover_text(why.clone().unwrap_or_default());
            if rename.clicked() {
                self.action = Some(DetailAction::Rename(host.primary().to_string()));
            }
            if let Some(why) = &why {
                ui.label(RichText::new(why).small().color(p.yellow));
            }
        });
    }

    fn metadata(
        &mut self,
        ui: &mut egui::Ui,
        alias: &str,
        data: &AppData,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        ui.label(RichText::new("Notes and tags").strong());
        let read_only = match &data.hosts_status {
            LoadStatus::ReadOnly(reason) => Some(reason.clone()),
            _ => None,
        };
        if let Some(reason) = &read_only {
            ui.colored_label(p.yellow, format!("hosts.toml is read-only: {reason}"));
        }
        ui.add_enabled_ui(read_only.is_none(), |ui| {
            egui::Grid::new("meta")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    let label = ui.label("Favorite");
                    ui.add(egui::Checkbox::without_text(&mut self.draft.favorite))
                        .labelled_by(label.id);
                    ui.end_row();

                    ui.label("Color");
                    ui.horizontal(|ui| {
                        let mut on = self.draft.color.is_some();
                        if ui.checkbox(&mut on, "").changed() {
                            self.draft.color = on.then_some([0xc6, 0x78, 0xdd]);
                        }
                        if let Some(rgb) = &mut self.draft.color {
                            ui.color_edit_button_srgb(rgb);
                        }
                    });
                    ui.end_row();

                    ui.label("Tags");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.tags)
                            .hint_text("comma-separated, e.g. prod, web")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Notes");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft.notes)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });

            let dirty = self.draft.to_meta() != self.base;
            ui.horizontal(|ui| {
                if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                    commands.push(Command::SetMetadata {
                        alias: alias.to_string(),
                        meta: self.draft.to_meta(),
                    });
                }
                if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                    self.draft = Draft::from_meta(&self.base);
                }
                if dirty {
                    ui.label(RichText::new("unsaved changes").small().color(p.yellow));
                }
            });
        });
    }

    fn effective(
        &mut self,
        ui: &mut egui::Ui,
        alias: &str,
        current: bool,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        egui::CollapsingHeader::new(RichText::new("Effective config (ssh -G)").strong())
            .id_salt("effective")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Filter")
                            .desired_width(200.0),
                    );
                    if icons::button(ui, icons::REFRESH_CW, "Run ssh -G again").clicked() {
                        self.resolved.remove(alias);
                        self.requested.retain(|(a, _)| a != alias);
                        commands.push(Command::Resolve(alias.to_string()));
                    }
                    if !current {
                        ui.spinner();
                    }
                });
                match self.resolved.get(alias) {
                    None => {}
                    Some((_, Err(e))) => {
                        ui.colored_label(p.red, e);
                    }
                    Some((_, Ok(resolved))) => grid(ui, resolved, &self.filter),
                }
            });
    }
}

/// Between route hops.
const ARROW: &str = "›";

/// "Route: you › bastion › db-gw › this host" (SPEC.md §12, M8). Hops that
/// are hosts of the config can be clicked; returns the clicked one.
/// `route` is `None` for hosts without `ProxyJump`, `Some(None)` while it
/// is being worked out.
fn route_line(
    ui: &mut egui::Ui,
    alias: &str,
    route: Option<Option<Result<Vec<RouteHop>, String>>>,
    proxy_command: Option<String>,
    hosts: Option<&HostList>,
) -> Option<String> {
    let p = &Palette::ONE_DARK;
    let mut clicked = None;
    if route.is_none() && proxy_command.is_none() {
        return None;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Route").color(p.dim));
        ui.label("you");
        match route {
            Some(Some(Ok(hops))) => {
                for hop in hops {
                    ui.label(RichText::new(ARROW).color(p.dim));
                    let tip = format!("{}:{}", hop.hostname, hop.port);
                    let known = hosts.and_then(|h| h.find(&hop.destination));
                    match known {
                        Some(host) => {
                            if ui.link(&hop.destination).on_hover_text(tip).clicked() {
                                clicked = Some(host.primary().to_string());
                            }
                        }
                        None => {
                            ui.label(RichText::new(&hop.destination).monospace())
                                .on_hover_text(tip);
                        }
                    }
                }
            }
            Some(Some(Err(e))) => {
                ui.label(RichText::new(format!("{ARROW} ?")).color(p.dim));
                ui.colored_label(p.yellow, e);
            }
            Some(None) => {
                ui.label(RichText::new(ARROW).color(p.dim));
                ui.spinner();
            }
            None => {}
        }
        if let Some(command) = &proxy_command {
            ui.label(RichText::new(ARROW).color(p.dim));
            ui.label(RichText::new("ProxyCommand").italics())
                .on_hover_text(command);
        }
        ui.label(RichText::new(ARROW).color(p.dim));
        ui.label(RichText::new(alias).strong());
    });
    clicked
}

/// "Health: Auth OK, login took 240 ms · 2 min ago [Check]" and a hint for
/// what to do about a problem (SPEC.md §12, M8).
fn health_line(
    ui: &mut egui::Ui,
    alias: &str,
    health: Option<&HealthSnapshot>,
    ssh: Option<&SshInfo>,
    commands: &mut Vec<Command>,
) {
    let p = &Palette::ONE_DARK;
    let checking = health.is_some_and(|h| h.checking.contains(alias));
    let report = health.and_then(|h| h.reports.get(alias));
    ui.horizontal(|ui| {
        ui.label(RichText::new("Health").color(p.dim));
        util::health_mark(ui, health, alias, 8.0);
        match (checking, report) {
            (true, _) => {
                ui.spinner();
                ui.label("checking…");
            }
            (false, Some(report)) => {
                ui.label(report.health.describe());
                ui.label(
                    RichText::new(util::ago(report.checked_at))
                        .small()
                        .color(p.dim),
                );
            }
            (false, None) => {
                ui.label(RichText::new("not checked").color(p.dim));
            }
        }
        if ui
            .add_enabled(!checking, egui::Button::new("Check"))
            .on_hover_text(
                "Log in once without a password (ssh BatchMode, 3 s connect timeout) \
                 and run `true`",
            )
            .clicked()
        {
            commands.push(Command::CheckHealth(vec![alias.to_string()]));
        }
    });
    let hint = match report.map(|r| &r.health) {
        Some(Health::AuthFailed) if !checking => Some(hints::auth_hint(
            ssh.and_then(|s| s.agent.as_ref()),
            ssh.and_then(|s| s.service.as_ref())
                .and_then(|s| s.as_ref().ok()),
        )),
        Some(Health::HostKeyUnknown) if !checking => Some(
            "Connect once to see and accept the host key in the terminal, or start a tunnel \
             through this host."
                .to_string(),
        ),
        Some(Health::HostKeyChanged) if !checking => {
            Some("Start a tunnel through this host to review the changed key.".to_string())
        }
        Some(Health::Unreachable(why)) if !checking => hints::failure_hint(why).map(str::to_string),
        _ => None,
    };
    if let Some(hint) = hint {
        ui.label(RichText::new(hint).small().color(p.yellow));
    }
}

fn grid(ui: &mut egui::Ui, resolved: &Resolved, filter: &str) {
    let p = &Palette::ONE_DARK;
    let needle = filter.trim().to_lowercase();
    let wanted = |(k, v): &&(String, String)| {
        needle.is_empty() || k.contains(&needle) || v.to_lowercase().contains(&needle)
    };
    let important = IMPORTANT
        .iter()
        .flat_map(|key| resolved.options.iter().filter(move |(k, _)| k == key));
    let rest = resolved
        .options
        .iter()
        .filter(|(k, _)| !IMPORTANT.contains(&k.as_str()));
    egui::Grid::new("resolved")
        .num_columns(2)
        .striped(true)
        .spacing([16.0, 2.0])
        .show(ui, |ui| {
            for (key, value) in important.chain(rest).filter(wanted) {
                let is_important = IMPORTANT.contains(&key.as_str());
                let key_text = RichText::new(key).monospace();
                ui.label(if is_important {
                    key_text.color(p.accent)
                } else {
                    key_text.color(p.dim)
                });
                ui.label(RichText::new(value).monospace());
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_tags_are_trimmed_and_deduplicated() {
        let draft = Draft {
            tags: " prod, web ,, prod,db ".into(),
            notes: "note\n\n".into(),
            ..Draft::default()
        };
        let meta = draft.to_meta();
        assert_eq!(meta.tags, ["prod", "web", "db"]);
        assert_eq!(meta.notes, "note");
        assert_eq!(Draft::from_meta(&meta).tags, "prod, web, db");
    }

    #[test]
    fn untouched_draft_matches_its_metadata() {
        let meta = HostMeta {
            tags: vec!["a".into()],
            notes: "n".into(),
            favorite: true,
            color: Color::parse("#112233"),
        };
        assert_eq!(Draft::from_meta(&meta).to_meta(), meta);
    }
}
