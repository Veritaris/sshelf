//! Orphan popover (SPEC.md §6): relink or delete app data that no longer
//! links to what it refers to — host metadata, tunnels whose host is gone,
//! and group entries naming missing tunnels. Nothing is deleted without an
//! explicit confirmation.

use std::collections::HashMap;

use egui::RichText;
use sshelf_core::core::{AppData, Command};

use crate::theme::Palette;

#[derive(Default)]
pub struct OrphansWindow {
    pub open: bool,
    /// Orphan key → chosen relink target.
    choice: HashMap<String, String>,
    confirm_delete: Option<String>,
}

/// One-line descriptions of every orphan, for the sidebar tooltip.
pub fn descriptions(data: &AppData) -> Vec<String> {
    data.orphans
        .iter()
        .map(|o| o.describe())
        .chain(data.tunnel_orphans.iter().map(|o| o.describe()))
        .chain(data.group_orphans.iter().map(|o| o.describe()))
        .collect()
}

impl OrphansWindow {
    /// `hosts`: primary aliases of every host (tunnels may relink to any).
    pub fn show(&mut self, ctx: &egui::Context, data: &AppData, hosts: &[String]) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let mut open = self.open;
        egui::Window::new("Orphaned data")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "This app data no longer links to what it refers to. \
                         It is never deleted automatically.",
                    )
                    .color(p.dim),
                );
                if data.orphan_count() == 0 {
                    ui.label("Nothing is orphaned.");
                }
                for orphan in &data.orphans {
                    ui.separator();
                    ui.label(orphan.describe());
                    let key = format!("meta:{}", orphan.key);
                    self.relink(ui, &key, &data.relink_targets, &mut commands, |to| {
                        Command::RelinkOrphan {
                            key: orphan.key.clone(),
                            to,
                        }
                    });
                    self.delete(ui, &key, "Delete…", &mut commands, || {
                        Command::DeleteOrphan {
                            key: orphan.key.clone(),
                        }
                    });
                }
                for orphan in &data.tunnel_orphans {
                    ui.separator();
                    ui.label(orphan.describe());
                    let key = format!("tunnel:{}", orphan.tunnel_id);
                    self.relink(ui, &key, hosts, &mut commands, |to| Command::RelinkTunnel {
                        tunnel_id: orphan.tunnel_id.clone(),
                        to,
                    });
                    self.delete(ui, &key, "Delete tunnel…", &mut commands, || {
                        Command::DeleteTunnel(orphan.tunnel_id.clone())
                    });
                }
                for orphan in &data.group_orphans {
                    ui.separator();
                    ui.label(orphan.describe());
                    let key = format!("group:{}:{}", orphan.group_id, orphan.tunnel_id);
                    let members = data
                        .tunnels
                        .group(&orphan.group_id)
                        .map(|g| g.tunnels.clone())
                        .unwrap_or_default();
                    let targets: Vec<String> = data
                        .tunnels
                        .tunnels
                        .iter()
                        .filter(|t| !members.contains(&t.id))
                        .map(|t| t.id.clone())
                        .collect();
                    self.relink(ui, &key, &targets, &mut commands, |to| {
                        Command::RelinkGroupMember {
                            group_id: orphan.group_id.clone(),
                            from: orphan.tunnel_id.clone(),
                            to,
                        }
                    });
                    self.delete(ui, &key, "Remove from group…", &mut commands, || {
                        Command::RemoveGroupMember {
                            group_id: orphan.group_id.clone(),
                            tunnel_id: orphan.tunnel_id.clone(),
                        }
                    });
                }
            });
        self.open = open && data.orphan_count() > 0;
        commands
    }

    fn relink(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        targets: &[String],
        commands: &mut Vec<Command>,
        command: impl FnOnce(String) -> Command,
    ) {
        if targets.is_empty() {
            ui.label(
                RichText::new("Nothing to relink to.")
                    .small()
                    .color(Palette::ONE_DARK.dim),
            );
            return;
        }
        ui.horizontal(|ui| {
            let chosen = self.choice.get(key).cloned();
            let label = ui.label("Relink to");
            egui::ComboBox::from_id_salt(("relink", key))
                .selected_text(chosen.as_deref().unwrap_or("choose"))
                .show_ui(ui, |ui| {
                    for target in targets {
                        if ui
                            .selectable_label(chosen.as_ref() == Some(target), target)
                            .clicked()
                        {
                            self.choice.insert(key.to_string(), target.clone());
                        }
                    }
                })
                .response
                .labelled_by(label.id);
            let valid = chosen.as_ref().filter(|c| targets.contains(c));
            if ui
                .add_enabled(valid.is_some(), egui::Button::new("Relink"))
                .clicked()
                && let Some(to) = valid
            {
                commands.push(command(to.clone()));
                self.choice.remove(key);
            }
        });
    }

    fn delete(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        label: &str,
        commands: &mut Vec<Command>,
        command: impl FnOnce() -> Command,
    ) {
        let p = &Palette::ONE_DARK;
        if self.confirm_delete.as_deref() == Some(key) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("This cannot be undone.").color(p.red));
                if ui.button("Delete").clicked() {
                    commands.push(command());
                    self.confirm_delete = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = None;
                }
            });
        } else if ui.button(label).clicked() {
            self.confirm_delete = Some(key.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sshelf_core::metadata::{HostMeta, HostsFile};
    use sshelf_core::orphans::{GroupOrphan, Orphan, OrphanReason, TunnelOrphan};
    use sshelf_core::storage::LoadStatus;
    use sshelf_core::tunnels::{Group, Tunnel};

    use super::*;

    fn data() -> AppData {
        let mut hosts = HostsFile::default();
        hosts.hosts.insert("gone".into(), HostMeta::default());
        let mut data = AppData {
            hosts,
            hosts_status: LoadStatus::Loaded,
            orphans: vec![Orphan {
                key: "gone".into(),
                reason: OrphanReason::MissingAlias,
            }],
            relink_targets: vec!["db".into()],
            ..AppData::new(PathBuf::from("app"))
        };
        data.tunnels.upsert_tunnel(Tunnel {
            id: "t-1".into(),
            name: "Old".into(),
            host: "old-host".into(),
            forwards: Vec::new(),
        });
        data.tunnels.upsert_group(Group {
            id: "g-1".into(),
            name: "Work".into(),
            tunnels: vec!["t-missing".into()],
        });
        data.tunnel_orphans = vec![TunnelOrphan {
            tunnel_id: "t-1".into(),
            name: "Old".into(),
            host: "old-host".into(),
        }];
        data.group_orphans = vec![GroupOrphan {
            group_id: "g-1".into(),
            group_name: "Work".into(),
            tunnel_id: "t-missing".into(),
        }];
        data
    }

    fn harness() -> Harness<'static, (OrphansWindow, Vec<Command>)> {
        let data = data();
        let hosts = vec!["db".to_string(), "web".to_string()];
        let window = OrphansWindow {
            open: true,
            ..OrphansWindow::default()
        };
        Harness::builder().with_size([800.0, 800.0]).build_ui_state(
            move |ui, (window, commands): &mut (OrphansWindow, Vec<Command>)| {
                commands.extend(window.show(ui.ctx(), &data, &hosts));
            },
            (window, Vec::new()),
        )
    }

    #[test]
    fn lists_every_kind_of_orphan() {
        let mut harness = harness();
        harness.step();
        harness.get_by_label("metadata for `gone`: no such host");
        harness.get_by_label("tunnel \"Old\": no host `old-host`");
        harness.get_by_label("group \"Work\": no tunnel `t-missing`");
        assert_eq!(descriptions(&data()).len(), 3);
    }

    #[test]
    fn delete_needs_confirmation() {
        let mut harness = harness();
        harness.step();
        harness.get_by_label("Delete tunnel…").click();
        harness.step();
        harness.step();
        assert!(harness.state().1.is_empty());
        harness.get_by_label("Delete").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::DeleteTunnel("t-1".into())]);
    }

    #[test]
    fn relink_needs_a_target() {
        let mut harness = harness();
        harness.step();
        harness
            .state_mut()
            .0
            .choice
            .insert("tunnel:t-1".into(), "web".into());
        harness
            .state_mut()
            .0
            .choice
            .insert("group:g-1:t-missing".into(), "t-1".into());
        harness.step();
        for button in harness.get_all_by_label("Relink") {
            button.click();
        }
        harness.step();
        let commands = &harness.state().1;
        assert!(commands.contains(&Command::RelinkTunnel {
            tunnel_id: "t-1".into(),
            to: "web".into()
        }));
        assert!(commands.contains(&Command::RelinkGroupMember {
            group_id: "g-1".into(),
            from: "t-missing".into(),
            to: "t-1".into()
        }));
        assert_eq!(
            commands.len(),
            2,
            "the metadata orphan had no target chosen"
        );
    }
}
