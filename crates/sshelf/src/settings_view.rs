//! Settings (SPEC.md §8, §10): ssh binary, version and agent status (with the
//! Windows service check), the terminal command template, and where sshelf
//! keeps its files.

use std::path::{Path, PathBuf};

use egui::RichText;
use sshelf_core::agent_service::{self, AgentService};
use sshelf_core::core::{AppData, Command, SshInfo};
use sshelf_core::settings::{CloseBehavior, DEFAULT_TERMINAL_COMMAND, Settings};
use sshelf_core::ssh::{AgentStatus, ToolSource};
use sshelf_core::storage::LoadStatus;
use sshelf_core::terminal;

use crate::icons;
use crate::theme::Palette;

#[derive(Default)]
pub struct SettingsView {
    /// Saved settings the draft started from.
    base: Option<Settings>,
    ssh_path: String,
    terminal_command: String,
    close_behavior: CloseBehavior,
}

impl SettingsView {
    /// The edited fields over the saved settings (fields this view doesn't
    /// edit keep their saved values).
    fn draft(&self) -> Settings {
        let path = self.ssh_path.trim();
        Settings {
            ssh_path: (!path.is_empty()).then(|| PathBuf::from(path)),
            terminal_command: self.terminal_command.clone(),
            close_behavior: self.close_behavior,
            ..self.base.clone().unwrap_or_default()
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
            self.base = Some(saved.clone());
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        data: Option<&AppData>,
        ssh: Option<&SshInfo>,
        ssh_config: Option<&Path>,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some(data) = data else {
            ui.spinner();
            return commands;
        };
        self.sync(&data.settings);
        egui::ScrollArea::vertical()
            .id_salt("settings")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                ssh_section(ui, ssh, &mut commands);
                ui.horizontal(|ui| {
                    let label = ui.label("Override");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ssh_path)
                            .hint_text("path to an ssh binary (empty: detect)")
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                });
                ui.separator();
                self.terminal_section(ui, ssh);
                ui.separator();
                self.close_section(ui, ssh);
                ui.separator();
                self.save_buttons(ui, data, &mut commands);
                ui.separator();
                files_section(ui, data, ssh_config);
            });
        commands
    }

    fn terminal_section(&mut self, ui: &mut egui::Ui, ssh: Option<&SshInfo>) {
        let p = &Palette::ONE_DARK;
        ui.label(RichText::new("Connect").strong());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.terminal_command)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(ui.available_width() - 70.0),
            );
            if ui.button("Default").clicked() {
                self.terminal_command = DEFAULT_TERMINAL_COMMAND.to_string();
            }
        });
        let detected = ssh.and_then(|s| s.terminal.as_ref());
        if let Err(e) = terminal::plan(&self.terminal_command, Path::new("ssh"), "alias", detected)
        {
            ui.colored_label(p.red, e.to_string());
        }
        ui.label(
            RichText::new(
                "{terminal_cmd}: the detected terminal (first word only) · {ssh}: the ssh \
                 binary · {alias}: the host. Words are split on spaces, \"…\" groups; no shell.",
            )
            .small()
            .color(p.dim),
        );
        let terminal = detected
            .map(|t| t.describe())
            .unwrap_or_else(|| "none found".to_string());
        ui.label(RichText::new(format!("Detected terminal: {terminal}")).color(p.dim));
    }

    fn close_section(&mut self, ui: &mut egui::Ui, ssh: Option<&SshInfo>) {
        let p = &Palette::ONE_DARK;
        ui.label(RichText::new("Closing").strong());
        ui.horizontal(|ui| {
            let label = ui.label("Closing the window");
            let text = |b: CloseBehavior| match b {
                CloseBehavior::Ask => "Ask while tunnels run",
                CloseBehavior::StopAndQuit => "Stop the tunnels and quit",
                CloseBehavior::MinimizeToTray => "Minimize to tray (always)",
            };
            egui::ComboBox::from_id_salt("close_behavior")
                .selected_text(text(self.close_behavior))
                .show_ui(ui, |ui| {
                    for behavior in [
                        CloseBehavior::Ask,
                        CloseBehavior::StopAndQuit,
                        CloseBehavior::MinimizeToTray,
                    ] {
                        ui.selectable_value(&mut self.close_behavior, behavior, text(behavior));
                    }
                })
                .response
                .labelled_by(label.id);
            if self.close_behavior != CloseBehavior::Ask && ui.button("Reset").clicked() {
                self.close_behavior = CloseBehavior::Ask;
            }
        });
        let tray = ssh.is_some_and(|s| s.tray == Ok(true));
        if self.close_behavior == CloseBehavior::MinimizeToTray && !tray {
            ui.colored_label(
                p.yellow,
                "This desktop shows no tray right now, so closing asks instead.",
            );
        }
        ui.label(
            RichText::new(
                "Without running tunnels, closing quits — unless Minimize to tray is chosen. \
                 Quit in the tray menu always quits.",
            )
            .small()
            .color(p.dim),
        );
    }

    fn save_buttons(&mut self, ui: &mut egui::Ui, data: &AppData, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        if let LoadStatus::ReadOnly(reason) = &data.settings_status {
            ui.colored_label(p.yellow, format!("settings.toml is read-only: {reason}"));
            return;
        }
        let dirty = self.base.as_ref().is_some_and(|b| self.draft() != *b);
        ui.horizontal(|ui| {
            if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                commands.push(Command::SaveSettings(self.draft()));
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                self.base = None;
                self.sync(&data.settings);
            }
            if dirty {
                ui.label(RichText::new("unsaved changes").small().color(p.yellow));
            }
        });
    }
}

