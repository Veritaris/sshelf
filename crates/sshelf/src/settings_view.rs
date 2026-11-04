//! Settings (SPEC.md §8, §10; board A-Settings): two columns of cards —
//! Appearance (host list style, card grouping, *Reduce motion*), Connect
//! (the terminal command template), Window & tray (closing, the tray menu),
//! ssh (binary, version, the agent, the override), Files and Help (the
//! tour, M10). *Save* and *Revert* sit next to the title.

use std::path::{Path, PathBuf};

use egui::{Align, Align2, Color32, FontId, Frame, Id, Layout, Margin, RichText, Sense, Ui, vec2};
use sshelf_core::agent_service::{self, AgentService};
use sshelf_core::core::{AppData, Command, SshInfo};
use sshelf_core::settings::{
    CardsGroupBy, CloseBehavior, DEFAULT_TERMINAL_COMMAND, HostListStyle, Settings, TrayMenu,
};
use sshelf_core::ssh::{AgentStatus, ToolSource};
use sshelf_core::storage::LoadStatus;
use sshelf_core::terminal;

use crate::theme::{Palette, radius};
use crate::tour::steps::Chapter;
use crate::widgets::button::{self, Button};
use crate::widgets::dropdown::Dropdown;
use crate::widgets::field::TextField;
use crate::widgets::switch::switch;
use crate::widgets::{nav, popup, text};
use crate::{fonts, icons};

/// Label column of a card's rows.
const LABEL: f32 = 150.0;
/// Space between the columns and between cards.
const GAP: f32 = 16.0;

#[derive(Default)]
pub struct SettingsView {
    /// Saved settings the draft started from.
    base: Option<Settings>,
    ssh_path: String,
    terminal_command: String,
    close_behavior: CloseBehavior,
    host_list_style: HostListStyle,
    cards_group_by: CardsGroupBy,
    reduce_motion: bool,
    tray_menu: TrayMenu,
    /// Toasts for the app ("Copied command").
    notices: Vec<String>,
    /// The tour asked for from Help: the chapter to start at.
    tour: Option<Chapter>,
}

impl SettingsView {
    /// The settings as edited (the tour's state is kept as saved).
    fn draft(&self) -> Settings {
        let path = self.ssh_path.trim();
        Settings {
            ssh_path: (!path.is_empty()).then(|| PathBuf::from(path)),
            terminal_command: self.terminal_command.clone(),
            close_behavior: self.close_behavior,
            host_list_style: self.host_list_style,
            cards_group_by: self.cards_group_by,
            reduce_motion: self.reduce_motion,
            tray_menu: self.tray_menu,
            tour: self.base.as_ref().and_then(|b| b.tour),
        }
    }

    /// Follows the saved settings: always when the draft has no edits, and
    /// when the draft is exactly what was saved (it was just saved from here).
    fn sync(&mut self, saved: &Settings) {
        let dirty = self.base.as_ref().is_some_and(|b| self.draft() != *b);
        if self.base.as_ref() != Some(saved) && (!dirty || self.draft() == *saved) {
            self.ssh_path = saved
                .ssh_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            self.terminal_command = saved.terminal_command.clone();
            self.close_behavior = saved.close_behavior;
            self.host_list_style = saved.host_list_style;
            self.cards_group_by = saved.cards_group_by;
            self.reduce_motion = saved.reduce_motion;
            self.tray_menu = saved.tray_menu;
            self.base = Some(saved.clone());
        }
    }

    /// Toasts to show, once.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// The tour to start (Help), once: the chapter it starts at.
    pub fn take_tour(&mut self) -> Option<Chapter> {
        self.tour.take()
    }

