//! Tunnels view (SPEC.md §7, §10; boards A-Tunnels, Tunnel-States,
//! Tunnel-Edit): a list of two-line rows with each state's action, a detail
//! pane (status chip with uptime, owners, failure banner, forwards, the log
//! with times), the editor (forwards as separate fields, a searchable host,
//! the ssh command it runs) and the delete dialog.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Frame, Id, Layout, Margin, Rect, RichText, Role,
    Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};
use sshconf::HostList;
use sshelf_core::core::{AppData, Command, ConfigSnapshot, SshInfo};
use sshelf_core::hints;
use sshelf_core::supervisor::{FailureKind, Owner, Snapshot, TunnelState, TunnelStatus};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Tunnel, TunnelsFile};

use crate::theme::{Palette, radius, timing};
use crate::tour::steps;
use crate::tour::targets::Target;
use crate::widgets::button::{self, Button};
use crate::widgets::dropdown::{Choice, Dropdown};
use crate::widgets::field::TextField;
use crate::widgets::switch::switch;
use crate::widgets::{marks, nav, popup, text};
use crate::{fonts, icons, motion, tour};

/// Width of the tunnel list.
const LIST_WIDTH: f32 = 340.0;
/// A list row (two lines).
const ROW: f32 = 52.0;
const ROW_GAP: f32 = 2.0;

#[derive(Default)]
pub struct TunnelsView {
    selected: Option<String>,
    editor: Option<Tunnel>,
    show_debug: bool,
    /// The tunnel the delete dialog is for.
    confirm_delete: Option<String>,
    /// The "via" host link was clicked: the app shows that host.
    open_host: Option<String>,
    /// *Show log* was clicked: scroll the log into view.
    show_log: bool,
    /// Targets set aside while their forward is *D* (forward index →
    /// `host:port`), so switching back to L or R keeps what was typed.
    set_aside: HashMap<usize, String>,
    /// A key fix the failure banner asked for, for the app.
    key_fix: Option<KeyFix>,
}

/// `(label, color)` for a tunnel state.
pub fn state_label(state: &TunnelState) -> (String, Color32) {
    let p = &Palette::ONE_DARK;
    match state {
        TunnelState::Stopped => ("stopped".into(), p.dim),
        TunnelState::Starting => ("starting…".into(), p.yellow),
        TunnelState::Running => ("running".into(), p.green),
        TunnelState::Stopping => ("stopping…".into(), p.yellow),
        TunnelState::Failed(_) => ("failed".into(), p.red),
    }
}

/// "Reconnecting in 8 s (attempt 3)" while an automatic restart is pending
/// (SPEC.md §7); asks for a repaint each second so the countdown moves.
pub fn reconnect_text(ctx: &egui::Context, status: Option<&TunnelStatus>) -> Option<String> {
    let reconnect = status?.reconnect?;
    let left = reconnect
        .at
        .duration_since(SystemTime::now())
        .unwrap_or_default();
    ctx.request_repaint_after(Duration::from_secs(1));
    Some(format!(
        "Reconnecting in {} s (attempt {})",
        left.as_secs() + u64::from(left.subsec_millis() > 0),
        reconnect.attempt
    ))
}

/// "manual start, \"Work on prod\"" for a tunnel's owners.
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

/// "manual start, group “Work on prod”": the owners as the tunnels view
/// words them.
fn owners_text(owners: &[&Owner], file: &TunnelsFile) -> String {
    owners
        .iter()
        .map(|o| match o {
            Owner::Manual => "manual start".to_string(),
            Owner::Group(id) => file.group(id).map_or_else(
                || format!("group {id}"),
                |g| format!("group \u{201c}{}\u{201d}", g.name),
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "40 s", "14 m", "2 h 14 m" since `since`; asks for a repaint when the
/// shown value changes.
fn uptime(ctx: &egui::Context, since: Instant) -> String {
    let secs = since.elapsed().as_secs();
    if secs < 60 {
        ctx.request_repaint_after(Duration::from_secs(1));
        format!("{secs} s")
    } else {
        ctx.request_repaint_after(Duration::from_secs(60 - secs % 60));
        let (h, m) = (secs / 3600, (secs / 60) % 60);
        if h == 0 {
            format!("{m} m")
        } else {
            format!("{h} h {m} m")
        }
    }
}

fn new_forward() -> Forward {
    Forward {
        kind: ForwardKind::L,
        bind: DEFAULT_BIND.to_string(),
        listen: 0,
        target: Some(String::new()),
    }
}

/// "L 6443": a forward's kind and listen port.
fn forward_short(forward: &Forward) -> String {
    format!("{:?} {}", forward.kind, forward.listen)
}

/// The target `host:port` split for its two fields (IPv6 without brackets).
fn split_target(target: &str) -> (String, String) {
    match target.rsplit_once(':') {
        Some((host, port)) => {
            let host = host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host);
            (host.to_string(), port.to_string())
        }
        None => (target.to_string(), String::new()),
    }
}

/// The two fields back as `host:port` (an IPv6 host in brackets).
fn join_target(host: &str, port: &str) -> String {
    let host = host.trim();
    if host.contains(':') {
        format!("[{host}]:{}", port.trim())
    } else {
        format!("{host}:{}", port.trim())
    }
}

/// `ssh …` as the supervisor runs it (the binary shown as `ssh`).
fn command_line(tunnel: &Tunnel) -> String {
    let args: Vec<String> = tunnel
        .ssh_args()
        .into_iter()
        .map(|a| {
            if a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a
            }
        })
        .collect();
    format!("ssh {}", args.join(" "))
}

/// What a list row offers for its state (SPEC.md §10, Tunnel-States).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Start,
    Stop,
    Cancel,
    Retry,
    Review,
}

impl Action {
    fn of(state: &TunnelState, status: Option<&TunnelStatus>) -> Option<Action> {
        let manual = status.is_some_and(|s| s.owners.contains(&Owner::Manual));
        match state {
            TunnelState::Starting if manual => Some(Action::Cancel),
            TunnelState::Starting | TunnelState::Stopping => None,
            // A group's tunnel keeps Start: it adds a manual owner.
            TunnelState::Running if manual => Some(Action::Stop),
            TunnelState::Running | TunnelState::Stopped => Some(Action::Start),
            TunnelState::Failed(_)
                if status.and_then(|s| s.failure) == Some(FailureKind::HostKeyChanged) =>
            {
                Some(Action::Review)
            }
            TunnelState::Failed(_) => Some(Action::Retry),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::Stop => "Stop",
            Action::Cancel => "Cancel",
            Action::Retry => "Retry",
            Action::Review => "Review…",
        }
    }

    fn icon(self) -> Option<char> {
        match self {
            Action::Start => Some(icons::PLAY),
            Action::Stop => Some(icons::SQUARE),
            Action::Retry => Some(icons::ROTATE_CW),
            Action::Cancel | Action::Review => None,
        }
    }

    fn command(self, id: &str) -> Command {
        match self {
            Action::Start | Action::Retry => Command::StartTunnel(id.to_string()),
            Action::Stop | Action::Cancel => Command::StopTunnel(id.to_string()),
            Action::Review => Command::ReviewChangedKey(id.to_string()),
        }
    }

    /// The button; `small` in list rows.
    fn button(self, small: bool, tip: Option<&str>) -> Button<'_> {
        let mut button = Button::new(self.label());
        if let Some(icon) = self.icon() {
            button = button.icon(icon);
        }
        if small {
            button = button.small();
        }
        if let Some(tip) = tip {
            button = button.tip(tip);
        }
        button
    }
}

