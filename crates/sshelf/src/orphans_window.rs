//! Orphan popover (SPEC.md §6, §10): opened from the rail's orphan
//! indicator, it relinks or deletes app data that no longer links to what it
//! refers to — host metadata, tunnels whose host is gone, and group entries
//! naming missing tunnels. Nothing is deleted without an explicit
//! confirmation. A click outside or `Esc` closes it.

use std::collections::HashMap;

use egui::{Align, Area, Frame, Id, Key, Layout, Margin, Order, RichText, Stroke, Ui, pos2};
use sshelf_core::core::{AppData, Command};

use crate::fonts;
use crate::icons;
use crate::theme::{Palette, radius};
use crate::widgets::button::Button;
use crate::widgets::dropdown::{Choice, Dropdown};

/// Width of the popover.
const WIDTH: f32 = 460.0;

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
        // Beside the rail, under the drag strip; it grows downward.
        let indicator = ctx
            .read_response(Id::unique("rail_orphans"))
            .map(|r| r.rect);
        let left = ctx.content_rect().left() + crate::sidebar::WIDTH + 8.0;
        let top = ctx.content_rect().top() + 52.0;
        // Room for the list: the window less the strip, the header and the
        // status bar.
        let list_height = (ctx.content_rect().bottom() - top - 180.0).max(160.0);
        let mut close = false;
        let area = Area::new(Id::unique("orphans_popover"))
            .order(Order::Foreground)
            .fixed_pos(pos2(left, top))
            // Taller than egui's default area (400 px) when the list is.
            .default_height(list_height + 180.0)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(p.bg_dark)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::CONTROL)
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(WIDTH - 32.0);
                        ui.spacing_mut().item_spacing.y = 10.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Orphaned data")
                                    .font(fonts::semibold(13.0))
                                    .color(p.text_strong),
                            );
                            ui.label(
                                RichText::new(data.orphan_count().to_string())
                                    .size(13.0)
                                    .color(p.dim),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                close = ui
                                    .add(Button::icon_only(icons::X, "Close").ghost().small())
                                    .clicked();
                            });
                        });
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "This app data no longer links to what it refers to. It is \
                                     never deleted automatically.",
                                )
                                .size(12.0)
                                .color(p.dim),
                            )
                            .wrap(),
                        );
                        if data.orphan_count() == 0 {
                            ui.label(RichText::new("Nothing is orphaned.").size(13.0));
                        }
                        egui::ScrollArea::vertical()
                            .id_salt("orphans")
                            .max_height(list_height)
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 8.0;
                                self.entries(ui, data, hosts, &mut commands);
                            });
                    });
            });
        let pressed_at = ctx.input(|i| {
            i.pointer
                .any_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        let outside = pressed_at.is_some_and(|pos| {
            !area.response.rect.contains(pos)
                && !indicator.is_some_and(|r| r.contains(pos))
                // A dropdown's panel (its own layer) belongs to the popover.
                && ctx
                    .layer_id_at(pos)
                    .is_none_or(|l| l.order < Order::Foreground)
        });
        // An open dropdown takes Esc first.
        if close || outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.open = false;
            self.confirm_delete = None;
        }
        if data.orphan_count() == 0 {
            self.open = false;
        }
        commands
    }

    fn entries(
        &mut self,
        ui: &mut Ui,
        data: &AppData,
        hosts: &[String],
        commands: &mut Vec<Command>,
    ) {
        for orphan in &data.orphans {
            let key = format!("meta:{}", orphan.key);
            entry(ui, &orphan.describe(), |ui| {
                self.relink(ui, &key, &data.relink_targets, commands, |to| {
                    Command::RelinkOrphan {
                        key: orphan.key.clone(),
                        to,
                    }
                });
                self.delete(ui, &key, "Delete…", commands, || Command::DeleteOrphan {
                    key: orphan.key.clone(),
                });
            });
        }
        for orphan in &data.tunnel_orphans {
            let key = format!("tunnel:{}", orphan.tunnel_id);
            entry(ui, &orphan.describe(), |ui| {
                self.relink(ui, &key, hosts, commands, |to| Command::RelinkTunnel {
                    tunnel_id: orphan.tunnel_id.clone(),
                    to,
                });
                self.delete(ui, &key, "Delete tunnel…", commands, || {
                    Command::DeleteTunnel(orphan.tunnel_id.clone())
                });
            });
        }
        for orphan in &data.group_orphans {
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
            entry(ui, &orphan.describe(), |ui| {
                self.relink(ui, &key, &targets, commands, |to| {
                    Command::RelinkGroupMember {
                        group_id: orphan.group_id.clone(),
                        from: orphan.tunnel_id.clone(),
                        to,
                    }
                });
                self.delete(ui, &key, "Remove from group…", commands, || {
                    Command::RemoveGroupMember {
                        group_id: orphan.group_id.clone(),
                        tunnel_id: orphan.tunnel_id.clone(),
                    }
                });
            });
        }
    }

    /// "Relink to" (a dropdown of `targets`) and *Relink*.
    fn relink(
        &mut self,
        ui: &mut Ui,
        key: &str,
        targets: &[String],
        commands: &mut Vec<Command>,
        command: impl FnOnce(String) -> Command,
    ) {
        let p = &Palette::ONE_DARK;
        if targets.is_empty() {
            ui.label(
                RichText::new("Nothing to relink to.")
                    .size(12.0)
                    .color(p.dim),
            );
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let chosen = self.choice.get(key).cloned();
            let selected = chosen
                .as_ref()
                .and_then(|c| targets.iter().position(|t| t == c))
                .unwrap_or(usize::MAX);
            let choices: Vec<Choice<usize>> = targets
                .iter()
                .enumerate()
                .map(|(i, t)| Choice::new(i, t))
                .collect();
            let mut dropdown = Dropdown::with_choices(
                ("relink", key),
                "Relink to",
                choices,
                chosen.clone().unwrap_or_else(|| "relink to…".into()),
            )
            .field(240.0)
            .mono();
            if targets.len() > 8 {
                dropdown = dropdown.searchable("targets");
            }
            if let Some(i) = dropdown.show(ui, selected) {
                self.choice.insert(key.to_string(), targets[i].clone());
            }
            let valid = chosen.as_ref().filter(|c| targets.contains(c));
            let mut relink = Button::new("Relink").small();
            if valid.is_none() {
                relink = relink.tip("Choose what to relink to first");
            }
            if ui.add_enabled(valid.is_some(), relink).clicked()
                && let Some(to) = valid
            {
                commands.push(command(to.clone()));
                self.choice.remove(key);
            }
        });
    }

    /// The delete button, then an inline confirmation: *Cancel* (grey,
    /// focused) and *Delete* (Dark Red).
    fn delete(
        &mut self,
        ui: &mut Ui,
        key: &str,
        label: &str,
        commands: &mut Vec<Command>,
        command: impl FnOnce() -> Command,
    ) {
        let p = &Palette::ONE_DARK;
        if self.confirm_delete.as_deref() == Some(key) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(
                    RichText::new("This cannot be undone.")
                        .size(12.0)
                        .color(p.red),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new("Delete").danger().small()).clicked() {
                        commands.push(command());
                        self.confirm_delete = None;
                    }
                    let cancel = ui.add(Button::new("Cancel").small());
                    if ui.memory(|m| m.focused().is_none()) {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        self.confirm_delete = None;
                    }
                });
            });
        } else if ui.add(Button::new(label).ghost().small()).clicked() {
            self.confirm_delete = Some(key.to_string());
        }
    }
}

/// One orphan on a raised card: what it is, then `actions`.
fn entry(ui: &mut Ui, description: &str, actions: impl FnOnce(&mut Ui)) {
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add(egui::Label::new(RichText::new(description).size(13.0)).wrap());
            actions(ui);
        });
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
    use crate::test_util;

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
            auto_reconnect: false,
        });
        data.tunnels.upsert_group(Group {
            id: "g-1".into(),
            name: "Work".into(),
            tunnels: vec!["t-missing".into()],
            start_on_launch: false,
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
        test_util::harness()
            .with_size([800.0, 800.0])
            .build_ui_state(
                move |ui, (window, commands): &mut (OrphansWindow, Vec<Command>)| {
                    if window.open {
                        commands.extend(window.show(ui.ctx(), &data, &hosts));
                    }
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
        harness.get_by_label("This cannot be undone.");
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
        // The metadata orphan's button (first) stays disabled.
        for index in 0..3 {
            harness
                .get_all_by_label("Relink")
                .nth(index)
                .unwrap()
                .click();
            harness.step();
        }
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

    #[test]
    fn escape_closes_it() {
        let mut harness = harness();
        harness.step();
        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(!harness.state().0.open);
    }
}
