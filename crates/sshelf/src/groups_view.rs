//! Groups view (SPEC.md §7, §10): one toggle per group, its aggregate state
//! (running / degraded / stopped) with "Retry failed", expandable per-tunnel
//! states, and the group editor.

use egui::RichText;
use sshelf_core::core::{AppData, Command};
use sshelf_core::hints;
use sshelf_core::supervisor::{GroupState, Owner, Snapshot, TunnelState};
use sshelf_core::tunnels::Group;

use crate::theme::Palette;
use crate::tunnels_view::{owner_names, reconnect_text, state_label, status_dot};

#[derive(Default)]
pub struct GroupsView {
    editor: Option<Group>,
    confirm_delete: Option<String>,
}

fn group_label(state: GroupState) -> (String, egui::Color32) {
    let p = &Palette::ONE_DARK;
    match state {
        GroupState::Stopped => ("stopped".into(), p.dim),
        GroupState::Starting => ("starting…".into(), p.yellow),
        GroupState::Running => ("running".into(), p.green),
        GroupState::Degraded { running, total } => {
            (format!("degraded {running}/{total}"), p.yellow)
        }
    }
}

impl GroupsView {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        data: Option<&AppData>,
        states: Option<&Snapshot>,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some(data) = data else {
            ui.spinner();
            return commands;
        };
        let empty = Snapshot::default();
        let states = states.unwrap_or(&empty);
        egui::ScrollArea::vertical()
            .id_salt("groups")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                if self.editor.is_some() {
                    self.editor(ui, data, &mut commands);
                    return;
                }
                if ui.button("+ New group").clicked() {
                    self.editor = Some(Group {
                        id: data.tunnels.new_group_id(),
                        name: String::new(),
                        tunnels: Vec::new(),
                        start_on_launch: false,
                    });
                }
                ui.separator();
                if data.tunnels.groups.is_empty() {
                    ui.label(
                        RichText::new(
                            "No groups yet. A group starts and stops several tunnels together.",
                        )
                        .color(Palette::ONE_DARK.dim),
                    );
                }
                for group in &data.tunnels.groups {
                    self.group(ui, group, data, states, &mut commands);
                    ui.separator();
                }
            });
        commands
    }

    fn group(
        &mut self,
        ui: &mut egui::Ui,
        group: &Group,
        data: &AppData,
        states: &Snapshot,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let state = states.group_state(group);
        let (label, color) = group_label(state);
        let active = state != GroupState::Stopped;
        ui.horizontal(|ui| {
            let mut on = active;
            if ui
                .toggle_value(&mut on, RichText::new(&group.name).strong())
                .on_hover_text(if active {
                    "Stop this group"
                } else {
                    "Start this group"
                })
                .clicked()
            {
                commands.push(if active {
                    Command::StopGroup(group.id.clone())
                } else {
                    Command::StartGroup(group.id.clone())
                });
            }
            ui.label(RichText::new(label).color(color));
            if group.start_on_launch {
                ui.label(RichText::new("starts with sshelf").small().color(p.dim));
            }
            if matches!(state, GroupState::Degraded { .. })
                && ui.small_button("↻ Retry failed").clicked()
            {
                commands.push(Command::StartGroup(group.id.clone()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.confirm_delete.as_ref() != Some(&group.id)
                    && ui.small_button("Delete…").clicked()
                {
                    self.confirm_delete = Some(group.id.clone());
                }
                if ui
                    .add_enabled(!active, egui::Button::new("Edit").small())
                    .on_disabled_hover_text("Stop the group before editing it")
                    .clicked()
                {
                    self.editor = Some(group.clone());
                }
            });
        });
        if self.confirm_delete.as_ref() == Some(&group.id) {
            ui.horizontal(|ui| {
                ui.colored_label(
                    p.red,
                    format!("Delete \"{}\"? Its tunnels are kept.", group.name),
                );
                if ui.button("Delete").clicked() {
                    commands.push(Command::DeleteGroup(group.id.clone()));
                    self.confirm_delete = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = None;
                }
            });
        }

        let owner = Owner::Group(group.id.clone());
        egui::CollapsingHeader::new(format!("Tunnels ({})", group.tunnels.len()))
            .id_salt(("group_tunnels", &group.id))
            .show(ui, |ui| {
                for id in &group.tunnels {
                    let Some(tunnel) = data.tunnels.tunnel(id) else {
                        ui.colored_label(p.yellow, format!("⚠ missing tunnel `{id}`"));
                        continue;
                    };
                    let tunnel_state = states.state(id);
                    let (label, color) = state_label(&tunnel_state);
                    ui.horizontal(|ui| {
                        status_dot(ui, color).on_hover_text(&label);
                        ui.label(&tunnel.name);
                        ui.label(RichText::new(label).color(color));
                        let others: Vec<&Owner> = states
                            .tunnels
                            .get(id)
                            .map(|s| s.owners.iter().filter(|o| **o != owner).collect())
                            .unwrap_or_default();
                        if !others.is_empty() {
                            let text =
                                format!("also used by {}", owner_names(&others, &data.tunnels));
                            ui.label(RichText::new(text).small().color(p.dim));
                        }
                    });
                    if let TunnelState::Failed(reason) = &tunnel_state {
                        ui.label(RichText::new(reason).small().color(p.red));
                        if let Some(text) = reconnect_text(ui.ctx(), states.tunnels.get(id)) {
                            ui.label(RichText::new(text).small().color(p.yellow));
                        }
                        if let Some(hint) = hints::failure_hint(reason) {
                            ui.label(RichText::new(hint).small().color(p.dim));
                        }
                    }
                }
            });
        if !active {
            let held: Vec<&str> = group
                .tunnels
                .iter()
                .filter(|id| {
                    matches!(
                        states.state(id),
                        TunnelState::Running | TunnelState::Starting
                    )
                })
                .filter_map(|id| data.tunnels.tunnel(id).map(|t| t.name.as_str()))
                .collect();
            if !held.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "Still running for other owners: {}",
                        held.join(", ")
                    ))
                    .small()
                    .color(p.dim),
                );
            }
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui, data: &AppData, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        let Some(draft) = &mut self.editor else {
            return;
        };
        let is_new = data.tunnels.group(&draft.id).is_none();
        ui.heading(if is_new { "New group" } else { "Edit group" });
        ui.horizontal(|ui| {
            let label = ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut draft.name)
                    .hint_text("e.g. Work on prod")
                    .desired_width(280.0),
            )
            .labelled_by(label.id);
        });
        ui.checkbox(&mut draft.start_on_launch, "Start when sshelf starts")
            .on_hover_text(
                "Host keys are still checked first; an unknown key asks before starting",
            );
        ui.label(RichText::new("Tunnels").strong());
        if data.tunnels.tunnels.is_empty() {
            ui.label(RichText::new("Create tunnels first.").color(p.dim));
        }
        for tunnel in &data.tunnels.tunnels {
            let mut member = draft.tunnels.contains(&tunnel.id);
            let text = format!("{} ({})", tunnel.name, tunnel.host);
            if ui.checkbox(&mut member, text).changed() {
                if member {
                    draft.tunnels.push(tunnel.id.clone());
                } else {
                    draft.tunnels.retain(|t| t != &tunnel.id);
                }
            }
        }

        let mut problems = Vec::new();
        if draft.name.trim().is_empty() {
            problems.push("the name is empty");
        }
        if draft.tunnels.is_empty() {
            problems.push("choose at least one tunnel");
        }
        for problem in &problems {
            ui.colored_label(p.red, *problem);
        }
        let mut close = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(problems.is_empty(), egui::Button::new("Save"))
                .clicked()
            {
                commands.push(Command::SaveGroup(draft.clone()));
                close = true;
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
        if close {
            self.editor = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};
    use std::path::PathBuf;
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sshelf_core::supervisor::TunnelStatus;
    use sshelf_core::tunnels::Tunnel;

    use super::*;

    fn data() -> Arc<AppData> {
        let mut data = AppData::new(PathBuf::from("app"));
        for (id, name) in [("t-a", "API"), ("t-b", "DB")] {
            data.tunnels.upsert_tunnel(Tunnel {
                id: id.into(),
                name: name.into(),
                host: "h".into(),
                forwards: Vec::new(),
                auto_reconnect: false,
            });
        }
        data.tunnels.upsert_group(Group {
            id: "g-1".into(),
            name: "Work".into(),
            tunnels: vec!["t-a".into(), "t-b".into()],
            start_on_launch: false,
        });
        data.tunnels.upsert_group(Group {
            id: "g-2".into(),
            name: "Other".into(),
            tunnels: vec!["t-b".into()],
            start_on_launch: false,
        });
        Arc::new(data)
    }

    fn status(state: TunnelState, owners: &[&str]) -> TunnelStatus {
        TunnelStatus {
            state,
            owners: owners
                .iter()
                .map(|g| Owner::Group(g.to_string()))
                .collect::<BTreeSet<_>>(),
            log: VecDeque::new(),
            pid: None,
            failure: None,
            reconnect: None,
        }
    }

    fn harness(states: Snapshot) -> Harness<'static, (GroupsView, Vec<Command>)> {
        let data = data();
        let states = Arc::new(states);
        Harness::builder()
            .with_size([1000.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (GroupsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&data), Some(&states)));
                },
                (GroupsView::default(), Vec::new()),
            )
    }

    #[test]
    fn toggle_starts_a_stopped_group() {
        let mut harness = harness(Snapshot::default());
        harness.step();
        harness.get_by_label("Work").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartGroup("g-1".into())]);
    }

    #[test]
    fn degraded_group_offers_retry_and_shows_shared_tunnels() {
        let states = Snapshot {
            tunnels: [
                (
                    "t-a".to_string(),
                    status(TunnelState::Failed("Permission denied".into()), &["g-1"]),
                ),
                (
                    "t-b".to_string(),
                    status(TunnelState::Running, &["g-1", "g-2"]),
                ),
            ]
            .into(),
        };
        let mut harness = harness(states);
        harness.step();
        harness.get_by_label("degraded 1/2");
        harness.get_by_label("Tunnels (2)").click();
        harness.step();
        harness.step();
        harness.get_by_label("Permission denied");
        harness.get_by_label("also used by \"Other\"");

        harness.get_by_label("↻ Retry failed").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartGroup("g-1".into())]);
    }

    #[test]
    fn toggle_stops_a_running_group() {
        let states = Snapshot {
            tunnels: [("t-b".to_string(), status(TunnelState::Running, &["g-2"]))].into(),
        };
        let mut harness = harness(states);
        harness.step();
        harness.get_by_label("Other").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StopGroup("g-2".into())]);
    }

    #[test]
    fn editor_saves_the_chosen_tunnels() {
        let mut harness = harness(Snapshot::default());
        harness.step();
        harness.get_by_label("+ New group").click();
        harness.step();
        harness.step();
        harness.get_by_label("choose at least one tunnel");
        harness.state_mut().0.editor.as_mut().unwrap().name = "New".into();
        harness.get_by_label("DB (h)").click();
        harness.step();
        harness.get_by_label("Start when sshelf starts").click();
        harness.step();
        harness.step();
        harness.get_by_label("Save").click();
        harness.step();
        let commands = &harness.state().1;
        let [Command::SaveGroup(group)] = commands.as_slice() else {
            panic!("{commands:?}");
        };
        assert_eq!(group.name, "New");
        assert_eq!(group.tunnels, ["t-b"]);
        assert!(group.id.starts_with("g-"));
        assert!(group.start_on_launch);
    }
}