    /// `running`: the user's tunnels running now (the tour can't start
    /// while any does).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        data: Option<&AppData>,
        ssh: Option<&SshInfo>,
        ssh_config: Option<&Path>,
        running: usize,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some(data) = data else {
            ui.spinner();
            return commands;
        };
        self.sync(&data.settings);
        // 32 px from the rail and the window edge, as the other views.
        let ui = &mut ui
            .new_child(egui::UiBuilder::new().max_rect(ui.max_rect().shrink2(vec2(24.0, 0.0))));
        self.header(ui, data, &mut commands);
        ui.add_space(16.0);
        egui::ScrollArea::vertical()
            .id_salt("settings")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let column = ((ui.available_width() - GAP) / 2.0).max(320.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.spacing_mut().item_spacing.y = GAP;
                        self.appearance(ui);
                        self.connect(ui, ssh);
                        self.window_and_tray(ui, ssh);
                    });
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.spacing_mut().item_spacing.y = GAP;
                        self.ssh(ui, ssh, &mut commands);
                        files(ui, data, ssh_config);
                        self.help(ui, running);
                    });
                });
                ui.add_space(20.0);
            });
        commands
    }

    /// "Settings", then *Revert* and *Save* (or why the file can't be
    /// written).
    fn header(&mut self, ui: &mut Ui, data: &AppData, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        let dirty = self.base.as_ref().is_some_and(|b| self.draft() != *b);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Settings")
                    .font(fonts::semibold(20.0))
                    .color(p.text_strong),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let read_only = match &data.settings_status {
                    LoadStatus::ReadOnly(reason) => {
                        Some(format!("settings.toml is read-only: {reason}"))
                    }
                    _ => None,
                };
                let mut save = Button::new("Save").primary();
                if let Some(reason) = &read_only {
                    save = save.tip(reason);
                } else if !dirty {
                    save = save.tip("Nothing changed");
                }
                if ui.add_enabled(dirty && read_only.is_none(), save).clicked() {
                    commands.push(Command::SaveSettings(self.draft()));
                }
                if ui.add_enabled(dirty, Button::new("Revert")).clicked() {
                    self.base = None;
                    self.sync(&data.settings);
                }
                if let Some(reason) = &read_only {
                    ui.label(RichText::new(reason).size(12.0).color(p.yellow));
                } else if dirty {
                    ui.label(RichText::new("unsaved changes").size(12.0).color(p.yellow));
                }
            });
        });
    }

    fn appearance(&mut self, ui: &mut Ui) {
        let p = &Palette::ONE_DARK;
        card(ui, "Appearance", |ui| {
            row(ui, "Host list style", |ui| {
                let styles = [HostListStyle::List, HostListStyle::Cards];
                let at = styles.iter().position(|s| *s == self.host_list_style);
                if let Some(i) = nav::segmented(
                    ui,
                    Id::unique("settings_list_style"),
                    &["List", "Cards"],
                    at.unwrap_or(0),
                ) {
                    self.host_list_style = styles[i];
                }
            });
            row(ui, "Cards grouped by", |ui| {
                let groups = [CardsGroupBy::Tag, CardsGroupBy::File, CardsGroupBy::None];
                let at = groups.iter().position(|g| *g == self.cards_group_by);
                if let Some(i) = nav::segmented(
                    ui,
                    Id::unique("settings_group_by"),
                    &["Tag", "File", "None"],
                    at.unwrap_or(0),
                ) {
                    self.cards_group_by = groups[i];
                }
            });
            row(ui, "Reduce motion", |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                switch(ui, &mut self.reduce_motion, "Reduce motion");
                ui.label(
                    RichText::new("no animations; rings stop pulsing")
                        .size(12.0)
                        .color(p.dim),
                );
            });
        });
    }

    fn connect(&mut self, ui: &mut Ui, ssh: Option<&SshInfo>) {
        let p = &Palette::ONE_DARK;
        card(ui, "Connect", |ui| {
            let detected = ssh.and_then(|s| s.terminal.as_ref());
            let problem =
                terminal::plan(&self.terminal_command, Path::new("ssh"), "alias", detected).err();
            let label = ui.label(RichText::new("Terminal command").size(13.0).color(p.dim));
            ui.add(
                TextField::new(&mut self.terminal_command)
                    .mono()
                    .error(problem.is_some()),
            )
            .labelled_by(label.id);
            if let Some(problem) = problem {
                ui.label(RichText::new(problem.to_string()).size(12.0).color(p.red));
            }
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "{terminal_cmd}: the detected terminal (first word only) · {ssh}: the \
                         ssh binary · {alias}: the host. Words are split on spaces, \"…\" \
                         groups; no shell.",
                    )
                    .size(12.0)
                    .color(p.dim),
                )
                .wrap(),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let terminal = detected
                    .map(|t| t.describe())
                    .unwrap_or_else(|| "none found".to_string());
                ui.label(
                    RichText::new(format!("Detected terminal: {terminal}"))
                        .size(12.0)
                        .color(p.dim),
                );
                if self.terminal_command != DEFAULT_TERMINAL_COMMAND {
                    ui.label(RichText::new("·").size(12.0).color(p.dim));
                    if button::link(ui, "Reset to default", 12.0).clicked() {
                        self.terminal_command = DEFAULT_TERMINAL_COMMAND.to_string();
                    }
                }
            });
        });
    }

    fn window_and_tray(&mut self, ui: &mut Ui, ssh: Option<&SshInfo>) {
        let p = &Palette::ONE_DARK;
        let tray = ssh.map(|s| &s.tray);
        let tray_available = tray.is_some_and(|t| *t == Ok(true));
        card(ui, "Window & tray", |ui| {
            row(ui, "Closing the window", |ui| {
                let text = |b: CloseBehavior| match b {
                    CloseBehavior::Ask => "Ask while tunnels run",
                    CloseBehavior::StopAndQuit => "Stop the tunnels and quit",
                    CloseBehavior::MinimizeToTray => "Minimize to tray (always)",
                };
                let options = [
                    CloseBehavior::Ask,
                    CloseBehavior::StopAndQuit,
                    CloseBehavior::MinimizeToTray,
                ]
                .map(|b| (b, text(b)));
                if let Some(picked) = Dropdown::new(
                    "settings_close",
                    "Closing the window",
                    &options,
                    text(self.close_behavior),
                )
                .panel_width(220.0)
                .show(ui, self.close_behavior)
                {
                    self.close_behavior = picked;
                }
            });
            if self.close_behavior == CloseBehavior::MinimizeToTray && !tray_available {
                ui.label(
                    RichText::new("This desktop shows no tray right now, so closing asks instead.")
                        .size(12.0)
                        .color(p.yellow),
                );
            }
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Without running tunnels, closing quits — unless Minimize to tray is \
                         chosen. Quit in the tray menu always quits.",
                    )
                    .size(12.0)
                    .color(p.dim),
                )
                .wrap(),
            );
            row(ui, "Tray menu", |ui| {
                let menus = [TrayMenu::Sshelf, TrayMenu::System];
                let at = menus.iter().position(|m| *m == self.tray_menu);
                if let Some(i) = nav::segmented(
                    ui,
                    Id::unique("settings_tray_menu"),
                    &["sshelf menu", "System menu"],
                    at.unwrap_or(0),
                ) {
                    self.tray_menu = menus[i];
                }
            });
            row(ui, "Tray", |ui| {
                let (text, color) = match tray {
                    Some(Ok(true)) => ("available".to_string(), p.green),
                    Some(Ok(false)) => (
                        "no tray host on this desktop (e.g. GNOME needs an AppIndicator \
                         extension)"
                            .to_string(),
                        p.yellow,
                    ),
                    Some(Err(e)) => (format!("not available: {e}"), p.yellow),
                    None => ("—".to_string(), p.dim),
                };
                ui.add(egui::Label::new(RichText::new(text).size(13.0).color(color)).wrap());
            });
        });
    }

    fn ssh(&mut self, ui: &mut Ui, ssh: Option<&SshInfo>, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        let mut copied = false;
        card_with(
            ui,
            "ssh",
            |ui| {
                if ui
                    .add(Button::new("Refresh").icon(icons::REFRESH_CW).small())
                    .clicked()
                {
                    commands.push(Command::RefreshSsh);
                }
            },
            |ui| {
                let Some(ssh) = ssh else {
                    ui.spinner();
                    return;
                };
                row(ui, "Binary", |ui| match &ssh.tools {
                    Ok(tools) => {
                        let source = match tools.source {
                            ToolSource::Override => "from Settings",
                            ToolSource::BuiltIn => "built-in",
                            ToolSource::Path => "from PATH",
                            ToolSource::Sandbox => "tour",
                        };
                        path_label(ui, &tools.ssh.display(), Some(source));
                    }
                    Err(e) => {
                        ui.add(egui::Label::new(RichText::new(e).size(13.0).color(p.red)).wrap());
                    }
                });
                row(ui, "Version", |ui| {
                    match &ssh.version {
                        Some(Ok(version)) => ui.label(
                            RichText::new(version)
                                .font(FontId::monospace(12.0))
                                .color(p.text),
                        ),
                        Some(Err(e)) => ui.label(RichText::new(e).size(13.0).color(p.red)),
                        None => ui.label(RichText::new("—").color(p.dim)),
                    };
                });
                row(ui, "Agent", |ui| {
                    let (text, color) = agent_line(ssh);
                    ui.add(egui::Label::new(RichText::new(text).size(13.0).color(color)).wrap());
                });
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (rect, _) = ui.allocate_exact_size(vec2(LABEL, 32.0), Sense::hover());
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    let label = child.label(RichText::new("Override").size(13.0).color(p.dim));
                    ui.add(
                        TextField::new(&mut self.ssh_path)
                            .hint("path to an ssh binary (empty: detect)")
                            .mono(),
                    )
                    .labelled_by(label.id);
                });
                if let Some(Ok(service)) = &ssh.service
                    && !service.is_running()
                {
                    let (intro, command) = agent_service::ENABLE_HINT
                        .split_once('\n')
                        .unwrap_or(("", agent_service::ENABLE_HINT));
                    let command: Vec<&str> = command.lines().map(str::trim).collect();
                    Frame::new()
                        .fill(p.bg_dark)
                        .corner_radius(radius::CONTROL)
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "Keys can't load while ssh-agent is stopped, and sshelf \
                                         doesn't change it (that needs admin rights). {intro}"
                                    ))
                                    .size(12.0),
                                )
                                .wrap(),
                            );
                            for line in &command {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(*line)
                                            .font(FontId::monospace(12.0))
                                            .color(p.text_strong),
                                    )
                                    .wrap(),
                                );
                            }
                            if button::link(ui, "Copy command", 12.0).clicked() {
                                ui.ctx().copy_text(command.join("\n"));
                                copied = true;
                            }
                        });
                }
            },
        );
        if copied {
            self.notices.push("Copied command".into());
        }
    }

    /// Help: the tour again, or one chapter of it. Both wait while the
    /// user's tunnels run: the tour swaps in its own Core.
    fn help(&mut self, ui: &mut Ui, running: usize) {
        let p = &Palette::ONE_DARK;
        let blocked = match running {
            0 => None,
            1 => Some(
                "Stop the running tunnel first: the tour uses its own sample data.".to_string(),
            ),
            n => Some(format!(
                "Stop the {n} running tunnels first: the tour uses its own sample data."
            )),
        };
        card(ui, "Help", |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let mut again = Button::new("Take the tour again").icon(icons::COMPASS);
                if let Some(reason) = &blocked {
                    again = again.tip(reason);
                }
                if ui.add_enabled(blocked.is_none(), again).clicked() {
                    self.tour = Some(Chapter::Hosts);
                }
                let options = Chapter::PLAYABLE.map(|c| (c, c.title()));
                let picked = ui
                    .add_enabled_ui(blocked.is_none(), |ui| {
                        Dropdown::new(
                            "settings_tour_chapter",
                            "Start a chapter",
                            &options,
                            "Start a chapter…",
                        )
                        .panel_width(200.0)
                        .show(ui, Chapter::Done)
                    })
                    .inner;
                if let Some(chapter) = picked {
                    self.tour = Some(chapter);
                }
            });
            let note = blocked.unwrap_or_else(|| {
                "A walk through sshelf on sample hosts; your own config and tunnels are not \
                 touched."
                    .to_string()
            });
            ui.add(egui::Label::new(RichText::new(note).size(12.0).color(p.dim)).wrap());
        });
    }
}

