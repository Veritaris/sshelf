//! Orphan popover (SPEC.md §6): relink or delete metadata that no longer
//! links to a host. Nothing is deleted without an explicit confirmation.

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

impl OrphansWindow {
    pub fn show(&mut self, ctx: &egui::Context, data: &AppData) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let mut open = self.open;
        egui::Window::new("Orphaned data")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "These hosts.toml entries no longer link to a host. \
                         They are never deleted automatically.",
                    )
                    .color(p.dim),
                );
                if data.orphans.is_empty() {
                    ui.label("Nothing is orphaned.");
                }
                for orphan in &data.orphans {
                    ui.separator();
                    ui.label(orphan.describe());
                    self.relink(ui, &orphan.key, &data.relink_targets, &mut commands);
                    self.delete(ui, &orphan.key, &mut commands);
                }
                if !data.orphans.is_empty() && data.relink_targets.is_empty() {
                    ui.label(
                        RichText::new("Every host already has metadata; nothing to relink to.")
                            .small()
                            .color(p.dim),
                    );
                }
            });
        self.open = open && !data.orphans.is_empty();
        commands
    }

    fn relink(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        targets: &[String],
        commands: &mut Vec<Command>,
    ) {
        if targets.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            let chosen = self.choice.get(key).cloned();
            let label = ui.label("Relink to");
            egui::ComboBox::from_id_salt(("relink", key))
                .selected_text(chosen.as_deref().unwrap_or("choose a host"))
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
                commands.push(Command::RelinkOrphan {
                    key: key.to_string(),
                    to: to.clone(),
                });
                self.choice.remove(key);
            }
        });
    }

    fn delete(&mut self, ui: &mut egui::Ui, key: &str, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        if self.confirm_delete.as_deref() == Some(key) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Delete the metadata for `{key}`?")).color(p.red));
                if ui.button("Delete").clicked() {
                    commands.push(Command::DeleteOrphan {
                        key: key.to_string(),
                    });
                    self.confirm_delete = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = None;
                }
            });
        } else if ui.button("Delete…").clicked() {
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
    use sshelf_core::orphans::{Orphan, OrphanReason};
    use sshelf_core::storage::LoadStatus;

    use super::*;

    fn data() -> AppData {
        let mut hosts = HostsFile::default();
        hosts.hosts.insert("gone".into(), HostMeta::default());
        AppData {
            hosts,
            hosts_status: LoadStatus::Loaded,
            orphans: vec![Orphan {
                key: "gone".into(),
                reason: OrphanReason::MissingAlias,
            }],
            relink_targets: vec!["db".into()],
            ..AppData::new(PathBuf::from("app"))
        }
    }

    fn harness() -> Harness<'static, (OrphansWindow, Vec<Command>)> {
        let data = data();
        let window = OrphansWindow {
            open: true,
            ..OrphansWindow::default()
        };
        Harness::builder().with_size([800.0, 600.0]).build_ui_state(
            move |ui, (window, commands): &mut (OrphansWindow, Vec<Command>)| {
                commands.extend(window.show(ui.ctx(), &data));
            },
            (window, Vec::new()),
        )
    }

    #[test]
    fn delete_needs_confirmation() {
        let mut harness = harness();
        harness.step();
        harness.get_by_label("Delete…").click();
        harness.step();
        harness.step();
        assert!(harness.state().1.is_empty());
        harness.get_by_label("Delete").click();
        harness.step();
        assert_eq!(
            harness.state().1,
            [Command::DeleteOrphan { key: "gone".into() }]
        );
    }

    #[test]
    fn relink_needs_a_target() {
        let mut harness = harness();
        harness.step();
        harness.get_by_label("Relink").click();
        harness.step();
        assert!(harness.state().1.is_empty());
        harness
            .state_mut()
            .0
            .choice
            .insert("gone".into(), "db".into());
        harness.step();
        harness.get_by_label("Relink").click();
        harness.step();
        assert_eq!(
            harness.state().1,
            [Command::RelinkOrphan {
                key: "gone".into(),
                to: "db".into()
            }]
        );
    }
}
