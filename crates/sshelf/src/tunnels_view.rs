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
use sshelf_core::kubectl::KubectlInfo;
use sshelf_core::redact::DOTS;
use sshelf_core::supervisor::{FailureKind, Owner, Snapshot, TunnelState, TunnelStatus};
use sshelf_core::tunnels::{
    DEFAULT_BIND, Forward, ForwardKind, ResourceKind, Tunnel, TunnelType, TunnelsFile,
};

use crate::theme::{Palette, radius, timing};
use crate::tour::steps;
use crate::tour::targets::Target;
use crate::widgets::button::{self, Button};
use crate::widgets::dropdown::{Choice, Dropdown};
use crate::widgets::field::TextField;
use crate::widgets::switch::switch;
use crate::widgets::{marks, masked, nav, popup, text};
use crate::{fonts, icons, kube_editor, motion, privacy, tour};

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
    /// The list's type filter, kept for the session.
    filter: TypeFilter,
    /// *Settings › kubectl* was clicked: the app opens Settings.
    open_settings: bool,
    /// The type a new tunnel starts as: the one used last this session.
    last_type: TunnelType,
    /// The kubectl editor's lookups.
    lookups: kube_editor::Lookups,
    /// *Pick another pod*: the editor scrolls to *Forward to*.
    scroll_to_resource: bool,
    /// A kubectl tunnel just saved and started: its toast once it runs.
    announce: Option<String>,
    /// That toast, for the app: the text and the address to copy.
    running_toast: Option<(String, String)>,
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
            Owner::NeededBy(id) => format!("needed by {}", tunnel_name(file, id)),
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
            Owner::NeededBy(id) => format!("needed by {}", tunnel_name(file, id)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A tunnel's name, or its id when it is gone.
pub fn tunnel_name(file: &TunnelsFile, id: &str) -> String {
    file.tunnel(id)
        .map_or_else(|| id.to_string(), |t| t.name.clone())
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

/// "L 6443": a forward's kind and listen port (masked in streamer mode).
fn forward_short(forward: &Forward, masked: bool) -> String {
    if masked {
        format!("{:?} {DOTS}", forward.kind)
    } else {
        format!("{:?} {}", forward.kind, forward.listen)
    }
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

/// `<context> / <namespace>` for a kubectl tunnel ("current context" when
/// none is pinned; the context masked in streamer mode): the groups' host
/// column and the group editor's notes.
pub fn kube_place(tunnel: &Tunnel, masked: bool) -> String {
    let kube = &tunnel.kube;
    let context = match (kube.context.as_str(), masked) {
        ("", _) => "current context",
        (_, true) => DOTS,
        (context, false) => context,
    };
    format!("{context} / {}", kube.namespace)
}

/// `<context> / <namespace> · <kind>/<name>` for a kubectl tunnel.
fn kube_where(tunnel: &Tunnel, masked: bool) -> String {
    format!("{} · {}", kube_place(tunnel, masked), tunnel.kube.resource)
}

/// The list's second line for a kubectl tunnel: where it forwards to and its
/// first local port.
fn kube_short(tunnel: &Tunnel, masked: bool) -> String {
    let place = kube_where(tunnel, masked);
    match tunnel.kube.forwards.first() {
        Some(_) if masked => format!("{place} · {DOTS}"),
        Some(f) => format!("{place} · {}", f.listen),
        None => place,
    }
}

/// A failure in a few words, for list rows: kubectl's kinds (SPEC.md §7
/// *kubectl tunnels*, *Failures*); `None` for the rest.
fn kube_why(tunnel: &Tunnel, failure: Option<FailureKind>, masked: bool) -> Option<String> {
    let kind = tunnel.kube.resource.kind.word();
    let context = match (tunnel.kube.context.as_str(), masked) {
        ("", _) => "the current context",
        (_, true) => DOTS,
        (context, false) => context,
    };
    Some(match failure? {
        FailureKind::ResourceGone => format!("{kind} not found"),
        FailureKind::NotLoggedIn => format!("not logged in to {context}"),
        FailureKind::ClusterUnreachable => "can't reach the cluster".into(),
        FailureKind::PortInUse => "a local port is in use".into(),
        FailureKind::NoContext => "context not in your kubeconfig".into(),
        FailureKind::KubectlMissing => "kubectl not found".into(),
        _ => return None,
    })
}

/// The command as the supervisor runs it (the binary shown by its name).
fn command_line(tunnel: &Tunnel) -> String {
    let args: Vec<String> = tunnel
        .args()
        .into_iter()
        .map(|a| {
            if a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a
            }
        })
        .collect();
    format!("{} {}", tunnel.kind.program(), args.join(" "))
}

/// What a list row offers for its state (SPEC.md §10, Tunnel-States).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Start,
    Stop,
    Cancel,
    Retry,
    Review,
    /// Running for a group only: a manual owner keeps it up once the group
    /// stops. Offered in the detail and the menu, not on the row (where it
    /// would read as *Start* on a running tunnel).
    Keep,
}

impl Action {
    fn of(state: &TunnelState, status: Option<&TunnelStatus>) -> Option<Action> {
        let manual = status.is_some_and(|s| s.owners.contains(&Owner::Manual));
        match state {
            TunnelState::Starting if manual => Some(Action::Cancel),
            TunnelState::Starting | TunnelState::Stopping => None,
            TunnelState::Running if manual => Some(Action::Stop),
            TunnelState::Running => Some(Action::Keep),
            TunnelState::Stopped => Some(Action::Start),
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
            Action::Keep => "Keep running",
        }
    }

    fn icon(self) -> Option<char> {
        match self {
            Action::Start => Some(icons::PLAY),
            Action::Stop => Some(icons::SQUARE),
            Action::Retry => Some(icons::ROTATE_CW),
            Action::Cancel | Action::Review | Action::Keep => None,
        }
    }

    fn command(self, id: &str) -> Command {
        match self {
            Action::Start | Action::Retry | Action::Keep => Command::StartTunnel(id.to_string()),
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

/// Why the row's `action` can't run: a kubectl tunnel can't start while
/// kubectl isn't found (SPEC.md §10 *kubectl tunnels*).
fn blocked(tunnel: &Tunnel, action: Option<Action>, kubectl_missing: bool) -> Option<&'static str> {
    let starts = matches!(action, Some(Action::Start | Action::Retry));
    (starts && kubectl_missing && tunnel.is_kubectl()).then_some(KUBECTL_MISSING)
}

/// The reason kubectl tunnels can't start.
const KUBECTL_MISSING: &str = "kubectl wasn't found (Settings › kubectl)";

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

/// A row's second line and its colour. `kubectl_missing`: kubectl wasn't
/// found, so a kubectl tunnel can't start.
fn sub_line(
    ctx: &egui::Context,
    tunnel: &Tunnel,
    status: Option<&TunnelStatus>,
    file: &TunnelsFile,
    kubectl_missing: bool,
) -> (String, Color32) {
    let p = &Palette::ONE_DARK;
    let state = status.map_or(TunnelState::Stopped, |s| s.state.clone());
    let masked = privacy::get(ctx).on;
    if tunnel.is_kubectl() {
        return kube_sub_line(ctx, tunnel, status, file, kubectl_missing);
    }
    let forward = tunnel.forwards.first().map(|f| forward_short(f, masked));
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

/// [`sub_line`] for a kubectl tunnel (SPEC.md §10 *kubectl tunnels*: where
/// it forwards to, else its state).
fn kube_sub_line(
    ctx: &egui::Context,
    tunnel: &Tunnel,
    status: Option<&TunnelStatus>,
    file: &TunnelsFile,
    kubectl_missing: bool,
) -> (String, Color32) {
    let p = &Palette::ONE_DARK;
    let state = status.map_or(TunnelState::Stopped, |s| s.state.clone());
    let masked = privacy::get(ctx).on;
    match &state {
        TunnelState::Starting => (
            "Starting… waiting for \u{201c}Forwarding from …\u{201d}".into(),
            p.yellow,
        ),
        TunnelState::Stopping => ("Stopping…".into(), p.dim),
        TunnelState::Stopped if kubectl_missing => ("kubectl not found".into(), p.yellow),
        TunnelState::Stopped => (kube_short(tunnel, masked), p.dim),
        TunnelState::Running => {
            let status = status.expect("running has a status");
            if status.owners.contains(&Owner::Manual) {
                (kube_short(tunnel, masked), p.dim)
            } else {
                let owners: Vec<&Owner> = status.owners.iter().collect();
                (format!("Running · {}", owners_text(&owners, file)), p.dim)
            }
        }
        TunnelState::Failed(reason) => {
            if let Some(text) = reconnect_text(ctx, status) {
                return (text, p.yellow);
            }
            let why = kube_why(tunnel, status.and_then(|s| s.failure), masked)
                .unwrap_or_else(|| reason.clone());
            (format!("Failed · {why}"), p.red)
        }
    }
}

/// Which tunnels the list shows (the *All / ssh / kubectl* filter).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TypeFilter {
    #[default]
    All,
    Ssh,
    Kubectl,
}

impl TypeFilter {
    const ALL: [TypeFilter; 3] = [TypeFilter::All, TypeFilter::Ssh, TypeFilter::Kubectl];

    fn shows(self, tunnel: &Tunnel) -> bool {
        match self {
            TypeFilter::All => true,
            TypeFilter::Ssh => !tunnel.is_kubectl(),
            TypeFilter::Kubectl => tunnel.is_kubectl(),
        }
    }
}

/// A fix the kubectl failure banner asked for.
enum KubeFix {
    /// *Edit*, *Pick another pod*: the editor.
    Edit,
    /// *Settings › kubectl*.
    Settings,
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

    /// *Settings › kubectl* was clicked, once.
    pub fn take_open_settings(&mut self) -> bool {
        std::mem::take(&mut self.open_settings)
    }

    /// "web api is running on" and its `127.0.0.1:18080`, once (a kubectl
    /// tunnel saved and started is up).
    pub fn take_running_toast(&mut self) -> Option<(String, String)> {
        self.running_toast.take()
    }

    /// Core answered a kubectl lookup.
    pub fn on_lookup(
        &mut self,
        query: sshelf_core::kubectl::KubeQuery,
        result: Result<Vec<sshelf_core::kubectl::KubeItem>, String>,
    ) {
        self.lookups.on_result(query, result);
    }

    /// The editor is open.
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub fn close_editor(&mut self) {
        self.editor = None;
        self.set_aside.clear();
        self.lookups.clear();
    }

    /// The draft in the editor (the tour fills it in).
    pub fn draft_mut(&mut self) -> Option<&mut Tunnel> {
        self.editor.as_mut()
    }

    /// Opens the editor for a new ssh tunnel to `host` (host detail "New
    /// tunnel" always starts as ssh with its host).
    pub fn new_tunnel(&mut self, host: &str, file: &TunnelsFile) {
        self.selected = None;
        self.confirm_delete = None;
        self.lookups.clear();
        self.editor = Some(Tunnel::ssh(
            &file.new_tunnel_id(),
            "",
            host,
            vec![new_forward()],
        ));
    }

    /// The list's *New tunnel*: the type used last this session (ssh at
    /// first).
    fn new_tunnel_of_last_type(&mut self, file: &TunnelsFile) {
        self.new_tunnel("", file);
        if let Some(draft) = &mut self.editor {
            kube_editor::switch_type(draft, self.last_type);
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        config: Option<&ConfigSnapshot>,
        data: Option<&AppData>,
        states: Option<&Snapshot>,
        (ssh, kubectl): (Option<&SshInfo>, Option<&KubectlInfo>),
    ) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let Some(data) = data else {
            ui.spinner();
            return commands;
        };
        // Only once detection says so: no flash of "not found" at start.
        let kubectl_missing = kubectl.is_some_and(|k| k.kubectl.is_err());
        let empty = Snapshot::default();
        let states = states.unwrap_or(&empty);
        // A kubectl tunnel saved and started: say where it runs once it does.
        if let Some(id) = self.announce.clone() {
            match states.state(&id) {
                TunnelState::Running => {
                    self.announce = None;
                    if let Some(tunnel) = data.tunnels.tunnel(&id)
                        && let Some((address, port)) = tunnel.local_ports().first().cloned()
                    {
                        self.running_toast = Some((
                            format!("{} is running on", tunnel.name),
                            format!("{address}:{port}"),
                        ));
                    }
                }
                TunnelState::Failed(_) | TunnelState::Stopped
                    if states.tunnels.contains_key(&id) =>
                {
                    self.announce = None;
                }
                _ => {}
            }
        }
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
                self.list(ui, data, states, kubectl_missing, &mut commands);
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
                    self.editor(ui, (data, hosts), (kubectl, kubectl_missing), &mut commands);
                } else if let Some(tunnel) = self
                    .selected
                    .as_ref()
                    .and_then(|id| data.tunnels.tunnel(id))
                    .cloned()
                {
                    self.detail(
                        ui,
                        &tunnel,
                        (data, states),
                        (ssh, kubectl_missing),
                        &mut commands,
                    );
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
        kubectl_missing: bool,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let all = &data.tunnels.tunnels;
        let running = all
            .iter()
            .filter(|t| states.state(&t.id) == TunnelState::Running)
            .count();
        // The type filter, only while both types exist.
        let both = all.iter().any(Tunnel::is_kubectl) && all.iter().any(|t| !t.is_kubectl());
        if !both {
            self.filter = TypeFilter::All;
        }
        let tunnels: Vec<&Tunnel> = all.iter().filter(|t| self.filter.shows(t)).collect();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Tunnels")
                    .font(fonts::semibold(16.0))
                    .color(p.text_strong),
            );
            ui.label(
                RichText::new(format!("{running} of {} running", all.len()))
                    .size(13.0)
                    .color(p.dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let response = ui.add(Button::icon_only(icons::PLUS, "New tunnel"));
                if tour::targets::clicked(ui.ctx(), Target::NewTunnel, &response) {
                    self.new_tunnel_of_last_type(&data.tunnels);
                }
            });
        });
        if both {
            ui.add_space(8.0);
            let at = TypeFilter::ALL.iter().position(|f| *f == self.filter);
            if let Some(picked) = nav::segmented(
                ui,
                Id::unique("tunnel_type_filter"),
                &["All", "ssh", "kubectl"],
                at.unwrap_or(0),
            ) {
                self.filter = TypeFilter::ALL[picked];
            }
        }
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
                for (index, &tunnel) in tunnels.iter().enumerate() {
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
                    marks::type_mark(&mut row, tunnel.kind, Some(24.0));
                    mark(&mut row, &state);
                    let action = Action::of(&state, status).filter(|a| *a != Action::Keep);
                    let blocked = blocked(tunnel, action, kubectl_missing);
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
                            let (line, color) =
                                sub_line(ui.ctx(), tunnel, status, &data.tunnels, kubectl_missing);
                            // Clicks go to the row: no tokens to peek here.
                            let line = privacy::get(ui.ctx()).plain(&line).into_owned();
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
                                .then_some("Started by a group; Start also keeps it up on its own")
                                .or(blocked);
                            let response =
                                ui.add_enabled(blocked.is_none(), action.button(true, tip));
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
                    let startable = !(kubectl_missing && tunnel.is_kubectl());
                    if let Some(chosen) =
                        tunnel_menu(&response, (&tunnel.id, startable), &state, status)
                    {
                        match chosen {
                            MenuAction::Command(command) => commands.push(*command),
                            MenuAction::Edit => {
                                self.select(&tunnel.id);
                                self.editor = Some(tunnel.clone());
                            }
                            MenuAction::CopyCommand => ui.ctx().copy_text(command_line(tunnel)),
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

    fn detail(
        &mut self,
        ui: &mut Ui,
        tunnel: &Tunnel,
        (data, states): (&AppData, &Snapshot),
        (ssh, kubectl_missing): (Option<&SshInfo>, bool),
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
                    marks::type_mark(ui, tunnel.kind, Some(28.0));
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
                    if tunnel.is_kubectl() {
                        kube_line(ui, tunnel);
                    } else {
                        ui.label(RichText::new("via").size(13.0).color(p.dim));
                        let host = privacy::get(ui.ctx()).alias_text(&tunnel.host).into_owned();
                        if button::link(ui, &host, 13.0).clicked() {
                            self.open_host = Some(tunnel.host.clone());
                        }
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
                    let blocked = blocked(tunnel, Some(action), kubectl_missing);
                    let tip = (action == Action::Keep)
                        .then_some("Runs for a group; this keeps it up when the group stops")
                        .or(blocked);
                    if ui
                        .add_enabled(blocked.is_none(), action.button(false, tip))
                        .clicked()
                    {
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
            if tunnel.is_kubectl() {
                match kube_failure_banner(
                    ui,
                    (tunnel, status, reason),
                    commands,
                    &mut self.show_log,
                ) {
                    Some(KubeFix::Edit) => {
                        self.editor = Some(tunnel.clone());
                        self.scroll_to_resource = true;
                    }
                    Some(KubeFix::Settings) => self.open_settings = true,
                    None => {}
                }
            } else {
                failure_banner(
                    ui,
                    (tunnel, status, reason),
                    ssh,
                    commands,
                    (&mut self.show_log, &mut self.key_fix),
                );
            }
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
        if tunnel.is_kubectl() {
            kube_forwards_table(ui, tunnel);
        } else {
            forwards_table(ui, tunnel);
        }
        let scroll_to_log = std::mem::take(&mut self.show_log);
        log(ui, status, &state, &mut self.show_debug, scroll_to_log);
    }

    fn editor(
        &mut self,
        ui: &mut Ui,
        (data, hosts): (&AppData, &HostList),
        (kubectl, kubectl_missing): (Option<&KubectlInfo>, bool),
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let Some(draft) = &mut self.editor else {
            return;
        };
        let is_new = data.tunnels.tunnel(&draft.id).is_none();
        let mut problems = draft.validate();
        if !draft.is_kubectl() && !draft.host.is_empty() && hosts.find(&draft.host).is_none() {
            problems.push(format!("no host `{}` in the ssh config", draft.host));
        }
        // kubectl: what is still missing, in short words (the footer).
        let missing = if draft.is_kubectl() {
            draft.missing()
        } else {
            Vec::new()
        };
        let missing_text =
            (!missing.is_empty()).then(|| format!("Missing: {}", missing.join(", ")));
        let in_tour = kubectl.is_some_and(|k| {
            k.kubectl.as_ref().err().map(String::as_str) == Some(sshelf_core::kubectl::NOT_IN_TOUR)
        });
        let last_type = &mut self.last_type;
        let open_settings = &mut self.open_settings;
        let lookups = &mut self.lookups;
        let scroll_to_resource = std::mem::take(&mut self.scroll_to_resource);
        // Local ports of the other tunnels: suggestions skip them.
        let taken: Vec<u16> = data
            .tunnels
            .tunnels
            .iter()
            .filter(|t| t.id != draft.id)
            .flat_map(Tunnel::listen_ports)
            .collect();
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
                    let why = missing_text
                        .clone()
                        .or_else(|| problems.first().map(|p| capitalized(p)));
                    // A kubectl tunnel can be saved without kubectl, not started.
                    let no_start =
                        (draft.is_kubectl() && kubectl_missing).then_some(KUBECTL_MISSING);
                    let mut save_button = Button::new("Save").primary();
                    let mut start_button = Button::new("Save and start");
                    if let Some(why) = &why {
                        save_button = save_button.tip(why);
                        start_button = start_button.tip(why);
                    }
                    if let Some(why) = no_start {
                        start_button = start_button.tip(why);
                    }
                    let response = ui.add_enabled(ok, save_button);
                    if tour::targets::clicked(ui.ctx(), Target::TunnelSave, &response) && ok {
                        save = Some(false);
                    }
                    if ui
                        .add_enabled(ok && no_start.is_none(), start_button)
                        .clicked()
                    {
                        save = Some(true);
                    }
                    close = ui.add(Button::new("Cancel").ghost()).clicked();
                    if let Some(missing) = &missing_text {
                        ui.label(RichText::new(missing).size(12.0).color(p.dim));
                    } else if !problems.is_empty() {
                        let count = match problems.len() {
                            1 => "1 problem".to_string(),
                            n => format!("{n} problems"),
                        };
                        ui.label(RichText::new(count).size(12.0).color(p.red));
                    } else if draft.is_kubectl() {
                        ui.label(RichText::new("Ready").size(12.0).color(p.green));
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
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if is_new { "New tunnel" } else { "Edit tunnel" })
                            .font(fonts::semibold(20.0))
                            .color(p.text_strong),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let kinds = [TunnelType::Ssh, TunnelType::Kubectl];
                        let segments = [
                            nav::Segment {
                                icon: Some((icons::TERMINAL, p.dim)),
                                ..nav::Segment::new("ssh")
                            },
                            nav::Segment {
                                icon: Some((icons::SHIP_WHEEL, p.blue)),
                                // The tour stays ssh-only (its sandbox has
                                // no kubectl).
                                disabled: in_tour.then_some(sshelf_core::kubectl::NOT_IN_TOUR),
                                ..nav::Segment::new("kubectl")
                            },
                        ];
                        let at = kinds.iter().position(|k| *k == draft.kind).unwrap_or(0);
                        if let (Some(picked), _) =
                            nav::segmented_with(ui, Id::unique("tunnel_type"), &segments, at)
                        {
                            kube_editor::switch_type(draft, kinds[picked]);
                            *last_type = kinds[picked];
                        }
                    });
                });
                if draft.is_kubectl() && kubectl_missing {
                    marks::banner(ui, p.yellow_tint, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                            ui.label(
                                RichText::new(
                                    "kubectl wasn't found, so this tunnel can be saved but not \
                                     started.",
                                )
                                .color(p.yellow),
                            );
                            if button::link(ui, "Settings › kubectl", 13.0).clicked() {
                                *open_settings = true;
                            }
                        });
                    });
                }
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
                if draft.is_kubectl() {
                    let env = kube_editor::Env {
                        info: kubectl,
                        taken: taken.clone(),
                        problems: &problems,
                        scroll_to_resource,
                    };
                    kube_editor::form(ui, draft, lookups, &env, commands);
                    return;
                }
                editor_row(ui, ("Host", true, LABEL), |ui, _| {
                    let mut errors = errors_for("no host");
                    // Hidden aliases: "host N" here too (and in the search).
                    let privacy = privacy::get(ui.ctx());
                    let shown: Vec<String> = hosts
                        .hosts
                        .iter()
                        .map(|h| privacy.alias_text(h.primary()).into_owned())
                        .collect();
                    let choices: Vec<Choice<usize>> = shown
                        .iter()
                        .enumerate()
                        .map(|(i, name)| Choice::new(i, name))
                        .collect();
                    let selected = hosts
                        .hosts
                        .iter()
                        .position(|h| h.primary() == draft.host)
                        .unwrap_or(usize::MAX);
                    let trigger = if draft.host.is_empty() {
                        "choose a host".to_string()
                    } else {
                        privacy.alias_text(&draft.host).into_owned()
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
                            let style = masked::Style {
                                font: FontId::monospace(12.0),
                                color: p.text,
                                on_card: false,
                            };
                            masked::text(ui, &command_line(draft), &style, true);
                        });
                    });
            });

        if let Some(start) = save {
            commands.push(Command::SaveTunnel(draft.clone()));
            if start {
                commands.push(Command::StartTunnel(draft.id.clone()));
                // kubectl: "web api is running on 127.0.0.1:18080" once up.
                if draft.is_kubectl() {
                    self.announce = Some(draft.id.clone());
                }
            }
            self.last_type = draft.kind;
            self.selected = Some(draft.id.clone());
            close = true;
        }
        if close {
            self.close_editor();
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
pub const LABEL: f32 = 110.0;

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
                        .prefilled()
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
                            .prefilled()
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
                                .prefilled()
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
                        .add(
                            TextField::new(&mut port)
                                .mono()
                                .prefilled()
                                .hint(labels[3])
                                .width(PORT),
                        )
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
                    let style = masked::Style {
                        font: FontId::proportional(12.0),
                        color: p.yellow,
                        on_card: true,
                    };
                    let text = format!("{}: {}", forward.bind, exposed_hint(forward.kind));
                    masked::text(ui, &text, &style, true);
                });
            }
            for error in errors {
                ui.label(RichText::new(error).size(12.0).color(p.red));
            }
        });
    remove
}