/// The agent in one line: the Windows service when it isn't running, else
/// what `ssh-add -l` said.
fn agent_line(ssh: &SshInfo) -> (String, Color32) {
    let p = &Palette::ONE_DARK;
    match &ssh.service {
        Some(Ok(AgentService::NotInstalled)) => {
            return ("service not installed".into(), p.yellow);
        }
        Some(Ok(AgentService::Installed {
            running: false,
            start_type,
        })) => {
            return (format!("service stopped ({start_type:?})"), p.yellow);
        }
        Some(Err(e)) => return (e.clone(), p.red),
        _ => {}
    }
    match &ssh.agent {
        Some(AgentStatus::Keys(0)) => ("running · no keys loaded".into(), p.green),
        Some(AgentStatus::Keys(1)) => ("running · 1 key loaded".into(), p.green),
        Some(AgentStatus::Keys(n)) => (format!("running · {n} keys loaded"), p.green),
        Some(AgentStatus::Unreachable(e)) => (format!("not reachable: {e}"), p.yellow),
        Some(AgentStatus::Error(e)) => (e.clone(), p.red),
        None => ("—".into(), p.dim),
    }
}

fn files(ui: &mut Ui, data: &AppData, ssh_config: Option<&Path>) {
    let p = &Palette::ONE_DARK;
    card(ui, "Files", |ui| {
        if let Some(config) = ssh_config {
            row(ui, "ssh config", |ui| {
                path_label(ui, &config.display().to_string(), None)
                    .on_hover_text("Written only when you save an edit");
            });
        }
        row(ui, "App data", |ui| {
            path_label(ui, &data.app_dir.display().to_string(), None);
        });
        row(ui, "Backups", |ui| {
            path_label(
                ui,
                &data.app_dir.join("backups").display().to_string(),
                None,
            )
            .on_hover_text("A copy of an ssh config file before its first change in a session");
        });
        row(ui, "settings / hosts / history", |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let files = [
                ("settings.toml", &data.settings_status),
                ("hosts.toml", &data.hosts_status),
                ("history.toml", &data.history_status),
            ];
            for (index, (name, status)) in files.into_iter().enumerate() {
                if index > 0 {
                    ui.label(RichText::new("·").size(12.0).color(p.dim));
                }
                let (text, color, detail) = match status {
                    LoadStatus::Missing => ("not created", p.dim, None),
                    LoadStatus::Loaded => ("loaded", p.green, None),
                    LoadStatus::Migrated { from, backup } => (
                        "migrated",
                        p.green,
                        Some(format!(
                            "migrated from version {from}; original kept as {}",
                            backup.display()
                        )),
                    ),
                    LoadStatus::ReadOnly(reason) => {
                        ("read-only", p.yellow, Some(format!("read-only: {reason}")))
                    }
                };
                let response = ui.add(
                    egui::Label::new(RichText::new(text).size(12.0).color(color))
                        .sense(Sense::hover()),
                );
                popup::tip_below(response, |ui| {
                    ui.label(name);
                    if let Some(detail) = &detail {
                        ui.label(detail);
                    }
                });
            }
        });
    });
}