fn ssh_section(ui: &mut egui::Ui, ssh: Option<&SshInfo>, commands: &mut Vec<Command>) {
    let p = &Palette::ONE_DARK;
    ui.horizontal(|ui| {
        ui.label(RichText::new("ssh").strong());
        if ui
            .small_button((icons::atom(icons::REFRESH_CW), "Refresh"))
            .clicked()
        {
            commands.push(Command::RefreshSsh);
        }
    });
    let Some(ssh) = ssh else {
        ui.spinner();
        return;
    };
    egui::Grid::new("ssh_info")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("Binary");
            match &ssh.tools {
                Ok(tools) => {
                    let source = match tools.source {
                        ToolSource::Override => "from Settings",
                        ToolSource::BuiltIn => "built-in Windows OpenSSH",
                        ToolSource::Path => "from PATH",
                    };
                    ui.label(format!("{} ({source})", tools.ssh.display()));
                }
                Err(e) => {
                    ui.colored_label(p.red, e);
                }
            }
            ui.end_row();

            ui.label("Version");
            match &ssh.version {
                Some(Ok(version)) => ui.monospace(version),
                Some(Err(e)) => ui.colored_label(p.red, e),
                None => ui.label("—"),
            };
            ui.end_row();

            ui.label("Agent");
            match &ssh.agent {
                Some(AgentStatus::Keys(n)) => {
                    ui.colored_label(p.green, format!("reachable, {n} key(s) loaded"))
                }
                Some(AgentStatus::Unreachable(e)) => {
                    ui.colored_label(p.yellow, format!("not reachable: {e}"))
                }
                Some(AgentStatus::Error(e)) => ui.colored_label(p.red, e),
                None => ui.label("—"),
            };
            ui.end_row();

            ui.label("Tray");
            match &ssh.tray {
                Ok(true) => ui.colored_label(p.green, "available"),
                Ok(false) => ui.colored_label(
                    p.yellow,
                    "no tray host on this desktop (e.g. GNOME needs an AppIndicator extension)",
                ),
                Err(e) => ui.colored_label(p.yellow, format!("not available: {e}")),
            };
            ui.end_row();

            if let Some(service) = &ssh.service {
                ui.label("Agent service");
                match service {
                    Ok(AgentService::NotInstalled) => {
                        ui.colored_label(p.yellow, "not installed");
                    }
                    Ok(AgentService::Installed {
                        running,
                        start_type,
                    }) => {
                        let text = format!(
                            "{} (start type: {start_type:?})",
                            if *running { "running" } else { "stopped" }
                        );
                        ui.colored_label(if *running { p.green } else { p.yellow }, text);
                    }
                    Err(e) => {
                        ui.colored_label(p.red, e);
                    }
                }
                ui.end_row();
            }
        });
    if let Some(Ok(service)) = &ssh.service
        && !service.is_running()
    {
        ui.label(
            "Keys cannot be loaded while the ssh-agent service is not running. \
             sshelf does not change it (that needs admin rights).",
        );
        ui.code(agent_service::ENABLE_HINT);
    }
}

fn files_section(ui: &mut egui::Ui, data: &AppData, ssh_config: Option<&Path>) {
    let p = &Palette::ONE_DARK;
    ui.label(RichText::new("Files").strong());
    egui::Grid::new("files")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            if let Some(config) = ssh_config {
                ui.label("ssh config");
                ui.label(format!(
                    "{} (written only when you save an edit)",
                    config.display()
                ));
                ui.end_row();
            }
            ui.label("App data");
            ui.label(data.app_dir.display().to_string());
            ui.end_row();
            ui.label("Config backups");
            ui.label(format!(
                "{} (a copy before the first change in a session)",
                data.app_dir.join("backups").display()
            ));
            ui.end_row();
            for (name, status) in [
                ("settings.toml", &data.settings_status),
                ("hosts.toml", &data.hosts_status),
                ("history.toml", &data.history_status),
            ] {
                ui.label(name);
                match status {
                    LoadStatus::Missing => ui.label(RichText::new("not created yet").color(p.dim)),
                    LoadStatus::Loaded => ui.label("loaded"),
                    LoadStatus::Migrated { from, backup } => ui.label(format!(
                        "migrated from version {from}; original kept as {}",
                        backup.display()
                    )),
                    LoadStatus::ReadOnly(reason) => {
                        ui.colored_label(p.yellow, format!("read-only: {reason}"))
                    }
                };
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshelf_core::settings::{HostListStyle, TrayMenu};

    use crate::test_util;

    fn data() -> AppData {
        AppData::new(PathBuf::from("app"))
    }

    #[test]
    fn editing_the_override_enables_save() {
        let data = data();
        let mut harness = test_util::harness()
            .with_size([900.0, 700.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (SettingsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&data), None, None));
                },
                (SettingsView::default(), Vec::new()),
            );
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
    fn saving_keeps_the_settings_this_view_does_not_edit() {
        let mut data = data();
        data.settings.host_list_style = HostListStyle::Cards;
        data.settings.reduce_motion = true;
        data.settings.tray_menu = TrayMenu::System;
        let saved = data.settings.clone();
        let mut harness = test_util::harness()
            .with_size([900.0, 700.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (SettingsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&data), None, None));
                },
                (SettingsView::default(), Vec::new()),
            );
        harness.step();
        harness.state_mut().0.terminal_command = "{ssh} {alias}".into();
        harness.step();
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
            .with_size([900.0, 700.0])
            .build_ui_state(
                |ui, (view, data): &mut (SettingsView, AppData)| {
                    view.show(ui, Some(data), None, None);
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