pub fn exposed_hint(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::R => {
            "listens on a non-loopback address of the server: other machines can reach it"
        }
        _ => "listens on a non-loopback address: other machines on your network can reach it",
    }
}

/// What the row menu asked for.
enum MenuAction {
    Command(Box<Command>),
    Edit,
    CopyCommand,
    Delete,
}

/// The row's right-click menu: Start, Stop, Edit, Copy command, Delete…
/// (SPEC.md §10, only existing actions). `startable`: Start can run (not
/// a kubectl tunnel while kubectl is missing).
fn tunnel_menu(
    response: &egui::Response,
    (tunnel_id, startable): (&str, bool),
    state: &TunnelState,
    status: Option<&TunnelStatus>,
) -> Option<MenuAction> {
    let manual = status.is_some_and(|s| s.owners.contains(&Owner::Manual));
    let active = !matches!(state, TunnelState::Stopped | TunnelState::Failed(_));
    let mut chosen = None;
    popup::context_menu(response, |menu| {
        // Running for a group only: a manual start keeps it up on its own.
        let start = if active && !manual {
            "Keep running"
        } else {
            "Start"
        };
        if menu.item(Some(icons::PLAY), start, startable && (!manual || !active)) {
            chosen = Some(MenuAction::Command(Box::new(Command::StartTunnel(
                tunnel_id.into(),
            ))));
        }
        if menu.item(Some(icons::SQUARE), "Stop", manual) {
            chosen = Some(MenuAction::Command(Box::new(Command::StopTunnel(
                tunnel_id.into(),
            ))));
        }
        menu.separator();
        if menu.item(None, "Edit", !active) {
            chosen = Some(MenuAction::Edit);
        }
        if menu.item(None, "Copy command", true) {
            chosen = Some(MenuAction::CopyCommand);
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
            let masked = privacy::get(ui.ctx()).on;
            let cells = |ui: &mut Ui, rect: Rect, texts: [(&str, Color32, bool); 5]| {
                let mut x = rect.left() + 12.0;
                for (i, (text, color, mono)) in texts.into_iter().enumerate() {
                    let font = if mono {
                        FontId::monospace(13.0)
                    } else {
                        fonts::semibold(11.0)
                    };
                    // Streamer mode: the addresses and ports are tokens.
                    if masked && mono && !text.is_empty() {
                        let cell = Rect::from_min_size(
                            pos2(x, rect.top()),
                            vec2(columns[i] - 8.0, rect.height()),
                        );
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(cell)
                                .layout(Layout::left_to_right(Align::Center)),
                        );
                        let style = masked::Style {
                            font,
                            color,
                            on_card: true,
                        };
                        masked::value(&mut child, text, &style);
                        x += columns[i];
                        continue;
                    }
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
        // ssh's own words: free text through the filter.
        let style = masked::Style {
            font: FontId::monospace(12.0),
            color: p.text_strong,
            on_card: true,
        };
        masked::text(ui, reason, &style, true);
        if let Some(body) = body {
            let style = masked::Style {
                font: FontId::proportional(13.0),
                color: p.text,
                on_card: true,
            };
            masked::text(ui, &body, &style, true);
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

/// `value` in mono, or as a masked token in streamer mode.
fn mono_value(ui: &mut Ui, value: &str, color: Color32) {
    let font = FontId::monospace(13.0);
    if privacy::get(ui.ctx()).on {
        let style = masked::Style {
            font,
            color,
            on_card: false,
        };
        masked::value(ui, value, &style);
    } else {
        ui.label(RichText::new(value).font(font).color(color));
    }
}

/// "kubectl · context dev-k3s · namespace api · service/web" under a kubectl
/// tunnel's name.
fn kube_line(ui: &mut Ui, tunnel: &Tunnel) {
    let p = &Palette::ONE_DARK;
    let dim = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).size(13.0).color(p.dim));
    };
    let kube = &tunnel.kube;
    dim(ui, "kubectl · context");
    if kube.context.is_empty() {
        dim(ui, "current");
    } else {
        mono_value(ui, &kube.context, p.text);
    }
    // Shown like aliases: "namespace N" / "resource N" while names are
    // hidden.
    let privacy = privacy::get(ui.ctx());
    dim(ui, "· namespace");
    ui.label(
        RichText::new(privacy.kube_name(&kube.namespace))
            .font(FontId::monospace(13.0))
            .color(p.text),
    );
    dim(ui, "·");
    ui.label(
        RichText::new(format!(
            "{}/{}",
            kube.resource.kind.word(),
            privacy.kube_name(&kube.resource.name)
        ))
        .font(FontId::monospace(13.0))
        .color(p.text),
    );
}

/// *Port on the service* / *pod* / *pod (picked by kubectl)*: where a
/// forward's remote port is.
fn kube_port_label(tunnel: &Tunnel) -> &'static str {
    match tunnel.kube.resource.kind {
        ResourceKind::Service => "Port on the service",
        ResourceKind::Pod => "Port on the pod",
        ResourceKind::Deployment => "Port on the pod (picked by kubectl)",
    }
}

/// A kubectl tunnel's forwards: *Listen on*, *Local port*, the port on the
/// resource.
fn kube_forwards_table(ui: &mut Ui, tunnel: &Tunnel) {
    let p = &Palette::ONE_DARK;
    let exposed = tunnel.kube.is_exposed();
    let address_color = if exposed { p.yellow } else { p.text };
    let response = Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new(Id::unique("kube_forwards"))
                .num_columns(3)
                .min_col_width(140.0)
                .spacing(vec2(16.0, 10.0))
                .show(ui, |ui| {
                    for title in ["Listen on", "Local port", kube_port_label(tunnel)] {
                        ui.label(
                            RichText::new(title.to_uppercase())
                                .font(fonts::semibold(11.0))
                                .color(p.dim),
                        );
                    }
                    ui.end_row();
                    for forward in &tunnel.kube.forwards {
                        mono_value(ui, &tunnel.kube.address, address_color);
                        mono_value(ui, &forward.listen.to_string(), p.text);
                        mono_value(ui, &forward.target, p.text);
                        ui.end_row();
                    }
                });
        })
        .response;
    if exposed {
        popup::tip_below(response, |ui| {
            ui.label(exposed_hint(ForwardKind::L));
        });
    }
}