/// The status mark: filled when settled, a pulsing ring in transition.
pub fn mark(ui: &mut Ui, state: &TunnelState) {
    let p = &Palette::ONE_DARK;
    match state {
        TunnelState::Running => marks::dot(ui, p.green),
        TunnelState::Stopped => marks::dot(ui, p.comment),
        TunnelState::Failed(_) => marks::dot(ui, p.red),
        TunnelState::Starting => marks::pulsing_ring(ui, marks::DOT, p.yellow),
        TunnelState::Stopping => marks::pulsing_ring(ui, marks::DOT, p.dim),
    };
}

/// A row's second line and its colour.
fn sub_line(
    ctx: &egui::Context,
    tunnel: &Tunnel,
    status: Option<&TunnelStatus>,
    file: &TunnelsFile,
) -> (String, Color32) {
    let p = &Palette::ONE_DARK;
    let state = status.map_or(TunnelState::Stopped, |s| s.state.clone());
    let forward = tunnel.forwards.first().map(forward_short);
    let with_forward = |text: String| match &forward {
        Some(f) => format!("{text} · {f}"),
        None => text,
    };
    match &state {
        TunnelState::Starting => ("Starting… connecting".into(), p.yellow),
        TunnelState::Stopping => ("Stopping…".into(), p.dim),
        TunnelState::Stopped => (with_forward("Stopped".into()), p.dim),
        TunnelState::Running => {
            let status = status.expect("running has a status");
            if !status.owners.contains(&Owner::Manual) {
                let owners: Vec<&Owner> = status.owners.iter().collect();
                (format!("Running · {}", owners_text(&owners, file)), p.dim)
            } else {
                let up = status
                    .running_since
                    .map(|since| format!(" · {}", uptime(ctx, since)))
                    .unwrap_or_default();
                (with_forward(format!("Running{up}")), p.dim)
            }
        }
        TunnelState::Failed(reason) => {
            if let Some(text) = reconnect_text(ctx, status) {
                return (text, p.yellow);
            }
            let why = match status.and_then(|s| s.failure) {
                Some(FailureKind::HostKeyChanged) => "host key changed".to_string(),
                Some(FailureKind::HostKeyUnknown) => "host key not known yet".to_string(),
                Some(FailureKind::Auth) => "key not accepted".to_string(),
                _ => reason.clone(),
            };
            (format!("Failed · {why}"), p.red)
        }
    }
}

impl TunnelsView {
    /// Shows tunnel `id` (from a host's *Tunnels* tab).
    pub fn select(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        self.editor = None;
        self.confirm_delete = None;
    }

    /// The host whose "via" link was clicked, once.
    pub fn take_open_host(&mut self) -> Option<String> {
        self.open_host.take()
    }

    /// A key fix the failure banner asked for, once.
    pub fn take_key_fix(&mut self) -> Option<KeyFix> {
        self.key_fix.take()
    }

    /// The editor is open.
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub fn close_editor(&mut self) {
        self.editor = None;
        self.set_aside.clear();
    }

    /// The draft in the editor (the tour fills it in).
    pub fn draft_mut(&mut self) -> Option<&mut Tunnel> {
        self.editor.as_mut()
    }

