//! Hosts view (SPEC.md §10): searchable host list with tags and favorites,
//! a collapsible patterns section, config diagnostics, and the detail pane.

use egui::{RichText, Sense};
use sshconf::{ConfigSet, HostEntry, HostList, PatternKind, Severity};
use sshelf_core::core::{AppData, Command, ConfigSnapshot, ConfigState, SshInfo};
use sshelf_core::metadata::HostMeta;

use crate::host_detail::{DetailAction, HostDetail};
use crate::host_editor::HostEditor;
use crate::theme::Palette;
use crate::util;

#[derive(Default)]
pub struct HostsView {
    search: String,
    tag: Option<String>,
    favorites_only: bool,
    /// Primary alias of the selected host.
    selected: Option<String>,
    pub detail: HostDetail,
    pub editor: HostEditor,
}

impl HostsView {
    /// Drops the selection when its host disappeared from the config (or
    /// follows it when it was renamed).
    pub fn on_config(&mut self, snapshot: &ConfigSnapshot) {
        let gone = match (&self.selected, snapshot.hosts()) {
            (Some(alias), Some(hosts)) => hosts.find(alias).is_none(),
            (Some(_), None) => true,
            _ => false,
        };
        if gone {
            let renamed = self
                .editor
                .renaming_to()
                .filter(|to| snapshot.hosts().is_some_and(|h| h.find(to).is_some()));
            self.selected = renamed.map(str::to_string);
        }
        self.editor.on_config(snapshot);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        config: Option<&ConfigSnapshot>,
        data: Option<&AppData>,
        ssh: Option<&SshInfo>,
    ) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let (Some(config), Some(data)) = (config, data) else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading your ssh config…");
            });
            return Vec::new();
        };
        let (set, hosts) = match &config.state {
            ConfigState::Loaded { set, hosts } => (set, hosts),
            ConfigState::Missing => {
                ui.heading("No ssh config yet");
                ui.label(format!(
                    "sshelf shows the hosts from {} as soon as it exists.",
                    config.path.display()
                ));
                return Vec::new();
            }
            ConfigState::Error(e) => {
                ui.heading("Cannot read the ssh config");
                ui.colored_label(p.red, e);
                return Vec::new();
            }
        };

        egui::Panel::left("host_list")
            .resizable(true)
            .default_size(340.0)
            .min_size(240.0)
            .show(ui, |ui| self.list(ui, set, hosts, data));

        // Another host was picked: an editor without changes gives way.
        if self.editor.is_open()
            && self.editor.alias() != self.selected.as_deref()
            && !self.editor.is_dirty()
        {
            self.editor.close();
        }

        let mut commands = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            if self.editor.is_open() {
                commands = self.editor.show(ui, config);
                return;
            }
            let host = self.selected.as_deref().and_then(|a| hosts.find(a));
            match host {
                Some(host) => commands = self.detail.show(ui, host, config, data, ssh),
                None => {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Select a host").color(p.dim));
                    });
                }
            }
        });
        match self.detail.take_action() {
            Some(DetailAction::Edit(alias)) => self.editor.open(config, &alias),
            Some(DetailAction::Rename(alias)) => self.editor.open_rename(&alias),
            None => {}
        }
        commands.extend(self.editor.show_rename(ui.ctx(), config, data));
        commands
    }

    fn list(&mut self, ui: &mut egui::Ui, set: &ConfigSet, hosts: &HostList, data: &AppData) {
        let p = &Palette::ONE_DARK;
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search aliases, tags, notes")
                .desired_width(f32::INFINITY),
        );
        let tags = data.hosts.all_tags();
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.favorites_only, "⭐ Favorites");
            for tag in &tags {
                let on = self.tag.as_ref() == Some(tag);
                if ui.selectable_label(on, format!("#{tag}")).clicked() {
                    self.tag = if on { None } else { Some(tag.clone()) };
                }
            }
        });
        diagnostics(ui, set);
        ui.separator();

        let needle = self.search.trim().to_lowercase();
        let mut rows: Vec<(&HostEntry, Option<&HostMeta>)> = hosts
            .hosts
            .iter()
            .map(|h| (h, data.hosts.get(h)))
            .filter(|(h, meta)| self.matches(h, *meta, &needle))
            .collect();
        // Favorites first, then config order (stable sort).
        rows.sort_by_key(|(_, meta)| !meta.is_some_and(|m| m.favorite));

        egui::ScrollArea::vertical()
            .id_salt("hosts")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.label(RichText::new("No matching hosts").color(p.dim));
                }
                for (host, meta) in rows {
                    self.row(ui, set, host, meta);
                }
                ui.add_space(8.0);
                patterns(ui, set, hosts);
            });
    }

    fn matches(&self, host: &HostEntry, meta: Option<&HostMeta>, needle: &str) -> bool {
        if self.favorites_only && !meta.is_some_and(|m| m.favorite) {
            return false;
        }
        if let Some(tag) = &self.tag
            && !meta.is_some_and(|m| m.tags.contains(tag))
        {
            return false;
        }
        if needle.is_empty() {
            return true;
        }
        host.aliases
            .iter()
            .any(|a| a.to_lowercase().contains(needle))
            || meta.is_some_and(|m| {
                m.tags.iter().any(|t| t.to_lowercase().contains(needle))
                    || m.notes.to_lowercase().contains(needle)
            })
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        set: &ConfigSet,
        host: &HostEntry,
        meta: Option<&HostMeta>,
    ) {
        let p = &Palette::ONE_DARK;
        let selected = self.selected.as_deref() == Some(host.primary());
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            if let Some(color) = meta.and_then(|m| m.color) {
                let [r, g, b] = color.0;
                ui.painter()
                    .circle_filled(dot.center(), 4.0, egui::Color32::from_rgb(r, g, b));
            }
            let mut name = RichText::new(host.primary());
            if selected {
                name = name.strong();
            }
            if ui.selectable_label(selected, name).clicked() {
                self.selected = Some(host.primary().to_string());
            }
            if meta.is_some_and(|m| m.favorite) {
                ui.label(RichText::new("⭐").color(p.yellow));
            }
            if host.aliases.len() > 1 {
                ui.label(RichText::new(host.aliases[1..].join(" ")).color(p.dim));
            }
            for tag in meta.map(|m| m.tags.as_slice()).unwrap_or_default() {
                ui.label(RichText::new(format!("#{tag}")).small().color(p.accent));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (short, full) = util::location(set, host.at);
                ui.label(RichText::new(short).small().color(p.dim))
                    .on_hover_text(full);
                if !host.also_in.is_empty() {
                    ui.label(RichText::new("+").small().color(p.dim))
                        .on_hover_text("Also configured in other blocks");
                }
                if host.conditional {
                    ui.label(RichText::new("cond").small().color(p.yellow))
                        .on_hover_text("Included inside a Host/Match block");
                }
            });
        });
    }
}

