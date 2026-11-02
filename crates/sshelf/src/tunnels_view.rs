//! Tunnels view (SPEC.md §7, §10): list with status and start/stop, a detail
//! pane with forwards, owners and the stderr log, and the tunnel editor.

use egui::RichText;
use sshconf::HostList;
use sshelf_core::core::{AppData, Command, ConfigSnapshot};
use sshelf_core::supervisor::{Owner, Snapshot, TunnelState, TunnelStatus};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Tunnel, TunnelsFile};

use crate::theme::Palette;

#[derive(Default)]
pub struct TunnelsView {
    selected: Option<String>,
    editor: Option<Tunnel>,
    show_debug: bool,
    confirm_delete: bool,
}

/// `(label, color)` for a tunnel state.
pub fn state_label(state: &TunnelState) -> (String, egui::Color32) {
    let p = &Palette::ONE_DARK;
    match state {
        TunnelState::Stopped => ("stopped".into(), p.dim),
        TunnelState::Starting => ("starting…".into(), p.yellow),
        TunnelState::Running => ("running".into(), p.green),
        TunnelState::Stopping => ("stopping…".into(), p.yellow),
        TunnelState::Failed(_) => ("failed".into(), p.red),
    }
}

/// "manual, Work on prod" for a tunnel's owners.
pub fn owner_names(owners: &[&Owner], file: &TunnelsFile) -> String {
    owners
        .iter()
        .map(|o| match o {
            Owner::Manual => "manual start".to_string(),
            Owner::Group(id) => file
                .group(id)
                .map_or_else(|| format!("group {id}"), |g| format!("\"{}\"", g.name)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn new_forward() -> Forward {
    Forward {
        kind: ForwardKind::L,
        bind: DEFAULT_BIND.to_string(),
        listen: 0,
        target: Some(String::new()),
    }
}

impl TunnelsView {
    /// Opens the editor for a new tunnel to `host` (host detail "New tunnel").
    pub fn new_tunnel(&mut self, host: &str, file: &TunnelsFile) {
        self.selected = None;
        self.confirm_delete = false;
        self.editor = Some(Tunnel {
            id: file.new_tunnel_id(),
            name: String::new(),
            host: host.to_string(),
            forwards: vec![new_forward()],
        });
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        config: Option<&ConfigSnapshot>,
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
        let no_hosts = HostList::default();
        let hosts = config.and_then(|c| c.hosts()).unwrap_or(&no_hosts);
        if self
            .selected
            .as_ref()
            .is_some_and(|id| data.tunnels.tunnel(id).is_none())
        {
            self.selected = None;
        }

        egui::Panel::left("tunnel_list")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| self.list(ui, data, states, &mut commands));
        egui::CentralPanel::default().show(ui, |ui| {
            if self.editor.is_some() {
                self.editor(ui, data, hosts, &mut commands);
            } else if let Some(tunnel) = self
                .selected
                .as_ref()
                .and_then(|id| data.tunnels.tunnel(id))
                .cloned()
            {
                self.detail(ui, &tunnel, data, states, &mut commands);
            } else {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Select a tunnel, or create one")
                            .color(Palette::ONE_DARK.dim),
                    );
                });
            }
        });
        commands
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        data: &AppData,
        states: &Snapshot,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        ui.add_space(6.0);
        if ui.button("+ New tunnel").clicked() {
            self.new_tunnel("", &data.tunnels);
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("tunnels")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if data.tunnels.tunnels.is_empty() {
                    ui.label(RichText::new("No tunnels yet").color(p.dim));
                }
                for tunnel in &data.tunnels.tunnels {
                    let state = states.state(&tunnel.id);
                    let (label, color) = state_label(&state);
                    ui.horizontal(|ui| {
                        status_dot(ui, color).on_hover_text(label);
                        let selected =
                            self.selected.as_ref() == Some(&tunnel.id) && self.editor.is_none();
                        if ui.selectable_label(selected, &tunnel.name).clicked() {
                            self.selected = Some(tunnel.id.clone());
                            self.editor = None;
                            self.confirm_delete = false;
                        }
                        ui.label(RichText::new(&tunnel.host).small().color(p.dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            start_stop(ui, tunnel, states, commands);
                        });
                    });
                }
            });
    }

    fn detail(
        &mut self,
        ui: &mut egui::Ui,
        tunnel: &Tunnel,
        data: &AppData,
        states: &Snapshot,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let status = states.tunnels.get(&tunnel.id);
        let state = states.state(&tunnel.id);
        let (label, color) = state_label(&state);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(RichText::new(&tunnel.name).color(p.accent).strong());
            ui.label(RichText::new(label).color(color));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                start_stop(ui, tunnel, states, commands);
            });
        });
        ui.label(RichText::new(format!("via {}", tunnel.host)).color(p.dim));
        if let TunnelState::Failed(reason) = &state {
            ui.colored_label(p.red, reason);
        }
        if let Some(status) = status
            && !status.owners.is_empty()
        {
            let owners: Vec<&Owner> = status.owners.iter().collect();
            ui.label(format!("Used by: {}", owner_names(&owners, &data.tunnels)));
        }
        if let Some(orphan) = data
            .tunnel_orphans
            .iter()
            .find(|o| o.tunnel_id == tunnel.id)
        {
            ui.colored_label(p.yellow, format!("⚠ {}", orphan.describe()));
        }

        ui.add_space(6.0);
        for forward in &tunnel.forwards {
            ui.horizontal(|ui| {
                ui.label(RichText::new(forward.summary()).monospace());
                if forward.is_exposed() {
                    ui.colored_label(p.yellow, "⚠ not loopback")
                        .on_hover_text(exposed_hint(forward.kind));
                }
            });
        }

        ui.add_space(6.0);
        let active = !matches!(state, TunnelState::Stopped | TunnelState::Failed(_));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!active, egui::Button::new("Edit"))
                .on_disabled_hover_text("Stop the tunnel before editing it")
                .clicked()
            {
                self.editor = Some(tunnel.clone());
            }
            if !self.confirm_delete && ui.button("Delete…").clicked() {
                self.confirm_delete = true;
            }
        });
        if self.confirm_delete {
            let groups: Vec<String> = data
                .tunnels
                .groups_using(&tunnel.id)
                .iter()
                .map(|g| format!("\"{}\"", g.name))
                .collect();
            let mut message = format!("Delete \"{}\"?", tunnel.name);
            if !groups.is_empty() {
                message.push_str(&format!(" It is also removed from {}.", groups.join(", ")));
            }
            ui.colored_label(p.red, message);
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    commands.push(Command::DeleteTunnel(tunnel.id.clone()));
                    self.confirm_delete = false;
                    self.selected = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = false;
                }
            });
        }

        ui.separator();
        log(ui, status, &mut self.show_debug);
    }

    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        data: &AppData,
        hosts: &HostList,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let Some(draft) = &mut self.editor else {
            return;
        };
        let is_new = data.tunnels.tunnel(&draft.id).is_none();
        ui.add_space(6.0);
        ui.heading(if is_new { "New tunnel" } else { "Edit tunnel" });
        egui::Grid::new("tunnel_fields")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let label = ui.label("Name");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .hint_text("e.g. Prod Postgres")
                        .desired_width(280.0),
                )
                .labelled_by(label.id);
                ui.end_row();

                let label = ui.label("Host");
                egui::ComboBox::from_id_salt("tunnel_host")
                    .selected_text(if draft.host.is_empty() {
                        "choose a host"
                    } else {
                        draft.host.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for host in &hosts.hosts {
                            let alias = host.primary();
                            if ui.selectable_label(draft.host == alias, alias).clicked() {
                                draft.host = alias.to_string();
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.label(RichText::new("Forwards").strong());
        let mut remove = None;
        egui::Grid::new("forwards")
            .num_columns(5)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                let kind_h = ui.label("Kind");
                let bind_h = ui.label("Bind address");
                let port_h = ui.label("Port");
                let target_h = ui.label("Target (host:port)");
                ui.label("");
                ui.end_row();
                for (i, forward) in draft.forwards.iter_mut().enumerate() {
                    egui::ComboBox::from_id_salt(("kind", i))
                        .width(60.0)
                        .selected_text(format!("{:?}", forward.kind))
                        .show_ui(ui, |ui| {
                            for kind in [ForwardKind::L, ForwardKind::R, ForwardKind::D] {
                                ui.selectable_value(&mut forward.kind, kind, format!("{kind:?}"));
                            }
                        })
                        .response
                        .labelled_by(kind_h.id);
                    ui.add(egui::TextEdit::singleline(&mut forward.bind).desired_width(120.0))
                        .labelled_by(bind_h.id);
                    ui.add(egui::DragValue::new(&mut forward.listen).range(0..=65535))
                        .labelled_by(port_h.id);
                    if forward.kind == ForwardKind::D {
                        forward.target = None;
                        ui.label(RichText::new("SOCKS proxy").color(p.dim));
                    } else {
                        let target = forward.target.get_or_insert_with(String::new);
                        ui.add(
                            egui::TextEdit::singleline(target)
                                .hint_text("localhost:5432")
                                .desired_width(180.0),
                        )
                        .labelled_by(target_h.id);
                    }
                    if ui
                        .small_button("✖")
                        .on_hover_text("Remove this forward")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            draft.forwards.remove(i);
        }
        if ui.button("+ Add forward").clicked() {
            draft.forwards.push(new_forward());
        }
        for forward in draft.forwards.iter().filter(|f| f.is_exposed()) {
            ui.colored_label(
                p.yellow,
                format!("⚠ {}: {}", forward.bind, exposed_hint(forward.kind)),
            );
        }

        let mut problems = draft.validate();
        if !draft.host.is_empty() && hosts.find(&draft.host).is_none() {
            problems.push(format!("no host `{}` in the ssh config", draft.host));
        }
        for problem in &problems {
            ui.colored_label(p.red, problem);
        }
        ui.add_space(6.0);
        let mut close = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(problems.is_empty(), egui::Button::new("Save"))
                .clicked()
            {
                commands.push(Command::SaveTunnel(draft.clone()));
                self.selected = Some(draft.id.clone());
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

/// A small filled circle (the bundled fonts have no `●`).
pub fn status_dot(ui: &mut egui::Ui, color: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    response
}

fn exposed_hint(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::R => {
            "listens on a non-loopback address of the server: other machines can reach it"
        }
        _ => "listens on a non-loopback address: other machines on your network can reach it",
    }
}

fn start_stop(ui: &mut egui::Ui, tunnel: &Tunnel, states: &Snapshot, commands: &mut Vec<Command>) {
    let status = states.tunnels.get(&tunnel.id);
    let manual = status.is_some_and(|s| s.owners.contains(&Owner::Manual));
    let state = states.state(&tunnel.id);
    if manual {
        if ui.small_button("⏹ Stop").clicked() {
            commands.push(Command::StopTunnel(tunnel.id.clone()));
        }
    } else {
        let label = if matches!(state, TunnelState::Failed(_)) {
            "↻ Retry"
        } else {
            "▶ Start"
        };
        let button = ui.small_button(label);
        let button = if matches!(state, TunnelState::Running | TunnelState::Starting) {
            button.on_hover_text("Already running for a group; this also keeps it up on its own")
        } else {
            button
        };
        if button.clicked() {
            commands.push(Command::StartTunnel(tunnel.id.clone()));
        }
    }
}

fn log(ui: &mut egui::Ui, status: Option<&TunnelStatus>, show_debug: &mut bool) {
    let p = &Palette::ONE_DARK;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Log").strong());
        ui.checkbox(show_debug, "show debug lines");
    });
    let Some(status) = status else {
        ui.label(RichText::new("Not started in this session.").color(p.dim));
        return;
    };
    egui::ScrollArea::vertical()
        .id_salt("tunnel_log")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for line in status.log.iter().filter(|l| *show_debug || !l.debug) {
                let text = RichText::new(&line.text).monospace().small();
                ui.label(if line.debug { text.color(p.dim) } else { text });
            }
        });
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sshconf::{ConfigSet, ResolveOptions};
    use sshelf_core::core::ConfigState;
    use sshelf_core::supervisor::LogLine;

    use super::*;

    struct Fixture {
        config: Arc<ConfigSnapshot>,
        data: Arc<AppData>,
        states: Arc<Snapshot>,
    }

    fn tunnel() -> Tunnel {
        Tunnel {
            id: "t-1".into(),
            name: "Prod DB".into(),
            host: "bastion".into(),
            forwards: vec![Forward {
                kind: ForwardKind::L,
                bind: DEFAULT_BIND.into(),
                listen: 5432,
                target: Some("db:5432".into()),
            }],
        }
    }

    fn fixture(state: TunnelState) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config");
        fs::write(&path, "Host bastion\nHost web\n").unwrap();
        let set = ConfigSet::load(&path, &ResolveOptions::new(tmp.path())).unwrap();
        let hosts = HostList::from_set(&set);
        let mut data = AppData::new(PathBuf::from("app"));
        data.tunnels.upsert_tunnel(tunnel());
        let status = TunnelStatus {
            state,
            owners: BTreeSet::from([Owner::Manual]),
            log: VecDeque::from([
                LogLine {
                    text: "debug1: hidden".into(),
                    debug: true,
                },
                LogLine {
                    text: "Authenticated to bastion".into(),
                    debug: false,
                },
            ]),
            pid: Some(1),
        };
        Fixture {
            config: Arc::new(ConfigSnapshot {
                generation: 1,
                path,
                state: ConfigState::Loaded { set, hosts },
            }),
            data: Arc::new(data),
            states: Arc::new(Snapshot {
                tunnels: [("t-1".to_string(), status)].into(),
            }),
        }
    }

    fn harness(f: Fixture, view: TunnelsView) -> Harness<'static, (TunnelsView, Vec<Command>)> {
        Harness::builder()
            .with_size([1200.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (TunnelsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&f.config), Some(&f.data), Some(&f.states)));
                },
                (view, Vec::new()),
            )
    }

    #[test]
    fn running_tunnel_shows_owners_and_log_and_cannot_be_edited() {
        let view = TunnelsView {
            selected: Some("t-1".into()),
            ..TunnelsView::default()
        };
        let mut harness = harness(fixture(TunnelState::Running), view);
        harness.step();
        harness.get_by_label("Used by: manual start");
        harness.get_by_label("Authenticated to bastion");
        assert!(harness.query_by_label("debug1: hidden").is_none());
        harness.get_by_label("Edit").click();
        harness.step();
        assert!(
            harness.state().0.editor.is_none(),
            "editing is disabled while running"
        );

        harness.get_by_label("show debug lines").click();
        harness.step();
        harness.step();
        harness.get_by_label("debug1: hidden");
    }

    #[test]
    fn stop_releases_the_manual_start() {
        let view = TunnelsView {
            selected: Some("t-1".into()),
            ..TunnelsView::default()
        };
        let mut harness = harness(fixture(TunnelState::Running), view);
        harness.step();
        harness.get_all_by_label("⏹ Stop").next().unwrap().click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StopTunnel("t-1".into())]);
    }

    #[test]
    fn editor_validates_and_warns_about_exposed_binds() {
        let f = fixture(TunnelState::Stopped);
        let mut view = TunnelsView::default();
        view.new_tunnel("bastion", &f.data.tunnels);
        let mut harness = harness(f, view);
        harness.step();
        harness.get_by_label("the name is empty");
        harness.get_by_label("forward 1: the port must be 1–65535");

        let draft = harness.state_mut().0.editor.as_mut().unwrap();
        draft.name = "API".into();
        draft.forwards[0].listen = 6443;
        draft.forwards[0].target = Some("localhost:6443".into());
        draft.forwards[0].bind = "0.0.0.0".into();
        harness.step();
        harness.step();
        assert!(harness.query_by_label("the name is empty").is_none());
        harness.get_by_label_contains("⚠ 0.0.0.0");

        harness.get_by_label("Save").click();
        harness.step();
        let commands = &harness.state().1;
        assert_eq!(commands.len(), 1);
        let Command::SaveTunnel(saved) = &commands[0] else {
            panic!("{commands:?}");
        };
        assert_eq!(saved.host, "bastion");
        assert!(saved.id.starts_with("t-"));
    }
}