    /// Opens the editor for a new tunnel to `host` (host detail "New tunnel").
    pub fn new_tunnel(&mut self, host: &str, file: &TunnelsFile) {
        self.selected = None;
        self.confirm_delete = None;
        self.editor = Some(Tunnel {
            id: file.new_tunnel_id(),
            name: String::new(),
            host: host.to_string(),
            forwards: vec![new_forward()],
            auto_reconnect: false,
        });
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
                ui.painter()
                    .vline(rect.right(), rect.y_range(), Stroke::new(1.0, p.bg_dark));
                if self.editor.is_some() {
                    ui.multiply_opacity(0.55);
                }
                self.list(ui, data, states, &mut commands);
            });
        egui::CentralPanel::default()
            .frame(Frame::NONE.inner_margin(Margin {
                left: 32,
                right: 32,
                top: 0,
                bottom: 24,
            }))
            .show(ui, |ui| {
                if self.editor.is_some() {
                    self.editor(ui, data, hosts, &mut commands);
                } else if let Some(tunnel) = self
                    .selected
                    .as_ref()
                    .and_then(|id| data.tunnels.tunnel(id))
                    .cloned()
                {
                    self.detail(ui, &tunnel, data, states, ssh, &mut commands);
                } else {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Select a tunnel, or create one")
                                .size(16.0)
                                .color(p.dim),
                        );
                    });
                }
            });
        self.delete_dialog(ui.ctx(), data, &mut commands);
        commands
    }

    fn list(
        &mut self,
        ui: &mut Ui,
        data: &AppData,
        states: &Snapshot,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let tunnels = &data.tunnels.tunnels;
        let running = tunnels
            .iter()
            .filter(|t| states.state(&t.id) == TunnelState::Running)
            .count();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Tunnels")
                    .font(fonts::semibold(16.0))
                    .color(p.text_strong),
            );
            ui.label(
                RichText::new(format!("{running} of {} running", tunnels.len()))
                    .size(13.0)
                    .color(p.dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let response = ui.add(Button::icon_only(icons::PLUS, "New tunnel"));
                if tour::targets::clicked(ui.ctx(), Target::NewTunnel, &response) {
                    self.new_tunnel("", &data.tunnels);
                }
            });
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("tunnels")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = ROW_GAP;
                if tunnels.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No tunnels yet").size(14.0).color(p.dim));
                        ui.label(
                            RichText::new("A tunnel forwards ports through one of your hosts.")
                                .size(12.0)
                                .color(p.dim),
                        );
                    });
                    return;
                }
                let selection_slot = ui.painter().add(Shape::Noop);
                let hover_slot = ui.painter().add(Shape::Noop);
                let mut hovered = None;
                let mut selected_rect = None;
                for (index, tunnel) in tunnels.iter().enumerate() {
                    let status = states.tunnels.get(&tunnel.id);
                    let state = states.state(&tunnel.id);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    let is_selected =
                        self.selected.as_ref() == Some(&tunnel.id) && self.editor.is_none();
                    response.widget_info(|| {
                        WidgetInfo::selected(Role::Button, true, is_selected, &tunnel.name)
                    });
                    if is_selected {
                        selected_rect = Some((index, rect));
                    }
                    if response.hovered() || popup::menu_is_open(&response) {
                        hovered = Some((index, rect));
                    }
                    if response.clicked() {
                        self.select(&tunnel.id);
                    }
                    let mut row = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect.shrink2(vec2(12.0, 0.0)))
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    row.spacing_mut().item_spacing.x = 10.0;
                    mark(&mut row, &state);
                    let action = Action::of(&state, status);
                    let button_width = if action.is_some() { 84.0 } else { 0.0 };
                    let text_width = (row.available_width() - button_width).max(40.0);
                    row.allocate_ui_with_layout(
                        vec2(text_width, ROW),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.add_space(9.0);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let color = if is_selected { p.text_strong } else { p.text };
                            let (name, _) = text::elide(
                                ui.painter(),
                                &tunnel.name,
                                &FontId::proportional(14.0),
                                text_width,
                            );
                            // Not selectable: clicks (and right clicks for
                            // the menu) go to the row.
                            ui.add(
                                egui::Label::new(RichText::new(name).size(14.0).color(color))
                                    .selectable(false),
                            );
                            let (line, color) = sub_line(ui.ctx(), tunnel, status, &data.tunnels);
                            let font = FontId::monospace(11.0);
                            let (line, _) = text::elide(ui.painter(), &line, &font, text_width);
                            ui.add(
                                egui::Label::new(RichText::new(line).font(font).color(color))
                                    .selectable(false),
                            );
                        },
                    );
                    let tour_id = steps::static_id(&tunnel.id);
                    if let Some(id) = tour_id {
                        tour::targets::register(ui.ctx(), Target::TunnelRow(id), rect);
                    }
                    if let Some(action) = action {
                        row.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let tip = (action == Action::Start && state == TunnelState::Running)
                                .then_some("Started by a group; Start also keeps it up on its own");
                            let response = ui.add(action.button(true, tip));
                            let clicked = match tour_id {
                                Some(id) => tour::targets::clicked(
                                    ui.ctx(),
                                    Target::TunnelAction(id),
                                    &response,
                                ),
                                None => response.clicked(),
                            };
                            if clicked {
                                commands.push(action.command(&tunnel.id));
                            }
                        });
                    }
                    if let Some(chosen) = tunnel_menu(&response, &tunnel.id, &state, status) {
                        match chosen {
                            MenuAction::Command(command) => commands.push(command),
                            MenuAction::Edit => {
                                self.select(&tunnel.id);
                                self.editor = Some(tunnel.clone());
                            }
                            MenuAction::CopySsh => ui.ctx().copy_text(command_line(tunnel)),
                            MenuAction::Delete => self.confirm_delete = Some(tunnel.id.clone()),
                        }
                    }
                    response.on_hover_cursor(CursorIcon::PointingHand);
                }
                if let Some((index, rect)) = selected_rect {
                    let target = motion::tween_rect(
                        ui.ctx(),
                        Id::unique("tunnel_selection"),
                        index,
                        rect,
                        timing::SLIDE,
                    );
                    ui.painter().set(
                        selection_slot,
                        Shape::rect_filled(target, radius::CONTROL, p.selection),
                    );
                }
                let hovered = hovered.filter(|(i, _)| selected_rect.map(|(s, _)| s) != Some(*i));
                nav::hover_highlight(
                    ui,
                    Id::unique("tunnel_hover"),
                    hover_slot,
                    hovered,
                    p.surface,
                );
            });
    }

    #[allow(clippy::too_many_arguments)]
    fn detail(
        &mut self,
        ui: &mut Ui,
        tunnel: &Tunnel,
        data: &AppData,
        states: &Snapshot,
        ssh: Option<&SshInfo>,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let status = states.tunnels.get(&tunnel.id);
        let state = states.state(&tunnel.id);
        let active = !matches!(state, TunnelState::Stopped | TunnelState::Failed(_));
        ui.spacing_mut().item_spacing.y = 16.0;

        // Header: name and status chip; how it runs; its actions.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new(&tunnel.name)
                            .font(fonts::semibold(22.0))
                            .color(p.text_strong),
                    );
                    let (chip, fill, color) = match &state {
                        TunnelState::Running => {
                            let up = status
                                .and_then(|s| s.running_since)
                                .map(|since| format!(" · {}", uptime(ui.ctx(), since)))
                                .unwrap_or_default();
                            (format!("Running{up}"), p.green_tint, p.green)
                        }
                        TunnelState::Starting => ("Starting…".into(), p.yellow_tint, p.yellow),
                        TunnelState::Stopping => ("Stopping…".into(), p.surface, p.dim),
                        TunnelState::Stopped => ("Stopped".into(), p.surface, p.dim),
                        TunnelState::Failed(_) => ("Failed".into(), p.red_tint, p.red),
                    };
                    marks::chip(ui, &chip, fill, color);
                });
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new("via").size(13.0).color(p.dim));
                    if button::link(ui, &tunnel.host, 13.0).clicked() {
                        self.open_host = Some(tunnel.host.clone());
                    }
                    if let Some(status) = status.filter(|s| !s.owners.is_empty()) {
                        let owners: Vec<&Owner> = status.owners.iter().collect();
                        ui.label(
                            RichText::new(format!(
                                "· owners: {}",
                                owners_text(&owners, &data.tunnels)
                            ))
                            .size(13.0)
                            .color(p.dim),
                        );
                    }
                });
                if tunnel.auto_reconnect {
                    ui.label(
                        RichText::new("Reconnects automatically after a dropped connection")
                            .size(12.0)
                            .color(p.dim),
                    );
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // Right to left: the state's action ends up rightmost.
                let manual = status.is_some_and(|s| s.owners.contains(&Owner::Manual));
                if let Some(action) = Action::of(&state, status) {
                    let tip = (action == Action::Start && state == TunnelState::Running)
                        .then_some("Started by a group; Start also keeps it up on its own");
                    if ui.add(action.button(false, tip)).clicked() {
                        commands.push(action.command(&tunnel.id));
                    }
                }
                // A failed manual start can also be given up.
                if manual
                    && matches!(state, TunnelState::Failed(_))
                    && ui
                        .add(
                            Button::new("Stop")
                                .icon(icons::SQUARE)
                                .tip("Gives up this start (and a pending reconnect)"),
                        )
                        .clicked()
                {
                    commands.push(Command::StopTunnel(tunnel.id.clone()));
                }
                let edit_tip = if active {
                    "Stop the tunnel before editing it"
                } else {
                    "Change its host, forwards or name"
                };
                if ui
                    .add_enabled(!active, Button::new("Edit").tip(edit_tip))
                    .clicked()
                {
                    self.editor = Some(tunnel.clone());
                }
                if ui.add(Button::new("Delete…").ghost()).clicked() {
                    self.confirm_delete = Some(tunnel.id.clone());
                }
            });
        });

        if let TunnelState::Failed(reason) = &state {
            failure_banner(
                ui,
                (tunnel, status, reason),
                ssh,
                commands,
                (&mut self.show_log, &mut self.key_fix),
            );
        }
        if let Some(orphan) = data
            .tunnel_orphans
            .iter()
            .find(|o| o.tunnel_id == tunnel.id)
        {
            marks::banner(ui, p.yellow_tint, |ui| {
                ui.horizontal_wrapped(|ui| {
                    icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                    ui.label(RichText::new(orphan.describe()).color(p.yellow));
                });
            });
        }
        forwards_table(ui, tunnel);
        let scroll_to_log = std::mem::take(&mut self.show_log);
        log(ui, status, &state, &mut self.show_debug, scroll_to_log);
    }

    fn editor(
        &mut self,
        ui: &mut Ui,
        data: &AppData,
        hosts: &HostList,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let Some(draft) = &mut self.editor else {
            return;
        };
        let is_new = data.tunnels.tunnel(&draft.id).is_none();
        let mut problems = draft.validate();
        if !draft.host.is_empty() && hosts.find(&draft.host).is_none() {
            problems.push(format!("no host `{}` in the ssh config", draft.host));
        }
        let general: Vec<&String> = problems
            .iter()
            .filter(|p| !p.starts_with("forward "))
            .collect();
        let mut close = false;
        let mut save = None;

        // The footer first, so the form gets the space above it.
        egui::Panel::bottom("tunnel_editor_footer")
            .show_separator_line(false)
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let line = ui.available_rect_before_wrap();
                ui.painter()
                    .hline(line.x_range(), line.top(), Stroke::new(1.0, p.selection));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let ok = problems.is_empty();
                    let why = problems.first().map(|p| capitalized(p));
                    let mut save_button = Button::new("Save").primary();
                    let mut start_button = Button::new("Save and start");
                    if let Some(why) = &why {
                        save_button = save_button.tip(why);
                        start_button = start_button.tip(why);
                    }
                    let response = ui.add_enabled(ok, save_button);
                    if tour::targets::clicked(ui.ctx(), Target::TunnelSave, &response) && ok {
                        save = Some(false);
                    }
                    if ui.add_enabled(ok, start_button).clicked() {
                        save = Some(true);
                    }
                    close = ui.add(Button::new("Cancel").ghost()).clicked();
                    if !problems.is_empty() {
                        let count = match problems.len() {
                            1 => "1 problem".to_string(),
                            n => format!("{n} problems"),
                        };
                        ui.label(RichText::new(count).size(12.0).color(p.red));
                    }
                });
                ui.add_space(16.0);
                tour::targets::register(ui.ctx(), Target::TunnelEditorFooter, ui.min_rect());
            });

        egui::ScrollArea::vertical()
            .id_salt("tunnel_editor")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.label(
                    RichText::new(if is_new { "New tunnel" } else { "Edit tunnel" })
                        .font(fonts::semibold(20.0))
                        .color(p.text_strong),
                );
                ui.add_space(4.0);
                let errors_for = |prefix: &str| -> Vec<String> {
                    general
                        .iter()
                        .filter(|p| p.starts_with(prefix))
                        .map(|p| capitalized(p))
                        .collect()
                };
                editor_row(ui, ("Name", false, LABEL), |ui, label| {
                    let errors = errors_for("the name");
                    ui.add(
                        TextField::new(&mut draft.name)
                            .hint("e.g. Prod Postgres")
                            .width(420.0)
                            .error(!errors.is_empty()),
                    )
                    .labelled_by(label);
                    field_errors(ui, &errors);
                });
                editor_row(ui, ("Host", true, LABEL), |ui, _| {
                    let mut errors = errors_for("no host");
                    let choices: Vec<Choice<usize>> = hosts
                        .hosts
                        .iter()
                        .enumerate()
                        .map(|(i, h)| Choice::new(i, h.primary()))
                        .collect();
                    let selected = hosts
                        .hosts
                        .iter()
                        .position(|h| h.primary() == draft.host)
                        .unwrap_or(usize::MAX);
                    let trigger = if draft.host.is_empty() {
                        "choose a host".to_string()
                    } else {
                        draft.host.clone()
                    };
                    let picked = Dropdown::with_choices("tunnel_host", "Host", choices, trigger)
                        .field(420.0)
                        .mono()
                        .searchable("hosts")
                        .show(ui, selected);
                    if let Some(host) = picked.and_then(|i| hosts.hosts.get(i)) {
                        draft.host = host.primary().to_string();
                        errors.clear();
                    }
                    field_errors(ui, &errors);
                });
                editor_row(ui, ("Reconnect", true, LABEL), |ui, _| {
                    // As tall as a field, so it lines up with its label.
                    let size = vec2(ui.available_width(), 32.0);
                    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        switch(ui, &mut draft.auto_reconnect, "Reconnect automatically");
                        ui.label(
                            RichText::new(
                                "automatically after a dropped connection, with growing delay",
                            )
                            .size(12.0)
                            .color(p.dim),
                        );
                    });
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Forwards")
                            .font(fonts::semibold(13.0))
                            .color(p.text_strong),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("one ssh process for all forwards")
                                .size(12.0)
                                .color(p.dim),
                        );
                    });
                });
                field_errors(ui, &errors_for("add at least"));
                let mut remove = None;
                for (index, forward) in draft.forwards.iter_mut().enumerate() {
                    let prefix = format!("forward {}: ", index + 1);
                    let errors: Vec<String> = problems
                        .iter()
                        .filter_map(|p| p.strip_prefix(&prefix))
                        .map(capitalized)
                        .collect();
                    let aside = &mut self.set_aside;
                    let top = ui.cursor().top();
                    if forward_card(ui, (index, forward), &errors, aside) {
                        remove = Some(index);
                    }
                    if index == 0 {
                        let card = ui.min_rect().with_min_y(top).intersect(ui.clip_rect());
                        tour::targets::register(ui.ctx(), Target::ForwardRow, card);
                    }
                }
                if let Some(index) = remove {
                    draft.forwards.remove(index);
                    // The indexes moved.
                    self.set_aside.clear();
                }
                if ui
                    .add(Button::new("Add forward").icon(icons::PLUS).small())
                    .clicked()
                {
                    draft.forwards.push(new_forward());
                }

                // What will run.
                Frame::new()
                    .fill(p.bg_dark)
                    .corner_radius(radius::CONTROL)
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Command").size(12.0).color(p.dim));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(command_line(draft))
                                        .font(FontId::monospace(12.0))
                                        .color(p.text),
                                )
                                .wrap(),
                            );
                        });
                    });
            });

        if let Some(start) = save {
            commands.push(Command::SaveTunnel(draft.clone()));
            if start {
                commands.push(Command::StartTunnel(draft.id.clone()));
            }
            self.selected = Some(draft.id.clone());
            close = true;
        }
        if close {
            self.editor = None;
            self.set_aside.clear();
        }
    }

    /// "Delete tunnel “x”?": what else it is removed from; *Cancel* (grey)
    /// has the focus (SPEC.md §10 *Destructive confirmations*).
    fn delete_dialog(&mut self, ctx: &egui::Context, data: &AppData, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        let Some(tunnel) = self
            .confirm_delete
            .as_ref()
            .and_then(|id| data.tunnels.tunnel(id))
            .cloned()
        else {
            self.confirm_delete = None;
            return;
        };
        let groups: Vec<String> = data
            .tunnels
            .groups_using(&tunnel.id)
            .iter()
            .map(|g| format!("group \u{201c}{}\u{201d}", g.name))
            .collect();
        let mut body = "It's removed from tunnels.toml".to_string();
        if !groups.is_empty() {
            body.push_str(&format!(" and from {}", groups.join(", ")));
        }
        body.push_str(". Your ssh config isn't touched.");
        let mut close = false;
        let response = egui::Modal::new(Id::unique("delete_tunnel"))
            .backdrop_color(p.overlay)
            .frame(
                Frame::new()
                    .fill(p.bg_dark)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::LARGE)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(412.0);
                ui.spacing_mut().item_spacing.y = 14.0;
                ui.label(
                    RichText::new(format!("Delete tunnel \u{201c}{}\u{201d}?", tunnel.name))
                        .font(fonts::semibold(17.0))
                        .color(p.text_strong),
                );
                ui.add(egui::Label::new(RichText::new(&body).size(13.0)).wrap());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new("Delete").danger().dialog()).clicked() {
                        commands.push(Command::DeleteTunnel(tunnel.id.clone()));
                        self.selected = None;
                        close = true;
                    }
                    let cancel = ui.add(Button::new("Cancel").dialog());
                    if ui.memory(|m| m.focused().is_none()) {
                        cancel.request_focus();
                    }
                    close |= cancel.clicked();
                });
            });
        if close || response.should_close() {
            self.confirm_delete = None;
        }
    }
}