fn diagnostics(ui: &mut egui::Ui, set: &ConfigSet) {
    let p = &Palette::ONE_DARK;
    if set.diagnostics.is_empty() {
        return;
    }
    let problems = set
        .diagnostics
        .iter()
        .filter(|d| d.severity > Severity::Info)
        .count();
    let title = if problems > 0 {
        RichText::new(format!("⚠ {problems} config problem(s)")).color(p.yellow)
    } else {
        RichText::new(format!("ℹ {} note(s)", set.diagnostics.len())).color(p.dim)
    };
    egui::CollapsingHeader::new(title)
        .id_salt("diagnostics")
        .show(ui, |ui| {
            for d in &set.diagnostics {
                let (icon, color) = match d.severity {
                    Severity::Error => ("✖", p.red),
                    Severity::Warning => ("⚠", p.yellow),
                    Severity::Info => ("ℹ", p.dim),
                };
                let (short, full) = util::location(set, d.at);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(icon).color(color));
                    ui.label(RichText::new(short).color(p.dim))
                        .on_hover_text(full);
                    ui.label(&d.message);
                });
            }
        });
}

fn patterns(ui: &mut egui::Ui, set: &ConfigSet, hosts: &HostList) {
    let p = &Palette::ONE_DARK;
    if hosts.patterns.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(format!("Patterns ({})", hosts.patterns.len()))
        .id_salt("patterns")
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Wildcard and Match blocks apply to other hosts; they are not connectable.",
                )
                .small()
                .color(p.dim),
            );
            for pattern in &hosts.patterns {
                let text = match &pattern.kind {
                    PatternKind::Host(patterns) => {
                        let words: Vec<String> = patterns
                            .iter()
                            .map(|p| format!("{}{}", if p.negated { "!" } else { "" }, p.pattern))
                            .collect();
                        format!("Host {}", words.join(" "))
                    }
                    PatternKind::Match(args) => format!("Match {}", args.join(" ")),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(text).monospace());
                    let (short, full) = util::location(set, pattern.at);
                    ui.label(RichText::new(short).small().color(p.dim))
                        .on_hover_text(full);
                    if pattern.conditional {
                        ui.label(RichText::new("cond").small().color(p.yellow));
                    }
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sshconf::{ConfigSet, HostList, ResolveOptions};
    use sshelf_core::core::{AppData, Command, ConfigSnapshot, ConfigState};
    use sshelf_core::metadata::{HostMeta, HostsFile};
    use sshelf_core::storage::LoadStatus;

    use super::*;

    fn fixture() -> (Arc<ConfigSnapshot>, Arc<AppData>) {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        fs::write(&config, "Host web w1\nHost db\nHost *.prod\n").unwrap();
        let opts = ResolveOptions::new(tmp.path());
        let set = ConfigSet::load(&config, &opts).unwrap();
        let hosts = HostList::from_set(&set);
        let snapshot = ConfigSnapshot {
            generation: 1,
            path: config,
            state: ConfigState::Loaded { set, hosts },
            read_only: Vec::new(),
        };
        let mut meta = HostsFile::default();
        meta.hosts.insert(
            "db".into(),
            HostMeta {
                tags: vec!["prod".into()],
                favorite: true,
                ..HostMeta::default()
            },
        );
        let data = AppData {
            hosts: meta,
            hosts_status: LoadStatus::Loaded,
            ..AppData::new(PathBuf::from("app"))
        };
        (Arc::new(snapshot), Arc::new(data))
    }

    fn harness(
        config: Arc<ConfigSnapshot>,
        data: Arc<AppData>,
    ) -> Harness<'static, (HostsView, Vec<Command>)> {
        Harness::builder()
            .with_size([1200.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (HostsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&config), Some(&data), None));
                },
                (HostsView::default(), Vec::new()),
            )
    }

    #[test]
    fn lists_hosts_and_patterns() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("web");
        harness.get_by_label("db");
        harness.get_by_label("Patterns (1)");
        harness.get_by_label("Select a host");
    }

    #[test]
    fn selecting_a_host_shows_details_and_resolves_it() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("web").click();
        for _ in 0..3 {
            harness.step();
        }
        harness.get_by_label("Defined at");
        harness.get_by_label("Effective config (ssh -G)");
        assert!(harness.query_by_label("Select a host").is_none());
        let commands = &harness.state().1;
        assert_eq!(commands, &[Command::Resolve("web".into())]);
    }

    #[test]
    fn new_tunnel_from_host_detail_names_the_host() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("web").click();
        harness.step();
        harness.step();
        harness.get_by_label("+ New tunnel").click();
        harness.step();
        assert_eq!(
            harness.state_mut().0.detail.take_new_tunnel().as_deref(),
            Some("web")
        );
        assert_eq!(harness.state_mut().0.detail.take_new_tunnel(), None);
    }

    #[test]
    fn edit_opens_the_editor_in_place_of_the_details() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("web").click();
        harness.step();
        harness.step();
        harness.get_by_label("Edit").click();
        for _ in 0..3 {
            harness.step();
        }
        harness.get_by_label("Editing web");
        assert!(harness.query_by_label("Defined at").is_none());
        // Picking another host closes an editor without changes.
        harness.get_by_label("db").click();
        for _ in 0..3 {
            harness.step();
        }
        assert!(!harness.state().0.editor.is_open());
        harness.get_by_label("Defined at");
    }

    #[test]
    fn search_filters_by_alias_and_tag() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .click();
        harness.step();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("prod");
        harness.step();
        harness.step();
        assert_eq!(harness.state().0.search, "prod");
        assert!(harness.query_by_label("web").is_none());
        harness.get_by_label("db");
    }
}
