//! Host detail pane: where the host is defined, metadata editing, Connect,
//! and the read-only `ssh -G` panel (SPEC.md §4, §10).

use std::collections::{HashMap, HashSet};

use egui::RichText;
use sshconf::{ConfigSet, HostEntry};
use sshelf_core::core::{AppData, Command, SshInfo};
use sshelf_core::metadata::{Color, HostMeta};
use sshelf_core::ssh::Resolved;
use sshelf_core::storage::LoadStatus;

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
        set: &ConfigSet,
        generation: u64,
        data: &AppData,
        ssh: Option<&SshInfo>,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let alias = host.primary().to_string();
        self.sync_draft(&alias, data.hosts.get(host).cloned().unwrap_or_default());
        let current = self
            .resolved
            .get(&alias)
            .is_some_and(|(g, _)| *g == generation);
        if !current && self.requested.insert((alias.clone(), generation)) {
            commands.push(Command::Resolve(alias.clone()));
        }

        egui::ScrollArea::vertical()
            .id_salt("host_detail")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.header(ui, host, set, data, ssh, &mut commands);
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
        set: &ConfigSet,
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
                .add_enabled(tools_error.is_none(), egui::Button::new("▶ Connect"))
                .on_hover_text("Open a terminal running ssh for this host")
                .on_disabled_hover_text(tools_error.unwrap_or_default());
            if button.clicked() {
                commands.push(Command::Connect(host.primary().to_string()));
            }
            let last = match data.history.last_launch(host) {
                Some(time) => format!("Last launched from sshelf {}", util::ago(time)),
                None => "Never launched from sshelf".to_string(),
            };
            ui.label(RichText::new(last).color(p.dim))
                .on_hover_text("Only sessions started from sshelf can be tracked.");
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
                    ui.label("Favorite");
                    ui.checkbox(&mut self.draft.favorite, "⭐");
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
                    if ui.button("↻").on_hover_text("Run ssh -G again").clicked() {
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