/// "the name is empty" as a sentence.
pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Red lines under a field.
pub fn field_errors(ui: &mut Ui, errors: &[String]) {
    for error in errors {
        ui.label(RichText::new(error).size(12.0).color(Palette::ONE_DARK.red));
    }
}

/// Label width of the tunnel editor's rows.
const LABEL: f32 = 110.0;

/// An editor row: the label (`label_width` wide), then `add`, given the
/// label's id. `named_by_field`: the field (a dropdown, a switch) carries
/// the name itself, so the label is only painted.
pub fn editor_row(
    ui: &mut Ui,
    (label, named_by_field, label_width): (&str, bool, f32),
    add: impl FnOnce(&mut Ui, Id),
) {
    let p = &Palette::ONE_DARK;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let (rect, _) = ui.allocate_exact_size(vec2(label_width, 32.0), Sense::hover());
        let label_id = if named_by_field {
            ui.painter().text(
                pos2(rect.left(), rect.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(13.0),
                p.text,
            );
            Id::NULL
        } else {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            child.label(RichText::new(label).size(13.0)).id
        };
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            add(ui, label_id);
        });
    });
}

/// Where the fields of a forward kind listen and point (SPEC.md §10
/// *Forwards*): bind label, listen port label, target labels.
fn forward_labels(kind: ForwardKind) -> [&'static str; 4] {
    match kind {
        ForwardKind::L => [
            "Bind address",
            "Local port",
            "IP / host on remote machine",
            "Remote port",
        ],
        ForwardKind::R => [
            "Bind on server",
            "Remote port",
            "IP / host on this machine",
            "Local port",
        ],
        ForwardKind::D => ["Bind address", "Local port", "", ""],
    }
}

