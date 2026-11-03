//! Hosts view (SPEC.md §10): the host list (search, filter chips, health
//! checks, a right-click menu), a collapsible patterns section, config
//! diagnostics, and the detail pane or the editor next to it.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Frame, Layout, Margin, RichText, Sense, Ui, UiBuilder, vec2};
use sshconf::{ConfigSet, HostEntry, HostList, PatternKind, Severity};
use sshelf_core::core::{AppData, Command, ConfigSnapshot, ConfigState, SshInfo};
use sshelf_core::health::HealthSnapshot;
use sshelf_core::metadata::HostMeta;
use sshelf_core::storage::LoadStatus;
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::host_detail::{DetailAction, HostDetail};
use crate::host_editor::HostEditor;
use crate::theme::{Palette, timing};
use crate::widgets::button::Button;
use crate::widgets::empty::empty_state;
use crate::widgets::{marks, popup, text};
use crate::{fonts, icons, motion, util};

/// Width of the host list pane.
const LIST_WIDTH: f32 = 320.0;
const ROW_HEIGHT: f32 = 40.0;
const ROW_GAP: f32 = 2.0;

/// Which hosts the list shows: one filter at a time (SPEC.md §10 *Layout*).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Favorites,
    Tag(String),
}

/// What the row menu asked for.
#[derive(Debug, Clone, PartialEq)]
enum RowAction {
    Connect,
    NewTunnel,
    Edit,
    Rename,
    Favorite(HostMeta),
    CopyAlias,
    CopySsh,
}

/// What the list asked for this frame.
#[derive(Default)]
struct ListAction {
    new_host: bool,
    commands: Vec<Command>,
    row: Option<(String, RowAction)>,
}

#[derive(Default)]
pub struct HostsView {
    search: String,
    filter: Filter,
    /// Primary alias of the selected host.
    selected: Option<String>,
    pub detail: HostDetail,
    pub editor: HostEditor,
    /// Latest health results (SPEC.md §12, M8).
    health: Option<Arc<HealthSnapshot>>,
    /// The hosts of the running *Check all*, for its "7 / 19".
    check_run: Vec<String>,
    /// Success messages for toasts ("Copied alias").
    notices: Vec<String>,
}

impl HostsView {
    pub fn on_health(&mut self, health: Arc<HealthSnapshot>) {
        if self.check_run.iter().all(|a| !health.checking.contains(a)) {
            self.check_run.clear();
        }
        self.health = Some(health);
    }