/// A settings card: raised, 8 px corners, its title, then `add`.
fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    card_with(ui, title, |_| {}, add);
}

/// A card with `actions` at the right end of its title row.
fn card_with(ui: &mut Ui, title: &str, actions: impl FnOnce(&mut Ui), add: impl FnOnce(&mut Ui)) {
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.horizontal(|ui| {
                ui.set_min_height(28.0);
                ui.label(
                    RichText::new(title)
                        .font(fonts::semibold(13.0))
                        .color(p.text_strong),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), actions);
            });
            add(ui);
        });
}

/// A card row: the dim label (150 px, painted: the control carries the
/// name), then `add` on one line.
fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    let p = &Palette::ONE_DARK;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.set_min_height(32.0);
        let (rect, _) = ui.allocate_exact_size(vec2(LABEL, 32.0), Sense::hover());
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            p.dim,
        );
        add(ui);
    });
}

/// A path in mono, cut in the middle to the width left (the full path in a
/// tooltip when cut), with an optional dim note after it.
fn path_label(ui: &mut Ui, path: &str, note: Option<&str>) -> egui::Response {
    let p = &Palette::ONE_DARK;
    let font = FontId::monospace(12.0);
    let note_width = note.map_or(0.0, |n| {
        ui.painter()
            .layout_no_wrap(n.to_string(), FontId::proportional(12.0), p.dim)
            .size()
            .x
            + 8.0
    });
    let room = (ui.available_width() - note_width).max(40.0);
    let (shown, cut) = text::elide(ui.painter(), path, &font, room);
    let response = ui
        .add(egui::Label::new(RichText::new(shown).font(font).color(p.text)).sense(Sense::hover()));
    if let Some(note) = note {
        ui.label(RichText::new(note).size(12.0).color(p.dim));
    }
    if cut {
        let full = path.to_string();
        popup::tip_below(response, move |ui| {
            ui.label(full);
        })
    } else {
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    use crate::test_util;

    fn data() -> AppData {
        AppData::new(PathBuf::from("app"))
    }

    fn harness(data: AppData) -> egui_kittest::Harness<'static, (SettingsView, Vec<Command>)> {
        test_util::harness()
            .with_size([1100.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (SettingsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&data), None, None, 0));
                },
                (SettingsView::default(), Vec::new()),
            )
    }

    fn help_harness(
        running: usize,
    ) -> egui_kittest::Harness<'static, (SettingsView, Vec<Chapter>)> {
        let data = data();
        test_util::harness()
            .with_size([1100.0, 800.0])
            .build_ui_state(
                move |ui, (view, asked): &mut (SettingsView, Vec<Chapter>)| {
                    view.show(ui, Some(&data), None, None, running);
                    asked.extend(view.take_tour());
                },
                (SettingsView::default(), Vec::new()),
            )
    }

    #[test]
    fn help_starts_the_tour_or_a_chapter() {
        let mut harness = help_harness(0);
        harness.step();
        harness.get_by_label("Take the tour again").click();
        harness.step();
        harness.get_by_label("Start a chapter").click();
        harness.step();
        harness.step();
        harness.get_by_label("Groups").click();
        harness.step();
        harness.step();
        assert_eq!(harness.state().1, [Chapter::Hosts, Chapter::Groups]);
    }

    #[test]
    fn help_waits_while_tunnels_run() {
        let mut harness = help_harness(2);
        harness.step();
        assert!(
            harness
                .get_by_label("Take the tour again")
                .accesskit_node()
                .is_disabled()
        );
        assert!(
            harness
                .get_by_label("Start a chapter")
                .accesskit_node()
                .is_disabled()
        );
        harness
            .get_by_label("Stop the 2 running tunnels first: the tour uses its own sample data.");
    }

    #[test]
    fn editing_the_override_enables_save() {
        let mut harness = harness(data());
        harness.step();
        harness.state_mut().0.ssh_path = "/opt/ssh".into();
        harness.step();
        harness.get_by_label("Save").click();
        harness.step();
        harness.step();
        let expected = Settings {
            ssh_path: Some(PathBuf::from("/opt/ssh")),
            ..Settings::default()
        };
        assert_eq!(harness.state().1, [Command::SaveSettings(expected)]);
    }

    #[test]
    fn appearance_and_tray_menu_are_edited_here() {
        let mut harness = harness(data());
        harness.step();
        harness.get_by_label("Cards").click();
        harness.step();
        harness.get_by_label("File").click();
        harness.step();
        harness.get_by_label("Reduce motion").click();
        harness.step();
        harness.get_by_label("System menu").click();
        harness.step();
        harness.step();
        harness.get_by_label("Save").click();
        harness.step();
        harness.step();
        let expected = Settings {
            host_list_style: HostListStyle::Cards,
            cards_group_by: CardsGroupBy::File,
            reduce_motion: true,
            tray_menu: TrayMenu::System,
            ..Settings::default()
        };
        assert_eq!(harness.state().1, [Command::SaveSettings(expected)]);
    }

    #[test]
    fn saving_keeps_the_other_saved_settings() {
        let mut data = data();
        data.settings.host_list_style = HostListStyle::Cards;
        data.settings.reduce_motion = true;
        data.settings.tray_menu = TrayMenu::System;
        let saved = data.settings.clone();
        let mut harness = harness(data);
        harness.step();
        harness.state_mut().0.terminal_command = "{ssh} {alias}".into();
        harness.step();
        harness.get_by_label("Reset to default");
        harness.get_by_label("Save").click();
        harness.step();
        harness.step();
        let expected = Settings {
            terminal_command: "{ssh} {alias}".into(),
            ..saved
        };
        assert_eq!(harness.state().1, [Command::SaveSettings(expected)]);
    }

    #[test]
    fn saved_settings_clear_the_unsaved_marker() {
        let state = (SettingsView::default(), data());
        let mut harness = test_util::harness()
            .with_size([1100.0, 800.0])
            .build_ui_state(
                |ui, (view, data): &mut (SettingsView, AppData)| {
                    view.show(ui, Some(data), None, None, 0);
                },
                state,
            );
        harness.step();
        harness.state_mut().0.close_behavior = CloseBehavior::StopAndQuit;
        harness.step();
        harness.get_by_label("unsaved changes");
        // The core saved it and sent the new data back.
        harness.state_mut().1.settings.close_behavior = CloseBehavior::StopAndQuit;
        harness.step();
        harness.step();
        assert!(harness.query_by_label("unsaved changes").is_none());
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
    }
}