/// One forward as separate fields; returns whether *Remove* was clicked.
fn forward_card(
    ui: &mut Ui,
    (index, forward): (usize, &mut Forward),
    errors: &[String],
    set_aside: &mut HashMap<usize, String>,
) -> bool {
    let p = &Palette::ONE_DARK;
    const KIND: f32 = 104.0;
    const BIND: f32 = 130.0;
    const PORT: f32 = 84.0;
    let mut remove = false;
    let labels = forward_labels(forward.kind);
    Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin {
            left: 10,
            right: 10,
            top: 8,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.visuals_mut().extreme_bg_color = p.bg_dark;
            ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
            let width = ui.available_width();
            let host_width =
                (width - KIND - BIND - PORT * 2.0 - 16.0 - 12.0 - 32.0 - 8.0 * 7.0).max(80.0);
            // The column labels are real labels: the fields below are
            // named by them.
            let small = |ui: &mut Ui, text: &str, w: f32| -> Id {
                let (rect, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());
                let font = FontId::proportional(11.0);
                let (shown, _) = text::elide(ui.painter(), text, &font, w);
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                child
                    .add(
                        egui::Label::new(RichText::new(shown).font(font).color(p.dim))
                            .selectable(false),
                    )
                    .id
            };
            let mut ids = [Id::NULL; 4];
            ui.horizontal(|ui| {
                small(ui, "Kind", KIND);
                ids[0] = small(ui, labels[0], BIND);
                ids[1] = small(ui, labels[1], PORT);
                if forward.kind != ForwardKind::D {
                    ui.add_space(16.0);
                    ids[2] = small(ui, labels[2], host_width);
                    ui.add_space(12.0);
                    ids[3] = small(ui, labels[3], PORT);
                }
            });
            ui.horizontal(|ui| {
                let kinds = [ForwardKind::L, ForwardKind::R, ForwardKind::D];
                let selected = kinds.iter().position(|k| *k == forward.kind).unwrap_or(0);
                let id = Id::unique(("forward_kind", index));
                if let Some(picked) = nav::segmented(ui, id, &["L", "R", "D"], selected) {
                    forward.kind = kinds[picked];
                    // A SOCKS forward has no target: keep it aside, and
                    // bring it back when the forward is L or R again.
                    forward.target = match forward.kind {
                        ForwardKind::D => {
                            if let Some(target) = forward.target.take() {
                                set_aside.insert(index, target);
                            }
                            None
                        }
                        _ => forward
                            .target
                            .take()
                            .or_else(|| set_aside.remove(&index))
                            .or_else(|| Some(String::new())),
                    };
                }
                let used = ui.min_rect().width();
                ui.add_space((KIND - used).max(0.0));
                // Not loopback: allowed, but outlined as a warning (§7).
                let exposed = forward.is_exposed();
                ui.add(
                    TextField::new(&mut forward.bind)
                        .mono()
                        .width(BIND)
                        .warning(exposed),
                )
                .labelled_by(ids[0]);
                let mut listen = if forward.listen == 0 {
                    String::new()
                } else {
                    forward.listen.to_string()
                };
                let response = ui
                    .add(
                        TextField::new(&mut listen)
                            .mono()
                            .hint(labels[1])
                            .width(PORT)
                            .error(forward.listen == 0 && !errors.is_empty()),
                    )
                    .labelled_by(ids[1]);
                if response.changed() {
                    if listen.trim().is_empty() {
                        forward.listen = 0;
                    } else if let Ok(port) = listen.trim().parse() {
                        forward.listen = port;
                    }
                }
                if forward.kind == ForwardKind::D {
                    ui.label(
                        RichText::new("SOCKS proxy: no remote target")
                            .size(12.0)
                            .color(p.dim),
                    );
                } else {
                    let (mut host, mut port) =
                        split_target(forward.target.as_deref().unwrap_or_default());
                    icons::show(ui, icons::ARROW_RIGHT, 14.0, p.dim);
                    let host_edit = ui
                        .add(
                            TextField::new(&mut host)
                                .mono()
                                .hint("localhost")
                                .width(host_width),
                        )
                        .labelled_by(ids[2]);
                    ui.label(
                        RichText::new(":")
                            .font(FontId::monospace(13.0))
                            .color(p.dim),
                    );
                    let port_edit = ui
                        .add(TextField::new(&mut port).mono().hint(labels[3]).width(PORT))
                        .labelled_by(ids[3]);
                    if host_edit.changed() || port_edit.changed() {
                        forward.target = Some(join_target(&host, &port));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let name = format!("Remove forward {}", index + 1);
                    remove = ui
                        .add(Button::icon_only(icons::X, &name).ghost().small())
                        .clicked();
                });
            });
            if forward.is_exposed() {
                ui.horizontal_wrapped(|ui| {
                    icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                    ui.label(
                        RichText::new(format!("{}: {}", forward.bind, exposed_hint(forward.kind)))
                            .size(12.0)
                            .color(p.yellow),
                    );
                });
            }
            for error in errors {
                ui.label(RichText::new(error).size(12.0).color(p.red));
            }
        });
    remove
}