    /// Toast texts raised since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

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
        ui: &mut Ui,
        config: Option<&ConfigSnapshot>,
        data: Option<&AppData>,
        states: Option<&Snapshot>,
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
                if self.editor.is_open() {
                    let mut commands = self.editor.show(ui, config);
                    commands.extend(self.editor.show_keys(ui.ctx(), config, ssh));
                    return commands;
                }
                let path = config.path.display().to_string();
                let body = mono_line(&[
                    (&path, true),
                    (" doesn't exist. Your first host creates it.", false),
                ]);
                let new_host = empty_state(ui, icons::SERVER, "No ssh config yet", body, |ui| {
                    ui.add(Button::new("New host").icon(icons::PLUS).primary())
                        .clicked()
                });
                if new_host {
                    self.editor.open_new(config);
                }
                return Vec::new();
            }
            ConfigState::Error(e) => {
                ui.heading("Cannot read the ssh config");
                ui.colored_label(p.red, e);
                return Vec::new();
            }
        };

        if let Some(alias) = self.editor.take_created() {
            self.selected = Some(alias);
        }
        let action = egui::Panel::left("host_list")
            .resizable(false)
            .exact_size(LIST_WIDTH)
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin {
                left: 12,
                right: 12,
                top: 0,
                bottom: 16,
            }))
            .show(ui, |ui| {
                let rect = ui.max_rect().expand2(vec2(12.0, 0.0));
                ui.painter().vline(
                    rect.right(),
                    rect.y_range(),
                    egui::Stroke::new(1.0, p.bg_dark),
                );
                self.list(ui, set, hosts, data, states, ssh)
            })
            .inner;
        if action.new_host {
            self.editor.open_new(config);
            self.selected = None;
        }
        let mut commands = action.commands;
        if let Some((alias, row_action)) = action.row {
            match row_action {
                RowAction::Connect => commands.push(Command::Connect(alias)),
                RowAction::NewTunnel => self.detail.request_new_tunnel(alias),
                RowAction::Edit => {
                    self.selected = Some(alias.clone());
                    self.editor.open(config, &alias);
                }
                RowAction::Rename => {
                    self.selected = Some(alias.clone());
                    self.editor.open_rename(&alias);
                }
                RowAction::Favorite(meta) => commands.push(Command::SetMetadata { alias, meta }),
                RowAction::CopyAlias => {
                    ui.ctx().copy_text(alias);
                    self.notices.push("Copied alias".into());
                }
                RowAction::CopySsh => {
                    ui.ctx().copy_text(format!("ssh {alias}"));
                    self.notices.push("Copied ssh command".into());
                }
            }
        }

        // Another host was picked: an editor without changes gives way.
        if self.editor.is_open()
            && !self.editor.is_new()
            && self.editor.alias() != self.selected.as_deref()
            && !self.editor.is_dirty()
        {
            self.editor.close();
        }

        egui::CentralPanel::default().show(ui, |ui| {
            if self.editor.is_open() {
                commands.extend(self.editor.show(ui, config));
                return;
            }
            let host = self.selected.as_deref().and_then(|a| hosts.find(a));
            match host {
                Some(host) => commands.extend(self.detail.show(
                    ui,
                    host,
                    config,
                    data,
                    ssh,
                    self.health.as_deref(),
                )),
                None => {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Select a host").size(16.0).color(p.dim));
                    });
                }
            }
        });
        match self.detail.take_action() {
            Some(DetailAction::Edit(alias)) => self.editor.open(config, &alias),
            Some(DetailAction::Rename(alias)) => self.editor.open_rename(&alias),
            Some(DetailAction::Select(alias)) => self.selected = Some(alias),
            None => {}
        }
        commands.extend(self.editor.show_rename(ui.ctx(), config, data));
        commands.extend(self.editor.show_keys(ui.ctx(), config, ssh));
        commands
    }

    /// The list pane: header, search, filter chips, rows, patterns.
    fn list(
        &mut self,
        ui: &mut Ui,
        set: &ConfigSet,
        hosts: &HostList,
        data: &AppData,
        states: Option<&Snapshot>,
        ssh: Option<&SshInfo>,
    ) -> ListAction {
        let p = &Palette::ONE_DARK;
        let mut action = ListAction::default();
        let mut check_all = false;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Hosts")
                    .font(fonts::semibold(16.0))
                    .color(p.text_strong),
            );
            ui.label(
                RichText::new(hosts.hosts.len().to_string())
                    .size(13.0)
                    .color(p.dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if ui.add(Button::icon_only(icons::PLUS, "New host")).clicked() {
                    action.new_host = true;
                }
                check_all = self.check_all_button(ui, &mut action.commands);
            });
        });
        ui.add_space(12.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search aliases, tags, notes")
                .margin(Margin::symmetric(12, 8))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(12.0);
        self.chips(ui, data);
        diagnostics(ui, set);
        ui.add_space(12.0);

        // After the search field and chips, so their changes show at once.
        let needle = self.search.trim().to_lowercase();
        let mut rows: Vec<(&HostEntry, Option<&HostMeta>)> = hosts
            .hosts
            .iter()
            .map(|h| (h, data.hosts.get(h)))
            .filter(|(h, meta)| self.matches(h, *meta, &needle))
            .collect();
        // Favorites first, then config order (stable sort).
        rows.sort_by_key(|(_, meta)| !meta.is_some_and(|m| m.favorite));
        if check_all && !rows.is_empty() {
            self.check_run = rows.iter().map(|(h, _)| h.primary().to_string()).collect();
            action
                .commands
                .push(Command::CheckHealth(self.check_run.clone()));
        }

        let filtering = !needle.is_empty() || self.filter != Filter::All;
        egui::ScrollArea::vertical()
            .id_salt("hosts")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = ROW_GAP;
                if rows.is_empty() {
                    self.empty(ui, hosts, &mut action);
                } else {
                    let highlight = ui.painter().add(egui::Shape::Noop);
                    let mut selected_rect = None;
                    let can_connect = ssh.is_none_or(|s| s.tools.is_ok());
                    let meta_writable = !matches!(data.hosts_status, LoadStatus::ReadOnly(_));
                    for (host, meta) in &rows {
                        let tunnels = host_tunnels(host, data, states);
                        let (response, chosen) = self.row(
                            ui,
                            host,
                            *meta,
                            &tunnels,
                            &needle,
                            (can_connect, meta_writable),
                        );
                        if self.selected.as_deref() == Some(host.primary()) {
                            selected_rect = Some(response.rect);
                        }
                        if response.clicked() {
                            self.selected = Some(host.primary().to_string());
                        }
                        if let Some(chosen) = chosen {
                            action.row = Some((host.primary().to_string(), chosen));
                        }
                    }
                    // The selection background slides from row to row.
                    if let (Some(target), Some(alias)) = (selected_rect, &self.selected) {
                        let key = egui::Id::unique(alias).value() as usize;
                        let rect = motion::tween_rect(
                            ui.ctx(),
                            egui::Id::unique("host_list_selection"),
                            key,
                            target,
                            timing::SLIDE,
                        );
                        ui.painter().set(
                            highlight,
                            egui::Shape::rect_filled(
                                rect,
                                crate::theme::radius::CONTROL,
                                p.selection,
                            ),
                        );
                    }
                    if filtering {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.label(
                                RichText::new(format!(
                                    "{} of {} match",
                                    rows.len(),
                                    hosts.hosts.len()
                                ))
                                .size(12.0)
                                .color(p.dim),
                            );
                        });
                    }
                }
                ui.add_space(8.0);
                patterns(ui, set, hosts);
            });
        action
    }

    /// *Check all* (Lucide `activity`): checks the hosts the list shows. While
    /// it runs it shows a ring and "7 / 19"; clicking it again stops.
    /// Returns `true` when a new check was asked for (the caller knows the
    /// hosts shown); a stop is sent from here.
    fn check_all_button(&mut self, ui: &mut Ui, commands: &mut Vec<Command>) -> bool {
        if self.check_run.is_empty() {
            return ui
                .add(Button::icon_only(icons::ACTIVITY, "Check all hosts"))
                .clicked();
        }
        let total = self.check_run.len();
        let checking = self.health.as_deref().map_or(total, |h| {
            self.check_run
                .iter()
                .filter(|a| h.checking.contains(*a))
                .count()
        });
        let done = total - checking;
        let name = format!("Checking {done} of {total} hosts");
        let stop = ui
            .add(
                Button::new(format!("{done} / {total}"))
                    .busy()
                    .name(&name)
                    .tip("Click to stop"),
            )
            .clicked();
        if stop {
            self.check_run.clear();
            commands.push(Command::CancelHealth);
        }
        false
    }

    /// All, Favorites and one chip per tag; one filter at a time.
    fn chips(&mut self, ui: &mut Ui, data: &AppData) {
        let tags = data.hosts.all_tags();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if marks::filter_chip(ui, None, "All", self.filter == Filter::All).clicked() {
                self.filter = Filter::All;
            }
            let favorites = self.filter == Filter::Favorites;
            if marks::filter_chip(ui, Some(icons::STAR), "Favorites", favorites).clicked() {
                self.filter = if favorites {
                    Filter::All
                } else {
                    Filter::Favorites
                };
            }
            for tag in tags {
                let on = self.filter == Filter::Tag(tag.clone());
                if marks::filter_chip(ui, None, &tag, on).clicked() {
                    self.filter = if on { Filter::All } else { Filter::Tag(tag) };
                }
            }
        });
    }

    /// The list when no row is shown: only wildcard blocks, or nothing
    /// matching the search or filter.
    fn empty(&mut self, ui: &mut Ui, hosts: &HostList, action: &mut ListAction) {
        if hosts.hosts.is_empty() {
            let body = mono_line(&[
                ("Your config has ", false),
                ("Host *", true),
                (" blocks but no hosts you can connect to.", false),
            ]);
            if empty_state(ui, icons::ASTERISK, "Only wildcard blocks", body, |ui| {
                ui.add(Button::new("New host").icon(icons::PLUS).primary())
                    .clicked()
            }) {
                action.new_host = true;
            }
            return;
        }
        let search = self.search.trim();
        let title = if search.is_empty() {
            "No hosts in this filter".to_string()
        } else {
            format!("Nothing matches “{search}”")
        };
        let label = if search.is_empty() {
            "Show all"
        } else {
            "Clear search"
        };
        if empty_state(
            ui,
            icons::SEARCH,
            &title,
            "Searched aliases, tags and notes.",
            |ui| ui.add(Button::new(label)).clicked(),
        ) {
            self.search.clear();
            self.filter = Filter::All;
        }
    }

    fn matches(&self, host: &HostEntry, meta: Option<&HostMeta>, needle: &str) -> bool {
        let keep = match &self.filter {
            Filter::All => true,
            Filter::Favorites => meta.is_some_and(|m| m.favorite),
            Filter::Tag(tag) => meta.is_some_and(|m| m.tags.contains(tag)),
        };
        if !keep {
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

    /// One 40 px row: tunnel mark, alias (search matches bold), notes,
    /// health dot; the right-click menu. Returns the row's response and the
    /// menu item chosen, if any.
    fn row(
        &self,
        ui: &mut Ui,
        host: &HostEntry,
        meta: Option<&HostMeta>,
        tunnels: &HostTunnels,
        needle: &str,
        (can_connect, meta_writable): (bool, bool),
    ) -> (egui::Response, Option<RowAction>) {
        let p = &Palette::ONE_DARK;
        let alias = host.primary();
        let selected = self.selected.as_deref() == Some(alias);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        let enabled = response.enabled();
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, alias)
        });
        let menu_open = popup::menu_is_open(&response);
        let hover = motion::fade(
            ui.ctx(),
            response.id.with("hover"),
            (response.hovered() || menu_open) && !selected,
            timing::HOVER,
            motion::ease_out,
        );
        if hover > 0.0 {
            ui.painter().rect_filled(
                rect,
                crate::theme::radius::CONTROL,
                p.surface.gamma_multiply(hover),
            );
        }
        if menu_open && !selected {
            marks::outline(ui.painter(), rect, p.border);
        }
        let text_color = if selected { p.text_strong } else { p.text };
        let inner = rect.shrink2(vec2(12.0, 0.0));
        let layout = Layout::left_to_right(Align::Center);
        ui.scope_builder(UiBuilder::new().max_rect(inner).layout(layout), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            match tunnels.mark {
                Some(Mark::Dot(color)) => {
                    marks::dot(ui, color);
                }
                Some(Mark::Ring(color)) => {
                    marks::pulsing_ring(ui, marks::DOT, color);
                }
                None => match meta.and_then(|m| m.color) {
                    Some(color) => {
                        let [r, g, b] = color.0;
                        marks::dot(ui, Color32::from_rgb(r, g, b));
                    }
                    None => {
                        ui.add_space(marks::DOT);
                    }
                },
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                util::health_mark(ui, self.health.as_deref(), alias, 6.0);
                if tunnels.running > 0 {
                    let text = match tunnels.running {
                        1 => "1 tunnel".to_string(),
                        n => format!("{n} tunnels"),
                    };
                    marks::chip(ui, &text, p.green_tint, p.green);
                }
                if host.conditional {
                    let response = marks::note(ui, "conditional");
                    popup::tip_below(response, |ui| {
                        ui.label("Included inside a Host or Match block: it only applies when that block matches.");
                    });
                }
                if host.has_wildcards {
                    let response = marks::note(ui, "+wildcard");
                    popup::tip_below(response, |ui| {
                        ui.label("Its Host line also has wildcard patterns.");
                    });
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    text::truncated(ui, highlighted(alias, needle, text_color), alias);
                });
            });
        });

        let mut chosen = None;
        popup::context_menu(&response, |menu| {
            if menu.item(Some(icons::SQUARE_TERMINAL), "Connect", can_connect) {
                chosen = Some(RowAction::Connect);
            }
            if menu.item(None, "New tunnel…", true) {
                chosen = Some(RowAction::NewTunnel);
            }
            menu.separator();
            if menu.item(None, "Edit", true) {
                chosen = Some(RowAction::Edit);
            }
            if menu.item(None, "Rename…", true) {
                chosen = Some(RowAction::Rename);
            }
            let favorite = meta.is_some_and(|m| m.favorite);
            let label = if favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            };
            if menu.item(Some(icons::STAR), label, meta_writable) {
                let mut meta = meta.cloned().unwrap_or_default();
                meta.favorite = !favorite;
                chosen = Some(RowAction::Favorite(meta));
            }
            menu.separator();
            if menu.item(None, "Copy alias", true) {
                chosen = Some(RowAction::CopyAlias);
            }
            if menu.item(None, "Copy ssh command", true) {
                chosen = Some(RowAction::CopySsh);
            }
        });
        (response, chosen)
    }
}