/// The kubectl failure banner (SPEC.md §7 *kubectl tunnels*, *Failures*):
/// the friendly line, kubectl's own line, the fix and *Show log*.
fn kube_failure_banner(
    ui: &mut Ui,
    (tunnel, status, reason): (&Tunnel, Option<&TunnelStatus>, &str),
    commands: &mut Vec<Command>,
    show_log: &mut bool,
) -> Option<KubeFix> {
    let p = &Palette::ONE_DARK;
    let failure = status.and_then(|s| s.failure);
    let kind = tunnel.kube.resource.kind.word();
    let masked = privacy::get(ui.ctx()).on;
    let context = match (tunnel.kube.context.as_str(), masked) {
        ("", _) => "the current context",
        (_, true) => DOTS,
        (context, false) => context,
    };
    let port = tunnel
        .kube
        .forwards
        .iter()
        .map(|f| f.listen.to_string())
        .find(|port| {
            reason.contains(&format!("port {port}")) || reason.contains(&format!("{{{port} "))
        });
    let title = match failure {
        Some(FailureKind::ResourceGone) => format!("The {kind} is gone"),
        Some(FailureKind::NotLoggedIn) => format!("Not logged in to {context}"),
        Some(FailureKind::ClusterUnreachable) => "Can't reach the cluster".into(),
        Some(FailureKind::PortInUse) => match &port {
            Some(_) if masked => format!("Local port {DOTS} is in use"),
            Some(port) => format!("Local port {port} is in use"),
            None => "A local port is in use".into(),
        },
        Some(FailureKind::NoContext) => "Context not in your kubeconfig".into(),
        Some(FailureKind::KubectlMissing) => "kubectl not found".into(),
        _ => "kubectl exited".into(),
    };
    let body = match failure {
        Some(FailureKind::NotLoggedIn) => {
            Some("Log in with your cluster's tool in a terminal, then retry.")
        }
        Some(FailureKind::ResourceGone) if tunnel.kube.resource.kind == ResourceKind::Pod => {
            Some("A pod that is deleted doesn't come back under its name: pick another one.")
        }
        Some(FailureKind::KubectlMissing) => {
            Some("kubectl tunnels can't start until kubectl is installed or its path is set.")
        }
        _ => None,
    };
    let mut fix = None;
    let top = ui.cursor().top();
    marks::banner(ui, p.red_tint, |ui| {
        ui.label(
            RichText::new(title)
                .font(fonts::semibold(14.0))
                .color(p.red),
        );
        // kubectl's own words: free text through the filter.
        let style = masked::Style {
            font: FontId::monospace(12.0),
            color: p.text_strong,
            on_card: true,
        };
        masked::text(ui, reason, &style, true);
        if let Some(body) = body {
            ui.label(RichText::new(body).size(13.0).color(p.text));
        }
        if let Some(text) = reconnect_text(ui.ctx(), status) {
            ui.label(RichText::new(text).size(13.0).color(p.yellow));
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // The fix: a button, and what it does (`None`: retry).
            let action = match failure {
                Some(FailureKind::ResourceGone) => Some((
                    Button::new(format!("Pick another {kind}"))
                        .small()
                        .tip("Opens the editor on Forward to"),
                    Some(KubeFix::Edit),
                )),
                Some(FailureKind::NotLoggedIn | FailureKind::ClusterUnreachable) => Some((
                    Button::new("Retry")
                        .name("Retry from the banner")
                        .icon(icons::ROTATE_CW)
                        .small(),
                    None,
                )),
                Some(FailureKind::PortInUse | FailureKind::NoContext) => Some((
                    Button::new("Edit").name("Edit from the banner").small(),
                    Some(KubeFix::Edit),
                )),
                Some(FailureKind::KubectlMissing) => Some((
                    Button::new("Settings › kubectl").small(),
                    Some(KubeFix::Settings),
                )),
                _ => None,
            };
            if let Some((button, then)) = action
                && ui.add(button).clicked()
            {
                match then {
                    Some(then) => fix = Some(then),
                    None => commands.push(Command::StartTunnel(tunnel.id.clone())),
                }
            }
            if ui.add(Button::new("Show log")).clicked() {
                *show_log = true;
            }
        });
    });
    let rect = ui.min_rect().with_min_y(top);
    tour::targets::register(ui.ctx(), Target::FailureBanner, rect);
    fix
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
                        let style = masked::Style {
                            font: FontId::monospace(12.0),
                            color,
                            on_card: false,
                        };
                        masked::text(ui, &format!("{time}  {}", line.text), &style, true);
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
        kubectl: Option<KubectlInfo>,
    }

    fn tunnel() -> Tunnel {
        Tunnel::ssh(
            "t-1",
            "Prod DB",
            "bastion",
            vec![Forward {
                kind: ForwardKind::L,
                bind: DEFAULT_BIND.into(),
                listen: 5432,
                target: Some("db:5432".into()),
            }],
        )
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
            after: None,
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
            kubectl: None,
        }
    }

    fn kube_tunnel() -> Tunnel {
        use sshelf_core::tunnels::{Kube, KubeForward, Resource};
        Tunnel::kubectl(
            "t-k",
            "web api",
            Kube {
                context: "lab-kq".into(),
                namespace: "api".into(),
                resource: Resource::parse("service/web").unwrap(),
                address: DEFAULT_BIND.into(),
                forwards: vec![
                    KubeForward {
                        listen: 18080,
                        target: "80".into(),
                    },
                    KubeForward {
                        listen: 19090,
                        target: "metrics".into(),
                    },
                ],
            },
        )
    }

    /// The ssh fixture plus kubectl tunnel `t-k` in `state` (failing with
    /// `failure`), and kubectl found or not.
    fn kube_fixture(state: TunnelState, failure: Option<FailureKind>, found: bool) -> Fixture {
        let mut f = fixture(TunnelState::Stopped);
        let mut data = (*f.data).clone();
        data.tunnels.upsert_tunnel(kube_tunnel());
        f.data = Arc::new(data);
        let mut states = (*f.states).clone();
        let mut status = states.tunnels["t-1"].clone();
        status.running_since = (state == TunnelState::Running).then(Instant::now);
        status.state = state;
        status.failure = failure;
        states.tunnels.insert("t-k".into(), status);
        f.states = Arc::new(states);
        f.kubectl = Some(KubectlInfo {
            kubectl: if found {
                Ok(sshelf_core::kubectl::Kubectl {
                    tool: sshelf_core::ssh::Tool::new("kubectl"),
                    source: sshelf_core::kubectl::KubectlSource::Path,
                })
            } else {
                Err("kubectl not found".into())
            },
            version: None,
            files: Default::default(),
            view: None,
        });
        f
    }

    fn kube_selected() -> TunnelsView {
        TunnelsView {
            selected: Some("t-k".into()),
            ..TunnelsView::default()
        }
    }

    #[test]
    fn kubectl_rows_have_their_mark_line_and_a_filter() {
        let mut harness = harness(
            kube_fixture(TunnelState::Running, None, true),
            kube_selected(),
        );
        harness.step();
        // The row's line, cut to the list's width (in full below).
        // Cut in the middle to fit the list.
        let drawn = test_util::drawn_text(&harness);
        assert!(drawn.contains("lab-kq / ap…web · 18080"), "{drawn}");
        // Both types exist: the filter, and only kubectl rows under it.
        harness.get_by_label("kubectl").click();
        harness.step();
        harness.step();
        assert!(harness.query_by_label("Prod DB").is_none());
        harness.get_by_label("ssh").click();
        harness.step();
        harness.step();
        let drawn = test_util::drawn_text(&harness);
        assert!(drawn.contains("Prod DB"), "{drawn}");
        assert!(!drawn.contains("lab-kq / ap"), "{drawn}");
    }

    /// The rows' second lines in full (the list cuts them to its width).
    #[test]
    fn row_lines_per_type_and_state() {
        let ctx = egui::Context::default();
        let file = TunnelsFile::default();
        let line = |tunnel: &Tunnel, state: TunnelState, failure, missing| {
            let mut status = fixture(TunnelState::Stopped).states.tunnels["t-1"].clone();
            status.running_since = (state == TunnelState::Running)
                .then(|| Instant::now() - Duration::from_secs(2 * 3600 + 14 * 60));
            status.state = state;
            status.failure = failure;
            sub_line(&ctx, tunnel, Some(&status), &file, missing).0
        };
        let (ssh, kube) = (tunnel(), kube_tunnel());
        assert_eq!(
            line(&ssh, TunnelState::Running, None, false),
            "Running · 2 h 14 m · L 5432"
        );
        let failed = TunnelState::Failed("why".into());
        assert_eq!(
            line(&ssh, failed.clone(), Some(FailureKind::Auth), false),
            "Failed · key not accepted"
        );
        assert_eq!(
            line(&kube, TunnelState::Running, None, false),
            "lab-kq / api · service/web · 18080"
        );
        assert_eq!(
            line(&kube, TunnelState::Stopped, None, true),
            "kubectl not found"
        );
        assert_eq!(
            line(
                &kube,
                failed.clone(),
                Some(FailureKind::ResourceGone),
                false
            ),
            "Failed · service not found"
        );
        assert_eq!(
            line(&kube, failed.clone(), Some(FailureKind::NotLoggedIn), false),
            "Failed · not logged in to lab-kq"
        );
        assert_eq!(
            line(&kube, failed, Some(FailureKind::Other), false),
            "Failed · why"
        );
        let mut current = kube_tunnel();
        current.kube.context.clear();
        assert_eq!(
            line(&current, TunnelState::Stopped, None, false),
            "current context / api · service/web · 18080"
        );
    }

    #[test]
    fn the_kubectl_detail_names_where_it_forwards() {
        let mut harness = harness(
            kube_fixture(TunnelState::Running, None, true),
            kube_selected(),
        );
        harness.step();
        harness.get_by_label("kubectl · context");
        harness.get_by_label("lab-kq");
        harness.get_by_label("service/web");
        harness.get_by_label("PORT ON THE SERVICE");
        harness.get_by_label("metrics");
        harness.get_by_label("19090");
        assert!(harness.query_by_label("via").is_none());
    }

    #[test]
    fn kubectl_failures_offer_their_fix() {
        let cases = [
            (
                FailureKind::ResourceGone,
                "The service is gone",
                "Pick another service",
            ),
            (
                FailureKind::NotLoggedIn,
                "Not logged in to lab-kq",
                "Retry from the banner",
            ),
            (
                FailureKind::ClusterUnreachable,
                "Can't reach the cluster",
                "Retry from the banner",
            ),
            (
                FailureKind::NoContext,
                "Context not in your kubeconfig",
                "Edit from the banner",
            ),
            (
                FailureKind::KubectlMissing,
                "kubectl not found",
                "Settings › kubectl",
            ),
        ];
        for (failure, title, fix) in cases {
            let state = TunnelState::Failed("kubectl said so".into());
            let mut harness = harness(kube_fixture(state, Some(failure), true), kube_selected());
            harness.step();
            harness.get_by_label(title);
            harness.get_by_label("kubectl said so");
            harness.get_by_label(fix).click();
            harness.step();
            let view = &mut harness.state_mut().0;
            match failure {
                FailureKind::NotLoggedIn | FailureKind::ClusterUnreachable => {
                    assert_eq!(harness.state().1, [Command::StartTunnel("t-k".into())]);
                }
                FailureKind::KubectlMissing => assert!(view.take_open_settings()),
                _ => assert!(view.is_editing(), "{title}"),
            }
        }
    }

    #[test]
    fn without_kubectl_its_tunnels_cant_start() {
        let mut harness = harness(
            kube_fixture(TunnelState::Stopped, None, false),
            kube_selected(),
        );
        harness.step();
        harness.get_by_label("kubectl not found");
        let starts: Vec<_> = harness.get_all_by_label("Start").collect();
        // The row's and the header's, both disabled; the ssh row's isn't.
        assert_eq!(
            starts
                .iter()
                .filter(|s| s.accesskit_node().is_disabled())
                .count(),
            2
        );
    }

    #[test]
    fn copy_command_copies_kubectls_command() {
        let mut harness = harness(
            kube_fixture(TunnelState::Stopped, None, true),
            kube_selected(),
        );
        harness.step();
        // The row (the detail's title has the name too).
        harness
            .get_all_by_label("web api")
            .next()
            .unwrap()
            .click_secondary();
        harness.step();
        harness.step();
        harness.get_by_label("Copy command").click();
        harness.step();
        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            });
        assert_eq!(
            copied.as_deref(),
            Some(
                "kubectl --context lab-kq --namespace api port-forward service/web 18080:80 \
                 19090:metrics --address 127.0.0.1"
            )
        );
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
                        (f.ssh.as_ref(), f.kubectl.as_ref()),
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
        // Cut to the list's width (in full in `row_lines_per_type_and_state`).
        assert!(test_util::drawn_text(&harness).contains("Failed · key"));
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
        // Cut to the list's width (in full in `row_lines_per_type_and_state`).
        assert!(test_util::drawn_text(&harness).contains("Running · 2 h"));
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
    fn a_group_tunnel_offers_keep_running_in_the_detail_only() {
        let mut f = fixture(TunnelState::Running);
        let mut states = (*f.states).clone();
        states.tunnels.get_mut("t-1").unwrap().owners = BTreeSet::from([Owner::Group("g".into())]);
        f.states = Arc::new(states);
        let mut harness = harness(f, selected());
        harness.step();
        harness.get_by_label("Running · group g");
        // No Start on a running row; the detail keeps it up on its own.
        assert!(harness.query_by_label("Start").is_none());
        assert_eq!(harness.get_all_by_label("Keep running").count(), 1);
        harness.get_by_label("Keep running").click();
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

    /// A new kubectl tunnel from empty, with the lookups answered by hand:
    /// the switch, Missing, the namespace (listed on open, with the loading
    /// row), the service and its port chips, the suggested local port,
    /// Ready, *Save and start*, then the toast once it runs.
    #[test]
    fn a_kubectl_tunnel_is_made_in_the_editor() {
        use sshelf_core::kubectl::{KubeItem, KubeList, KubePort, KubeQuery};
        let f = kube_fixture(TunnelState::Stopped, None, true);
        let mut view = TunnelsView::default();
        view.new_tunnel("", &f.data.tunnels);
        let mut harness = harness(f, view);
        harness.step();
        // The editor's switch (the list's type filter is first).
        harness.get_all_by_label("kubectl").last().unwrap().click();
        harness.step();
        harness.step();
        harness.get_by_label("Missing: name, namespace, service, a port");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
        harness.state_mut().0.editor.as_mut().unwrap().name = "web api 2".into();

        // The namespaces are listed when the dropdown opens.
        harness.get_by_label("Namespace").click();
        harness.step();
        harness.step();
        let namespaces = KubeQuery {
            context: String::new(),
            what: KubeList::Namespaces,
        };
        assert_eq!(harness.state().1, [Command::KubeLookup(namespaces.clone())]);
        assert!(test_util::drawn_text(&harness).contains("Listing namespaces on"));
        let item = |name: &str, ports: &[(u16, &str)]| KubeItem {
            name: name.into(),
            ports: ports
                .iter()
                .map(|&(port, name)| KubePort {
                    port,
                    name: (!name.is_empty()).then(|| name.into()),
                })
                .collect(),
            status: None,
        };
        harness
            .state_mut()
            .0
            .on_lookup(namespaces, Ok(vec![item("api", &[]), item("ops", &[])]));
        harness.step();
        harness.step();
        assert!(
            test_util::drawn_text(&harness)
                .contains("kubectl get namespaces · 2 · you can also type one")
        );
        harness.get_by_label("ops").click();
        harness.step();
        harness.step();
        assert_eq!(
            harness.state().0.editor.as_ref().unwrap().kube.namespace,
            "ops"
        );

        // Then the services in it, with their ports.
        harness.state_mut().1.clear();
        harness.get_by_label("Resource").click();
        harness.step();
        harness.step();
        let services = KubeQuery {
            context: String::new(),
            what: KubeList::Resources {
                namespace: "ops".into(),
                kind: ResourceKind::Service,
            },
        };
        assert_eq!(harness.state().1, [Command::KubeLookup(services.clone())]);
        harness.state_mut().0.on_lookup(
            services,
            Ok(vec![item("grafana", &[(80, "http"), (9090, "metrics")])]),
        );
        harness.step();
        harness.step();
        harness.get_by_label("grafana").click();
        harness.step();
        harness.step();
        // One empty forward with the ports as chips; 18080 is taken by
        // `web api`, so the suggestion is the next one up.
        harness.get_by_label("80 http").click();
        harness.step();
        harness.step();
        let draft = harness.state().0.editor.clone().unwrap();
        assert_eq!(draft.kube.resource.to_string(), "service/grafana");
        assert_eq!(
            draft.kube.forwards,
            [sshelf_core::tunnels::KubeForward {
                listen: 18081,
                target: "80".into()
            }]
        );
        harness.get_by_label("Ready");
        harness.get_by_label_contains(
            "kubectl --namespace ops port-forward service/grafana 18081:80 --address 127.0.0.1",
        );

        harness.state_mut().1.clear();
        harness.get_by_label("Save and start").click();
        harness.step();
        let id = draft.id.clone();
        assert_eq!(
            harness.state().1,
            [
                Command::SaveTunnel(draft.clone()),
                Command::StartTunnel(id.clone())
            ]
        );
        assert!(!harness.state().0.is_editing());
        assert_eq!(harness.state_mut().0.take_running_toast(), None);
    }

    #[test]
    fn a_typed_namespace_is_used_and_a_failed_lookup_says_why() {
        use sshelf_core::kubectl::{KubeList, KubeQuery};
        let f = kube_fixture(TunnelState::Stopped, None, true);
        let mut view = TunnelsView::default();
        view.new_tunnel("", &f.data.tunnels);
        kube_editor::switch_type(view.editor.as_mut().unwrap(), TunnelType::Kubectl);
        let mut harness = harness(f, view);
        harness.step();
        harness.get_by_label("Namespace").click();
        harness.step();
        harness.step();
        let query = KubeQuery {
            context: String::new(),
            what: KubeList::Namespaces,
        };
        harness
            .state_mut()
            .0
            .on_lookup(query, Err("Error from server (Forbidden)".into()));
        harness.step();
        harness.step();
        assert!(
            test_util::drawn_text(&harness)
                .contains("Can't list namespaces: Error from server (Forbidden)")
        );
        harness.event(egui::Event::Text("lab-ns".into()));
        harness.step();
        harness.step();
        harness.get_by_label("Use \u{201c}lab-ns\u{201d}");
        harness.key_press(egui::Key::Enter);
        harness.step();
        harness.step();
        assert_eq!(
            harness.state().0.editor.as_ref().unwrap().kube.namespace,
            "lab-ns"
        );
        // The resource can be picked now (it was disabled before).
        assert!(
            !harness
                .get_by_label("Resource")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn without_kubectl_a_kubectl_tunnel_saves_but_doesnt_start() {
        let f = kube_fixture(TunnelState::Stopped, None, false);
        let view = TunnelsView {
            editor: Some(kube_tunnel()),
            ..TunnelsView::default()
        };
        let mut harness = harness(f, view);
        harness.step();
        harness.get_by_label_contains("kubectl wasn't found, so this tunnel can be saved");
        harness.get_by_label("Ready");
        assert!(!harness.get_by_label("Save").accesskit_node().is_disabled());
        assert!(
            harness
                .get_by_label("Save and start")
                .accesskit_node()
                .is_disabled()
        );
        harness.get_by_label("Settings › kubectl").click();
        harness.step();
        assert!(harness.state_mut().0.take_open_settings());
    }

    /// The tour's sandbox has no kubectl: the switch's kubectl half is
    /// disabled there.
    #[test]
    fn the_tour_keeps_the_editor_ssh_only() {
        let mut f = fixture(TunnelState::Stopped);
        f.kubectl = Some(KubectlInfo {
            kubectl: Err(sshelf_core::kubectl::NOT_IN_TOUR.into()),
            version: None,
            files: Default::default(),
            view: None,
        });
        let mut view = TunnelsView::default();
        view.new_tunnel("", &f.data.tunnels);
        let mut harness = harness(f, view);
        harness.step();
        let kubectl = harness.get_by_label("kubectl");
        assert!(kubectl.accesskit_node().is_disabled());
        kubectl.click();
        harness.step();
        assert!(!harness.state().0.editor.as_ref().unwrap().is_kubectl());
    }

    /// Once a kubectl tunnel saved and started runs, the app gets its toast.
    #[test]
    fn a_started_kubectl_tunnel_announces_where_it_runs() {
        let f = kube_fixture(TunnelState::Running, None, true);
        let view = TunnelsView {
            announce: Some("t-k".into()),
            ..TunnelsView::default()
        };
        let mut harness = harness(f, view);
        harness.step();
        assert_eq!(
            harness.state_mut().0.take_running_toast(),
            Some((
                "web api is running on".to_string(),
                "127.0.0.1:18080".to_string()
            ))
        );
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