fn exposed_hint(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::R => {
            "listens on a non-loopback address of the server: other machines can reach it"
        }
        _ => "listens on a non-loopback address: other machines on your network can reach it",
    }
}

/// What the row menu asked for.
enum MenuAction {
    Command(Command),
    Edit,
    CopySsh,
    Delete,
}

/// The row's right-click menu: Start, Stop, Edit, Copy ssh command,
/// Delete… (SPEC.md §10, only existing actions).
fn tunnel_menu(
    response: &egui::Response,
    tunnel_id: &str,
    state: &TunnelState,
    status: Option<&TunnelStatus>,
) -> Option<MenuAction> {
    let manual = status.is_some_and(|s| s.owners.contains(&Owner::Manual));
    let active = !matches!(state, TunnelState::Stopped | TunnelState::Failed(_));
    let mut chosen = None;
    popup::context_menu(response, |menu| {
        if menu.item(Some(icons::PLAY), "Start", !manual || !active) {
            chosen = Some(MenuAction::Command(Command::StartTunnel(tunnel_id.into())));
        }
        if menu.item(Some(icons::SQUARE), "Stop", manual) {
            chosen = Some(MenuAction::Command(Command::StopTunnel(tunnel_id.into())));
        }
        menu.separator();
        if menu.item(None, "Edit", !active) {
            chosen = Some(MenuAction::Edit);
        }
        if menu.item(None, "Copy ssh command", true) {
            chosen = Some(MenuAction::CopySsh);
        }
        menu.separator();
        if menu.item(None, "Delete…", true) {
            chosen = Some(MenuAction::Delete);
        }
    });
    chosen
}

/// The forwards as a table; a non-loopback bind explains itself on hover.
fn forwards_table(ui: &mut Ui, tunnel: &Tunnel) {
    let p = &Palette::ONE_DARK;
    const ROW: f32 = 32.0;
    Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(4, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            let columns = [
                60.0,
                130.0,
                90.0,
                (width - 60.0 - 130.0 - 90.0 - 100.0 - 24.0).max(80.0),
                100.0,
            ];
            let cells = |ui: &mut Ui, rect: Rect, texts: [(&str, Color32, bool); 5]| {
                let mut x = rect.left() + 12.0;
                for (i, (text, color, mono)) in texts.into_iter().enumerate() {
                    let font = if mono {
                        FontId::monospace(13.0)
                    } else {
                        fonts::semibold(11.0)
                    };
                    let (shown, _) = text::elide(ui.painter(), text, &font, columns[i] - 8.0);
                    ui.painter().text(
                        pos2(x, rect.center().y),
                        Align2::LEFT_CENTER,
                        shown,
                        font,
                        color,
                    );
                    x += columns[i];
                }
            };
            let (header, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
            let [_, listen, host, port] = forward_labels(ForwardKind::L);
            cells(
                ui,
                header,
                [
                    ("KIND", p.dim, false),
                    ("BIND", p.dim, false),
                    (&listen.to_uppercase(), p.dim, false),
                    (&host.to_uppercase(), p.dim, false),
                    (&port.to_uppercase(), p.dim, false),
                ],
            );
            for forward in &tunnel.forwards {
                let (rect, response) = ui.allocate_exact_size(vec2(width, ROW), Sense::hover());
                // The kind as a chip.
                let chip = Rect::from_min_size(
                    pos2(rect.left() + 12.0, rect.center().y - 10.0),
                    vec2(24.0, 20.0),
                );
                ui.painter().rect_filled(chip, radius::CONTROL, p.selection);
                ui.painter().text(
                    chip.center(),
                    Align2::CENTER_CENTER,
                    format!("{:?}", forward.kind),
                    FontId::monospace(12.0),
                    p.text_strong,
                );
                let (host, port) = match &forward.target {
                    Some(target) => split_target(target),
                    None => ("SOCKS proxy".to_string(), "—".to_string()),
                };
                let target_color = if forward.target.is_some() {
                    p.text
                } else {
                    p.dim
                };
                let bind_color = if forward.is_exposed() {
                    p.yellow
                } else {
                    p.text
                };
                cells(
                    ui,
                    rect,
                    [
                        ("", p.text, true),
                        (&forward.bind, bind_color, true),
                        (&forward.listen.to_string(), p.text, true),
                        (&host, target_color, true),
                        (&port, target_color, true),
                    ],
                );
                if forward.is_exposed() {
                    let hint = exposed_hint(forward.kind);
                    popup::tip_below(response, |ui| {
                        ui.label(hint);
                    });
                }
            }
        });
}

/// The failure banner: what went wrong in plain words, its fix, and *Show
/// log*.
fn failure_banner(
    ui: &mut Ui,
    (tunnel, status, reason): (&Tunnel, Option<&TunnelStatus>, &str),
    ssh: Option<&SshInfo>,
    commands: &mut Vec<Command>,
    (show_log, key_fix): (&mut bool, &mut Option<KeyFix>),
) {
    let p = &Palette::ONE_DARK;
    let failure = status.and_then(|s| s.failure);
    let top = ui.cursor().top();
    marks::banner(ui, p.red_tint, |ui| {
        let title = match failure {
            Some(FailureKind::HostKeyChanged) => "The server's host key changed",
            Some(FailureKind::HostKeyUnknown) => "A host key on the way isn't stored yet",
            Some(FailureKind::Auth) => "The key was not accepted",
            _ => "ssh stopped",
        };
        ui.label(
            RichText::new(title)
                .font(fonts::semibold(14.0))
                .color(p.red),
        );
        let body = match failure {
            Some(FailureKind::HostKeyChanged) => Some(
                "Either the server was reinstalled, or someone is intercepting the connection. \
                 sshelf won't connect until you review the new key."
                    .to_string(),
            ),
            Some(FailureKind::HostKeyUnknown) => {
                Some("Retry checks every host on the way and asks you about each new key.".into())
            }
            Some(FailureKind::Auth) => Some(hints::auth_hint(
                ssh.and_then(|s| s.agent.as_ref()),
                ssh.and_then(|s| s.service.as_ref())
                    .and_then(|s| s.as_ref().ok()),
            )),
            _ => hints::failure_hint(reason).map(String::from),
        };
        ui.label(
            RichText::new(reason)
                .font(FontId::monospace(12.0))
                .color(p.text_strong),
        );
        if let Some(body) = body {
            ui.add(egui::Label::new(RichText::new(body).size(13.0).color(p.text)).wrap());
        }
        if let Some(text) = reconnect_text(ui.ctx(), status) {
            ui.label(RichText::new(text).size(13.0).color(p.yellow));
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // The health banner's fixes for "no key accepted" (SPEC.md §10).
            if failure == Some(FailureKind::Auth) {
                let service_stopped = ssh
                    .and_then(|s| s.service.as_ref())
                    .and_then(|s| s.as_ref().ok())
                    .is_some_and(|s| !s.is_running());
                let mut load = Button::new("Load key into agent")
                    .small()
                    .tip("Run ssh-add for the host's key in a terminal");
                if service_stopped {
                    load = load.tip("The ssh-agent service is not running");
                }
                if ui.add_enabled(!service_stopped, load).clicked() {
                    *key_fix = Some(KeyFix::LoadKey(tunnel.host.clone()));
                }
                let generate = ui.add(
                    Button::new("Generate key…")
                        .icon(icons::KEY_ROUND)
                        .small()
                        .tip("Create a key pair for this host"),
                );
                if tour::targets::clicked(ui.ctx(), Target::BannerGenerateKey, &generate) {
                    *key_fix = Some(KeyFix::GenerateKey(tunnel.host.clone()));
                }
            }
            if failure == Some(FailureKind::HostKeyChanged) {
                let review = Button::new("Review changed key…")
                    .danger()
                    .tip("Compare the stored key with the one the server presents");
                if ui.add(review).clicked() {
                    commands.push(Command::ReviewChangedKey(tunnel.id.clone()));
                }
            }
            if ui.add(Button::new("Show log")).clicked() {
                *show_log = true;
            }
        });
    });
    let rect = ui.min_rect().with_min_y(top);
    tour::targets::register(ui.ctx(), Target::FailureBanner, rect);
}