/// A row's tunnel mark.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mark {
    Dot(Color32),
    Ring(Color32),
}

/// What a host's tunnels show in its row.
struct HostTunnels {
    running: usize,
    mark: Option<Mark>,
}

/// The tunnels on `host` (by any of its aliases): a red dot if one failed,
/// a pulsing yellow ring while one starts, green while any run.
fn host_tunnels(host: &HostEntry, data: &AppData, states: Option<&Snapshot>) -> HostTunnels {
    let p = &Palette::ONE_DARK;
    let mine: Vec<&TunnelState> = data
        .tunnels
        .tunnels
        .iter()
        .filter(|t| host.aliases.contains(&t.host))
        .filter_map(|t| states.and_then(|s| s.tunnels.get(&t.id)).map(|s| &s.state))
        .collect();
    let running = mine.iter().filter(|s| ***s == TunnelState::Running).count();
    let mark = if mine.iter().any(|s| matches!(s, TunnelState::Failed(_))) {
        Some(Mark::Dot(p.red))
    } else if mine.iter().any(|s| **s == TunnelState::Starting) {
        Some(Mark::Ring(p.yellow))
    } else if running > 0 {
        Some(Mark::Dot(p.green))
    } else {
        None
    };
    HostTunnels { running, mark }
}

/// `alias` in 14 px, with the parts matching `needle` in semibold and bright.
fn highlighted(alias: &str, needle: &str, color: Color32) -> LayoutJob {
    let p = &Palette::ONE_DARK;
    let normal = TextFormat::simple(FontId::proportional(14.0), color);
    let bold = TextFormat::simple(fonts::semibold(14.0), p.text_strong);
    let mut job = LayoutJob::default();
    let lower = alias.to_lowercase();
    // Byte offsets only line up when lowercasing kept the lengths.
    if needle.is_empty() || lower.len() != alias.len() {
        job.append(alias, 0.0, normal);
        return job;
    }
    let mut at = 0;
    while let Some(found) = lower[at..].find(needle) {
        let start = at + found;
        let end = start + needle.len();
        if start > at {
            job.append(&alias[at..start], 0.0, normal.clone());
        }
        job.append(&alias[start..end], 0.0, bold.clone());
        at = end;
    }
    if at < alias.len() {
        job.append(&alias[at..], 0.0, normal);
    }
    job
}

/// One line of 13 px text with some parts in mono (a path, `Host *`).
fn mono_line(parts: &[(&str, bool)]) -> LayoutJob {
    let p = &Palette::ONE_DARK;
    let mut job = LayoutJob::default();
    for (text, mono) in parts {
        let font = if *mono {
            FontId::monospace(13.0)
        } else {
            FontId::proportional(13.0)
        };
        job.append(text, 0.0, TextFormat::simple(font, p.dim));
    }
    job
}

fn diagnostics(ui: &mut Ui, set: &ConfigSet) {
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
        RichText::new(format!("{problems} config problem(s)")).color(p.yellow)
    } else {
        RichText::new(format!("{} note(s)", set.diagnostics.len())).color(p.dim)
    };
    ui.add_space(8.0);
    egui::CollapsingHeader::new(title)
        .id_salt("diagnostics")
        .show(ui, |ui| {
            for d in &set.diagnostics {
                let (icon, color) = match d.severity {
                    Severity::Error => (icons::CIRCLE_X, p.red),
                    Severity::Warning => (icons::TRIANGLE_ALERT, p.yellow),
                    Severity::Info => (icons::INFO, p.dim),
                };
                let (short, full) = util::location(set, d.at);
                ui.horizontal_wrapped(|ui| {
                    icons::show(ui, icon, icons::INLINE, color);
                    ui.label(RichText::new(short).color(p.dim))
                        .on_hover_text(full);
                    ui.label(&d.message);
                });
            }
        });
}