/// A fix the failure banner asks the app for: the key window and the agent
/// live outside this view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFix {
    /// *Generate key…* for this host.
    GenerateKey(String),
    /// *Load key into agent* for this host's key.
    LoadKey(String),
}

/// The log, newest at the bottom, each line with its time; debug lines on
/// request.
fn log(
    ui: &mut Ui,
    status: Option<&TunnelStatus>,
    state: &TunnelState,
    show_debug: &mut bool,
    scroll_to: bool,
) {
    let p = &Palette::ONE_DARK;
    let header = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new("Log")
                .font(fonts::semibold(13.0))
                .color(p.text_strong),
        );
        ui.label(RichText::new("last 200 lines").size(12.0).color(p.dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new("show debug").size(12.0).color(p.dim));
            switch(ui, show_debug, "Show debug lines");
        });
    });
    if scroll_to {
        ui.scroll_to_rect(header.response.rect, Some(Align::Min));
    }
    let Some(status) = status else {
        ui.label(
            RichText::new("Not started in this session.")
                .size(13.0)
                .color(p.dim),
        );
        return;
    };
    let failed = matches!(state, TunnelState::Failed(_));
    let last = status.log.iter().rposition(|l| !l.debug);
    Frame::new()
        .fill(p.bg_dark)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("tunnel_log")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (index, line) in status.log.iter().enumerate() {
                        if line.debug && !*show_debug {
                            continue;
                        }
                        let time =
                            chrono::DateTime::<chrono::Local>::from(line.at).format("%H:%M:%S");
                        let color = if line.debug {
                            p.comment
                        } else if failed && Some(index) == last {
                            p.red
                        } else {
                            p.dim
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{time}  {}", line.text))
                                    .font(FontId::monospace(12.0))
                                    .color(color),
                            )
                            .wrap(),
                        );
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshconf::{ConfigSet, ResolveOptions};
    use sshelf_core::core::ConfigState;
    use sshelf_core::supervisor::{LogLine, Reconnect};

    use super::*;
    use crate::test_util;

    struct Fixture {
        config: Arc<ConfigSnapshot>,
        data: Arc<AppData>,
        states: Arc<Snapshot>,
        ssh: Option<SshInfo>,
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
            auto_reconnect: false,
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
        let running_since = (state == TunnelState::Running)
            .then(|| Instant::now() - Duration::from_secs(2 * 3600 + 14 * 60));
        let status = TunnelStatus {
            state,
            owners: BTreeSet::from([Owner::Manual]),
            log: VecDeque::from([
                LogLine {
                    text: "debug1: hidden".into(),
                    debug: true,
                    at: SystemTime::UNIX_EPOCH,
                },
                LogLine {
                    text: "Authenticated to bastion".into(),
                    debug: false,
                    at: SystemTime::UNIX_EPOCH,
                },
            ]),
            pid: Some(1),
            failure: None,
            reconnect: None,
            running_since,
        };
        Fixture {
            config: Arc::new(ConfigSnapshot {
                generation: 1,
                path,
                state: ConfigState::Loaded { set, hosts },
                read_only: Vec::new(),
            }),
            data: Arc::new(data),
            states: Arc::new(Snapshot {
                tunnels: [("t-1".to_string(), status)].into(),
            }),
            ssh: None,
        }
    }

    fn harness(f: Fixture, view: TunnelsView) -> Harness<'static, (TunnelsView, Vec<Command>)> {
        test_util::harness()
            .with_size([1200.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (TunnelsView, Vec<Command>)| {
                    commands.extend(view.show(
                        ui,
                        Some(&f.config),
                        Some(&f.data),
                        Some(&f.states),
                        f.ssh.as_ref(),
                    ));
                },
                (view, Vec::new()),
            )
    }

    fn selected() -> TunnelsView {
        TunnelsView {
            selected: Some("t-1".into()),
            ..TunnelsView::default()
        }
    }

    #[test]
    fn changed_host_key_offers_a_review() {
        let mut f = fixture(TunnelState::Failed("Host key verification failed.".into()));
        let mut states = (*f.states).clone();
        states.tunnels.get_mut("t-1").unwrap().failure = Some(FailureKind::HostKeyChanged);
        f.states = Arc::new(states);
        let mut harness = harness(f, selected());
        harness.step();
        harness.get_by_label("The server's host key changed");
        // The row and the header offer Review…, the banner the full review.
        assert_eq!(harness.get_all_by_label("Review…").count(), 2);
        harness.get_by_label("Review changed key…").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::ReviewChangedKey("t-1".into())]);
    }

    #[test]
    fn auth_failure_explains_the_agent() {
        let mut f = fixture(TunnelState::Failed(
            "bastion: Permission denied (publickey).".into(),
        ));
        let mut states = (*f.states).clone();
        states.tunnels.get_mut("t-1").unwrap().failure = Some(FailureKind::Auth);
        f.states = Arc::new(states);
        f.ssh = Some(SshInfo {
            tools: Err("not needed".into()),
            version: None,
            agent: Some(sshelf_core::ssh::AgentStatus::Keys(0)),
            service: None,
            terminal: None,
            tray: Ok(true),
        });
        let mut harness = harness(f, selected());
        harness.step();
        harness.get_by_label("Failed · key not accepted");
        harness.get_by_label_contains("ssh-add");
        harness.get_by_label_contains("no keys loaded");
        // The health banner's fixes, for the tunnel's host.
        harness.get_by_label("Generate key…").click();
        harness.step();
        assert_eq!(
            harness.state_mut().0.take_key_fix(),
            Some(KeyFix::GenerateKey("bastion".into()))
        );
        harness.get_by_label("Load key into agent").click();
        harness.step();
        assert_eq!(
            harness.state_mut().0.take_key_fix(),
            Some(KeyFix::LoadKey("bastion".into()))
        );
    }

    #[test]
    fn running_tunnel_shows_uptime_owners_and_a_timed_log() {
        let mut harness = harness(fixture(TunnelState::Running), selected());
        harness.step();
        harness.get_by_label("Running · 2 h 14 m · L 5432");
        harness.get_by_label("· owners: manual start");
        harness.get_by_label_contains("  Authenticated to bastion");
        assert!(harness.query_by_label_contains("debug1: hidden").is_none());
        assert!(harness.get_by_label("Edit").accesskit_node().is_disabled());

        harness.get_by_label("Show debug lines").click();
        harness.step();
        harness.step();
        harness.get_by_label_contains("debug1: hidden");
    }

    #[test]
    fn failed_tunnel_offers_retry_and_a_hint() {
        let reason = "Error: remote port forwarding failed for listen port 6443";
        let mut harness = harness(fixture(TunnelState::Failed(reason.into())), selected());
        harness.step();
        harness.get_by_label("ssh stopped");
        harness.get_by_label_contains("already in use there");
        harness.get_all_by_label("Retry").next().unwrap().click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartTunnel("t-1".into())]);
    }

    #[test]
    fn stop_releases_the_manual_start() {
        let mut harness = harness(fixture(TunnelState::Running), selected());
        harness.step();
        harness.get_all_by_label("Stop").next().unwrap().click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StopTunnel("t-1".into())]);
    }

    #[test]
    fn a_group_tunnel_keeps_start() {
        let mut f = fixture(TunnelState::Running);
        let mut states = (*f.states).clone();
        states.tunnels.get_mut("t-1").unwrap().owners = BTreeSet::from([Owner::Group("g".into())]);
        f.states = Arc::new(states);
        let mut harness = harness(f, TunnelsView::default());
        harness.step();
        harness.get_by_label("Running · group g");
        harness.get_by_label("Start").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartTunnel("t-1".into())]);
    }

    #[test]
    fn a_pending_reconnect_is_shown_and_the_option_is_saved() {
        let mut f = fixture(TunnelState::Failed(
            "Connection to bastion closed by remote host.".into(),
        ));
        let mut states = (*f.states).clone();
        states.tunnels.get_mut("t-1").unwrap().reconnect = Some(Reconnect {
            attempt: 2,
            at: SystemTime::now() + Duration::from_secs(4),
        });
        f.states = Arc::new(states);
        let mut harness = harness(f, selected());
        harness.step();
        harness
            .get_all_by_label_contains("(attempt 2)")
            .next()
            .unwrap();

        let mut draft = tunnel();
        draft.name = "API".into();
        harness.state_mut().0.editor = Some(draft);
        harness.step();
        harness.get_by_label("Reconnect automatically").click();
        harness.step();
        harness.get_by_label("Save").click();
        harness.step();
        let Some(Command::SaveTunnel(saved)) = harness.state().1.last() else {
            panic!("{:?}", harness.state().1);
        };
        assert!(saved.auto_reconnect);
    }

    #[test]
    fn editor_validates_and_warns_about_exposed_binds() {
        let f = fixture(TunnelState::Stopped);
        let mut view = TunnelsView::default();
        view.new_tunnel("bastion", &f.data.tunnels);
        let mut harness = harness(f, view);
        harness.step();
        harness.get_by_label("The name is empty");
        harness.get_by_label("The port must be 1–65535");
        harness.get_by_label_contains("ssh -v -N");

        let draft = harness.state_mut().0.editor.as_mut().unwrap();
        draft.name = "API".into();
        draft.forwards[0].listen = 6443;
        draft.forwards[0].target = Some("localhost:6443".into());
        draft.forwards[0].bind = "0.0.0.0".into();
        harness.step();
        harness.step();
        assert!(harness.query_by_label("The name is empty").is_none());
        harness.get_by_label_contains("0.0.0.0: listens on a non-loopback address");

        harness.get_by_label("Save and start").click();
        harness.step();
        let commands = &harness.state().1;
        let [Command::SaveTunnel(saved), Command::StartTunnel(started)] = commands.as_slice()
        else {
            panic!("{commands:?}");
        };
        assert_eq!(saved.host, "bastion");
        assert!(saved.id.starts_with("t-"));
        assert_eq!(started, &saved.id);
    }

    #[test]
    fn the_host_is_picked_with_a_search() {
        let f = fixture(TunnelState::Stopped);
        let mut view = TunnelsView::default();
        view.new_tunnel("", &f.data.tunnels);
        let mut harness = harness(f, view);
        harness.step();
        harness.get_by_label("Host").click();
        harness.step();
        harness.step();
        harness.event(egui::Event::Text("we".into()));
        harness.step();
        harness.step();
        assert!(harness.query_by_label("bastion").is_none(), "filtered out");
        harness.key_press(egui::Key::Enter);
        harness.step();
        harness.step();
        assert_eq!(harness.state().0.editor.as_ref().unwrap().host, "web");
    }

    #[test]
    fn deleting_asks_first_with_cancel_as_the_main_button() {
        let mut harness = harness(fixture(TunnelState::Stopped), selected());
        harness.step();
        harness.get_by_label("Delete…").click();
        harness.step();
        harness.step();
        harness.get_by_label("Delete tunnel \u{201c}Prod DB\u{201d}?");
        harness.get_by_label("Cancel").click();
        harness.step();
        harness.step();
        assert!(harness.state().1.is_empty());
        harness.get_by_label("Delete…").click();
        harness.step();
        harness.step();
        harness.get_by_label("Delete").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::DeleteTunnel("t-1".into())]);
    }

    #[test]
    fn the_row_menu_starts_a_tunnel() {
        let mut harness = harness(fixture(TunnelState::Stopped), TunnelsView::default());
        harness.step();
        // On the name itself, as people do.
        harness
            .get_all_by_label("Prod DB")
            .last()
            .unwrap()
            .click_secondary();
        harness.step();
        harness.step();
        // The row's own button is the first "Start"; the menu's is last.
        harness.get_all_by_label("Start").last().unwrap().click();
        harness.step();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartTunnel("t-1".into())]);
    }

    #[test]
    fn switching_a_forward_kind_keeps_the_target() {
        let f = fixture(TunnelState::Stopped);
        let view = TunnelsView {
            editor: Some(tunnel()),
            ..TunnelsView::default()
        };
        let mut harness = harness(f, view);
        harness.step();
        harness.get_by_label("D").click();
        harness.step();
        harness.step();
        let forward = |h: &Harness<'_, (TunnelsView, Vec<Command>)>| {
            h.state().0.editor.as_ref().unwrap().forwards[0].clone()
        };
        assert_eq!(forward(&harness).kind, ForwardKind::D);
        assert_eq!(forward(&harness).target, None);
        harness.get_by_label("L").click();
        harness.step();
        harness.step();
        assert_eq!(forward(&harness).target.as_deref(), Some("db:5432"));
        assert_eq!(forward(&harness).listen, 5432);
    }

    #[test]
    fn targets_split_and_join() {
        assert_eq!(split_target("db:5432"), ("db".into(), "5432".into()));
        assert_eq!(split_target("[::1]:80"), ("::1".into(), "80".into()));
        assert_eq!(join_target("::1", "80"), "[::1]:80");
        assert_eq!(join_target(" db ", "5432"), "db:5432");
    }
}