fn patterns(ui: &mut Ui, set: &ConfigSet, hosts: &HostList) {
    let p = &Palette::ONE_DARK;
    if hosts.patterns.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(
        RichText::new(format!("Patterns ({})", hosts.patterns.len()))
            .size(13.0)
            .color(p.dim),
    )
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
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshconf::{ConfigSet, HostList, ResolveOptions};
    use sshelf_core::core::{AppData, Command, ConfigSnapshot, ConfigState};
    use sshelf_core::metadata::{HostMeta, HostsFile};
    use sshelf_core::storage::LoadStatus;

    use super::*;
    use crate::test_util;

    fn snapshot(text: &str) -> Arc<ConfigSnapshot> {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        fs::write(&config, text).unwrap();
        let opts = ResolveOptions::new(tmp.path());
        let set = ConfigSet::load(&config, &opts).unwrap();
        let hosts = HostList::from_set(&set);
        Arc::new(ConfigSnapshot {
            generation: 1,
            path: config,
            state: ConfigState::Loaded { set, hosts },
            read_only: Vec::new(),
        })
    }

    fn fixture() -> (Arc<ConfigSnapshot>, Arc<AppData>) {
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
        (
            snapshot("Host web w1\nHost db\nHost *.prod\n"),
            Arc::new(data),
        )
    }

    fn harness(
        config: Arc<ConfigSnapshot>,
        data: Arc<AppData>,
    ) -> Harness<'static, (HostsView, Vec<Command>)> {
        test_util::harness()
            .with_size([1200.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (HostsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&config), Some(&data), None, None));
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
    fn health_checks_run_on_request_and_show_their_result() {
        use std::time::SystemTime;

        use sshelf_core::health::{Health, HealthReport, HealthSnapshot};

        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        // Only the shown hosts: favorites first (db), then config order.
        harness.get_by_label("Check all hosts").click();
        harness.step();
        assert_eq!(
            harness.state().1,
            [Command::CheckHealth(vec!["db".into(), "web".into()])]
        );

        harness.get_by_label("web").click();
        harness.step();
        harness.step();
        harness.get_by_label("not checked");
        harness.get_by_label("Check").click();
        harness.step();
        assert_eq!(
            harness.state().1.last(),
            Some(&Command::CheckHealth(vec!["web".into()]))
        );

        let mut health = HealthSnapshot::default();
        health.reports.insert(
            "web".into(),
            HealthReport {
                health: Health::HostKeyUnknown,
                checked_at: SystemTime::now(),
            },
        );
        harness.state_mut().0.on_health(Arc::new(health));
        harness.step();
        harness.get_by_label("Reachable, host key not known yet");
        harness.get_by_label_contains("Connect once to see and accept the host key");
    }

    #[test]
    fn check_all_shows_progress_and_stops_when_clicked_again() {
        use sshelf_core::health::HealthSnapshot;

        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("Check all hosts").click();
        harness.step();
        let mut health = HealthSnapshot::default();
        health.checking.insert("db".into());
        health.checking.insert("web".into());
        harness.state_mut().0.on_health(Arc::new(health.clone()));
        harness.step();
        harness.get_by_label("Checking 0 of 2 hosts");
        health.checking.remove("db");
        harness.state_mut().0.on_health(Arc::new(health));
        harness.step();
        harness.get_by_label("Checking 1 of 2 hosts").click();
        harness.step();
        harness.step();
        assert_eq!(harness.state().1.last(), Some(&Command::CancelHealth));
        harness.get_by_label("Check all hosts");
    }

    #[test]
    fn the_route_is_resolved_for_jumping_hosts_and_hops_are_links() {
        use sshelf_core::core::RouteHop;
        use sshelf_core::ssh::Resolved;

        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("web").click();
        harness.step();
        harness.step();
        harness.state_mut().0.detail.on_resolved(
            "web".into(),
            1,
            Ok(Resolved::parse(
                "hostname web.example.com\nproxyjump db\nproxycommand none\n",
            )),
        );
        harness.step();
        assert_eq!(
            harness.state().1.last(),
            Some(&Command::ResolveRoute("web".into()))
        );
        harness.state_mut().0.detail.on_route(
            "web".into(),
            1,
            Ok(vec![RouteHop {
                destination: "db".into(),
                hostname: "db.internal".into(),
                port: 22,
            }]),
        );
        harness.step();
        harness.get_by_label("you");
        // The hop is a link (the list row has the same name).
        harness
            .get_all_by_label("db")
            .find(|n| n.accesskit_node().role() == egui::accesskit::Role::Link)
            .expect("db as a link on the route")
            .click();
        harness.step();
        harness.step();
        assert_eq!(harness.state().0.selected.as_deref(), Some("db"));
    }

    #[test]
    fn search_filters_by_alias_and_tag_and_counts_matches() {
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
        harness.get_by_label("1 of 2 match");
    }

    #[test]
    fn filter_chips_show_one_kind_of_host_at_a_time() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("prod").click();
        harness.step();
        assert!(harness.query_by_label("web").is_none());
        harness.get_by_label("db");
        harness.get_by_label("Favorites").click();
        harness.step();
        assert_eq!(harness.state().0.filter, Filter::Favorites);
        harness.get_by_label("All").click();
        harness.step();
        harness.get_by_label("web");
    }

    #[test]
    fn nothing_matching_offers_to_clear_the_search() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.state_mut().0.search = "nope".into();
        harness.step();
        harness.get_by_label("Nothing matches “nope”");
        harness.get_by_label("Clear search").click();
        harness.step();
        harness.step();
        assert_eq!(harness.state().0.search, "");
        harness.get_by_label("web");
    }

    #[test]
    fn a_config_with_only_patterns_says_so() {
        let config = snapshot("Host *\n  ServerAliveInterval 60\n");
        let data = Arc::new(AppData::new(PathBuf::from("app")));
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("Only wildcard blocks");
        // The empty state's button (the list header has one too).
        harness.get_all_by_label("New host").last().unwrap().click();
        harness.step();
        harness.step();
        assert!(harness.state().0.editor.is_open());
    }

    #[test]
    fn the_row_menu_copies_and_favorites() {
        let (config, data) = fixture();
        let mut harness = harness(config, data);
        harness.step();
        harness.get_by_label("web").click_secondary();
        harness.step();
        harness.step();
        harness.get_by_label("Copy alias").click();
        harness.step();
        harness.step();
        assert_eq!(harness.state_mut().0.take_notices(), ["Copied alias"]);
        assert!(
            harness.query_by_label("Copy alias").is_none(),
            "menu closed"
        );

        harness.get_by_label("web").click_secondary();
        harness.step();
        harness.step();
        harness.get_by_label("Add to favorites").click();
        harness.step();
        harness.step();
        let Some(Command::SetMetadata { alias, meta }) = harness.state().1.last() else {
            panic!("{:?}", harness.state().1);
        };
        assert_eq!(alias, "web");
        assert!(meta.favorite);

        harness.get_by_label("db").click_secondary();
        harness.step();
        harness.step();
        harness.get_by_label("Edit").click();
        for _ in 0..3 {
            harness.step();
        }
        harness.get_by_label("Editing db");
    }

    #[test]
    fn search_matches_are_highlighted() {
        let job = highlighted("dev.example.org", "exam", Palette::ONE_DARK.text);
        let parts: Vec<&str> = job
            .sections
            .iter()
            .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(parts, ["dev.", "exam", "ple.org"]);
        assert_eq!(job.sections[1].format.color, Palette::ONE_DARK.text_strong);
    }
}
