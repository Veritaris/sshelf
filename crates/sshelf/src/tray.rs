//! Tray icon and menu (SPEC.md §10 "Tray"): the *Streamer mode* switch,
//! recent hosts to connect to, groups to start or stop, active tunnels to
//! stop, Show and Quit.
//!
//! [`model`] builds a [`TrayModel`] from app state (pure, tested); the native
//! menu ([`TrayModel::entries`]) and the sshelf menu (`tray_menu`) both show
//! it. [`Tray`] owns the platform icon, rebuilds the native menu only when it
//! changes (and drops it while the sshelf menu is used), and turns clicks into
//! [`TrayAction`]s. Everything here runs on the main thread from
//! `App::logic`, which eframe keeps calling while the window is hidden; tray
//! clicks request a repaint to get there.

use std::sync::mpsc::{self, Receiver};

use sshconf::HostList;
use sshelf_core::core::{AppData, Command};
use sshelf_core::settings::TrayIconStyle;
use sshelf_core::supervisor::{GroupState, Owner, Snapshot, TunnelState};
use sshelf_core::tunnels::TunnelType;
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::close_guard;
use crate::icon::{self, Status};
use crate::icon_shape;

/// Recent hosts in the menu.
pub const RECENT_HOSTS: usize = 8;

/// Where the icon is on screen, in physical pixels (from the click).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    Connect(String),
    StartGroup(String),
    StopGroup(String),
    StopTunnel(String),
    /// Shows the window on this tunnel (a failed one in the sshelf menu).
    ShowTunnel(String),
    Show,
    Quit,
    /// Right click with the sshelf menu on: open it by the icon.
    OpenMenu(IconRect),
    /// The *Streamer mode* switch: on or off.
    SetStreamer(bool),
}

impl TrayAction {
    /// The menu item id.
    pub fn id(&self) -> String {
        match self {
            TrayAction::Connect(alias) => format!("connect:{alias}"),
            TrayAction::StartGroup(id) => format!("group-start:{id}"),
            TrayAction::StopGroup(id) => format!("group-stop:{id}"),
            TrayAction::StopTunnel(id) => format!("tunnel-stop:{id}"),
            TrayAction::ShowTunnel(id) => format!("tunnel-show:{id}"),
            TrayAction::Show => "show".into(),
            TrayAction::Quit => "quit".into(),
            TrayAction::OpenMenu(_) => "open-menu".into(),
            TrayAction::SetStreamer(on) => format!("streamer:{}", if *on { "on" } else { "off" }),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "show" => return Some(TrayAction::Show),
            "quit" => return Some(TrayAction::Quit),
            _ => {}
        }
        let (kind, arg) = id.split_once(':')?;
        let arg = arg.to_string();
        Some(match kind {
            "connect" => TrayAction::Connect(arg),
            "group-start" => TrayAction::StartGroup(arg),
            "group-stop" => TrayAction::StopGroup(arg),
            "tunnel-stop" => TrayAction::StopTunnel(arg),
            "tunnel-show" => TrayAction::ShowTunnel(arg),
            "streamer" => match arg.as_str() {
                "on" => TrayAction::SetStreamer(true),
                "off" => TrayAction::SetStreamer(false),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// The core command, for actions that are one.
    pub fn command(&self) -> Option<Command> {
        Some(match self {
            TrayAction::Connect(alias) => Command::Connect(alias.clone()),
            TrayAction::StartGroup(id) => Command::StartGroup(id.clone()),
            TrayAction::StopGroup(id) => Command::StopGroup(id.clone()),
            TrayAction::StopTunnel(id) => Command::StopTunnel(id.clone()),
            TrayAction::ShowTunnel(_)
            | TrayAction::Show
            | TrayAction::Quit
            | TrayAction::OpenMenu(_)
            | TrayAction::SetStreamer(_) => return None,
        })
    }
}

/// A native menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A clickable item, or a disabled line of information (`None`).
    Item {
        label: String,
        action: Option<String>,
    },
    /// An item with a check mark; a click runs `action`.
    Check {
        label: String,
        checked: bool,
        action: String,
    },
    Separator,
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
}

fn item(label: impl Into<String>, action: TrayAction) -> Entry {
    Entry::Item {
        label: label.into(),
        action: Some(action.id()),
    }
}

fn info(label: impl Into<String>) -> Entry {
    Entry::Item {
        label: label.into(),
        action: None,
    }
}

/// A group in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    pub id: String,
    pub name: String,
    pub state: GroupState,
}

/// A tunnel that isn't stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelRow {
    pub id: String,
    pub name: String,
    /// The first forward's listen port.
    pub port: Option<u16>,
    /// ssh or kubectl (the sshelf menu shows its mark).
    pub kind: TunnelType,
    pub state: TunnelState,
    /// Started by hand: *Stop* releases that start.
    pub manual: bool,
    pub reconnecting: bool,
    /// The groups holding it.
    pub used_by: Vec<String>,
}

/// A host launched from sshelf: its alias, and how both menus show it (in
/// streamer mode "host N" or "•••••"; the app sets it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentHost {
    pub alias: String,
    pub shown: String,
}

impl RecentHost {
    pub fn new(alias: &str) -> Self {
        Self {
            alias: alias.to_string(),
            shown: alias.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayModel {
    /// Tunnels starting or running.
    pub running: usize,
    pub failed: usize,
    /// The icon's dot.
    pub status: Status,
    pub tooltip: String,
    pub groups: Vec<GroupRow>,
    pub tunnels: Vec<TunnelRow>,
    /// Hosts launched from sshelf that still exist, newest first.
    pub recent: Vec<RecentHost>,
    /// A host key waits for the user's answer.
    pub host_key_waiting: bool,
    /// The tour runs: this is its sandbox, under a "Tour" header.
    pub tour: bool,
    /// Streamer mode is on (the user's, also during the tour): its switch
    /// is on and ports are masked.
    pub streamer: bool,
}

impl Default for TrayModel {
    fn default() -> Self {
        model(None, None, None, false)
    }
}

/// The menu for the current state.
pub fn model(
    data: Option<&AppData>,
    hosts: Option<&HostList>,
    states: Option<&Snapshot>,
    host_key_waiting: bool,
) -> TrayModel {
    let empty = Snapshot::default();
    let states = states.unwrap_or(&empty);

    let mut recent: Vec<RecentHost> = Vec::new();
    if let (Some(data), Some(hosts)) = (data, hosts) {
        for launch in &data.history.entries {
            if let Some(host) = hosts.find(&launch.alias)
                && !recent.iter().any(|r| r.alias == host.primary())
            {
                recent.push(RecentHost::new(host.primary()));
            }
            if recent.len() == RECENT_HOSTS {
                break;
            }
        }
    }

    let groups = data.map_or_else(Vec::new, |data| {
        data.tunnels
            .groups
            .iter()
            .map(|group| GroupRow {
                id: group.id.clone(),
                name: group.name.clone(),
                state: states.group_state(group),
            })
            .collect()
    });

    let mut tunnels = Vec::new();
    if let Some(data) = data {
        for tunnel in &data.tunnels.tunnels {
            let Some(status) = states.tunnels.get(&tunnel.id) else {
                continue;
            };
            if status.state == TunnelState::Stopped {
                continue;
            }
            tunnels.push(TunnelRow {
                id: tunnel.id.clone(),
                name: tunnel.name.clone(),
                port: tunnel.listen_ports().first().copied(),
                kind: tunnel.kind,
                state: status.state.clone(),
                manual: status.owners.contains(&Owner::Manual),
                reconnecting: status.reconnect.is_some(),
                used_by: status
                    .owners
                    .iter()
                    .filter_map(|o| match o {
                        Owner::Group(id) => data.tunnels.group(id).map(|g| g.name.clone()),
                        Owner::Manual => None,
                    })
                    .collect(),
            });
        }
    }

    let running = close_guard::running_count(Some(states));
    let failed = states
        .tunnels
        .values()
        .filter(|t| matches!(t.state, TunnelState::Failed(_)))
        .count();
    let mut tooltip = match running {
        0 => "sshelf".to_string(),
        1 => "sshelf · 1 tunnel running".to_string(),
        n => format!("sshelf · {n} tunnels running"),
    };
    if failed > 0 {
        tooltip.push_str(&format!(" · {failed} failed"));
    }
    let status = Status::of(
        Some(states),
        data.map(|d| d.tunnels.groups.as_slice())
            .unwrap_or_default(),
    );
    TrayModel {
        running,
        failed,
        status,
        tooltip,
        groups,
        tunnels,
        recent,
        host_key_waiting,
        tour: false,
        streamer: false,
    }
}

impl TrayModel {
    /// "2 running · 1 failed", or "nothing running".
    pub fn summary(&self) -> String {
        match (self.running, self.failed) {
            (0, 0) => "nothing running".into(),
            (r, 0) => format!("{r} running"),
            (0, f) => format!("{f} failed"),
            (r, f) => format!("{r} running · {f} failed"),
        }
    }

    /// The native menu.
    pub fn entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        if self.tour {
            entries.push(info("Tour · sample data"));
        }
        entries.push(info(format!("sshelf: {}", self.summary())));
        if self.host_key_waiting {
            entries.push(item("A host key waits for you…", TrayAction::Show));
        }
        entries.push(Entry::Check {
            label: "Streamer mode".into(),
            checked: self.streamer,
            action: TrayAction::SetStreamer(!self.streamer).id(),
        });
        entries.push(Entry::Separator);
        let connect: Vec<Entry> = if self.recent.is_empty() {
            vec![info("No sessions launched from sshelf yet")]
        } else {
            self.recent
                .iter()
                .map(|r| item(r.shown.clone(), TrayAction::Connect(r.alias.clone())))
                .collect()
        };
        entries.push(Entry::Submenu {
            label: "Connect".into(),
            entries: connect,
        });

        if !self.groups.is_empty() {
            entries.push(Entry::Separator);
            entries.push(info("Groups"));
            for group in &self.groups {
                let (name, stop) = (&group.name, TrayAction::StopGroup(group.id.clone()));
                entries.push(match group.state {
                    GroupState::Stopped => item(
                        format!("Start {name}"),
                        TrayAction::StartGroup(group.id.clone()),
                    ),
                    GroupState::Starting => item(format!("Stop {name} (starting)"), stop),
                    GroupState::Running => item(format!("Stop {name}"), stop),
                    GroupState::Degraded { running, total } => {
                        item(format!("Stop {name} ({running}/{total} running)"), stop)
                    }
                });
            }
        }

        if !self.tunnels.is_empty() {
            entries.push(Entry::Separator);
            entries.push(info("Active tunnels"));
            for tunnel in &self.tunnels {
                let name = &tunnel.name;
                entries.push(match &tunnel.state {
                    TunnelState::Stopping => info(format!("{name} (stopping)")),
                    state if tunnel.manual => {
                        let suffix = match state {
                            TunnelState::Starting => " (starting)",
                            TunnelState::Failed(_) if tunnel.reconnecting => " (reconnecting)",
                            TunnelState::Failed(_) => " (failed)",
                            _ => "",
                        };
                        item(
                            format!("Stop {name}{suffix}"),
                            TrayAction::StopTunnel(tunnel.id.clone()),
                        )
                    }
                    _ => info(format!("{name} (used by {})", tunnel.used_by.join(", "))),
                });
            }
        }

        entries.push(Entry::Separator);
        entries.push(item("Show sshelf", TrayAction::Show));
        entries.push(item("Quit", TrayAction::Quit));
        entries
    }
}

/// What the platform tray reports.
enum TrayEvent {
    Menu(String),
    /// The icon was clicked with the button that shows the window.
    ShowClicked,
    /// The icon was clicked with the menu's button (right; left on macOS).
    MenuClicked(IconRect),
}

/// The platform tray icon.
pub struct Tray {
    icon: TrayIcon,
    shown: Option<TrayModel>,
    /// The native menu is attached (not the sshelf menu).
    native: Option<bool>,
    /// How the icon is drawn now.
    look: Option<Look>,
    events: Receiver<TrayEvent>,
}

impl Tray {
    /// Creates the icon; clicks wake the app through `ctx`.
    pub fn create(ctx: &egui::Context) -> Result<Self, String> {
        let (tx, events) = mpsc::channel();
        let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = menu_tx.send(TrayEvent::Menu(event.id().0.clone()));
            menu_ctx.request_repaint();
        }));
        let icon_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                // The menu's button: right, but left on macOS, where menu
                // bar items open their menu on a left click; the other one
                // shows the window.
                let menu_button = if cfg!(target_os = "macos") {
                    MouseButton::Left
                } else {
                    MouseButton::Right
                };
                let event = match button {
                    MouseButton::Middle => return,
                    b if b == menu_button => TrayEvent::MenuClicked(IconRect {
                        x: rect.position.x,
                        y: rect.position.y,
                        width: f64::from(rect.size.width),
                        height: f64::from(rect.size.height),
                    }),
                    _ => TrayEvent::ShowClicked,
                };
                let _ = tx.send(event);
                icon_ctx.request_repaint();
            }
        }));
        let icon = TrayIconBuilder::new()
            .with_icon(tray_icon(Status::Idle, Look::of(TrayIconStyle::Colored))?)
            .with_tooltip("sshelf")
            // The system menu (when chosen) on the same button as ours.
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            shown: None,
            native: None,
            look: None,
            events,
        })
    }

    /// Shows `model`, rebuilding what changed. `native`: the system menu
    /// opens on the menu's button (right; left on macOS); otherwise there is
    /// none, and those clicks come back as [`TrayAction::OpenMenu`].
    pub fn update(&mut self, model: TrayModel, native: bool, style: TrayIconStyle) {
        let look = Look::of(style);
        if self.shown.as_ref() == Some(&model)
            && self.native == Some(native)
            && self.look == Some(look)
        {
            return;
        }
        let previous = self.shown.as_ref();
        if (previous.is_none_or(|p| p.status != model.status) || self.look != Some(look))
            && let Ok(icon) = tray_icon(model.status, look)
        {
            #[cfg(target_os = "macos")]
            let _ = if look.template {
                self.icon.set_icon_templated(Some(icon))
            } else {
                self.icon.set_icon(Some(icon))
            };
            #[cfg(not(target_os = "macos"))]
            let _ = self.icon.set_icon(Some(icon));
            self.look = Some(look);
        }
        if previous.is_none_or(|p| p.tooltip != model.tooltip) {
            let _ = self.icon.set_tooltip(Some(&model.tooltip));
        }
        let entries = model.entries();
        let menu_changed = previous.is_none_or(|p| p.entries() != entries);
        if self.native != Some(native) || (native && menu_changed) {
            if native {
                let menu = Menu::new();
                for entry in build(&entries) {
                    let _ = menu.append(entry.as_ref());
                }
                self.icon.set_menu(Some(Box::new(menu)));
            } else {
                self.icon.set_menu(None);
            }
        }
        self.native = Some(native);
        self.shown = Some(model);
    }

    /// Clicks since the last call; a click on the icon shows the window.
    pub fn actions(&mut self) -> Vec<TrayAction> {
        let native = self.native != Some(false);
        self.events
            .try_iter()
            .filter_map(|event| match event {
                TrayEvent::Menu(id) => TrayAction::parse(&id),
                TrayEvent::ShowClicked => Some(TrayAction::Show),
                TrayEvent::MenuClicked(rect) => (!native).then_some(TrayAction::OpenMenu(rect)),
            })
            .collect()
    }
}

fn tray_icon(status: Status, look: Look) -> Result<tray_icon::Icon, String> {
    let rgba = icon::rgba(status, look.colors, look.template);
    tray_icon::Icon::from_rgba(rgba, icon::SIZE, icon::SIZE).map_err(|e| e.to_string())
}

/// How the tray icon is drawn for a [`TrayIconStyle`] here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    colors: icon_shape::Look,
    /// A macOS template image (the system tints it; colours are dropped).
    template: bool,
}

impl Look {
    fn of(style: TrayIconStyle) -> Self {
        let colors = match style {
            TrayIconStyle::Colored => icon_shape::COLORED,
            TrayIconStyle::Monochrome => icon_shape::Look {
                body: mono_ink(),
                cut: None,
            },
        };
        Self {
            colors,
            template: cfg!(target_os = "macos") && style == TrayIconStyle::Monochrome,
        }
    }
}

/// The monochrome icon's colour: black on macOS (a template, tinted by the
/// system); on Windows white or black for the taskbar's theme; white on
/// Linux, whose panels are dark by default.
fn mono_ink() -> [u8; 3] {
    const WHITE: [u8; 3] = [255, 255, 255];
    const BLACK: [u8; 3] = [0, 0, 0];
    if cfg!(target_os = "macos") {
        return BLACK;
    }
    #[cfg(target_os = "windows")]
    if windows_taskbar_is_light() {
        return BLACK;
    }
    WHITE
}

/// Windows: whether the taskbar is light ("Choose your default Windows
/// mode": `SystemUsesLightTheme`; apps have their own setting). Read each
/// time the icon is drawn, so a theme change shows with the next update.
#[cfg(target_os = "windows")]
fn windows_taskbar_is_light() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("SystemUsesLightTheme");
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated UTF-16; `value` and `size`
    // describe a 4-byte buffer, which a REG_DWORD fits.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && value == 1
}

/// Menu items for `entries`.
fn build(entries: &[Entry]) -> Vec<Box<dyn IsMenuItem>> {
    entries
        .iter()
        .map(|entry| -> Box<dyn IsMenuItem> {
            match entry {
                Entry::Item {
                    label,
                    action: Some(id),
                } => Box::new(MenuItem::with_id(id.clone(), menu_text(label), true, None)),
                Entry::Item {
                    label,
                    action: None,
                } => Box::new(MenuItem::new(menu_text(label), false, None)),
                Entry::Check {
                    label,
                    checked,
                    action,
                } => Box::new(CheckMenuItem::with_id(
                    action.clone(),
                    menu_text(label),
                    true,
                    *checked,
                    None,
                )),
                Entry::Separator => Box::new(PredefinedMenuItem::separator()),
                Entry::Submenu { label, entries } => {
                    let submenu = Submenu::new(menu_text(label), true);
                    for child in build(entries) {
                        let _ = submenu.append(child.as_ref());
                    }
                    Box::new(submenu)
                }
            }
        })
        .collect()
}

/// Windows menus treat `&` as a mnemonic marker.
fn menu_text(label: &str) -> String {
    if cfg!(windows) {
        label.replace('&', "&&")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use sshconf::{ConfigSet, ResolveOptions};
    use sshelf_core::supervisor::TunnelStatus;
    use sshelf_core::tunnels::{Forward, ForwardKind, Group, Tunnel};

    use super::*;

    fn hosts(text: &str) -> HostList {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        fs::write(&config, text).unwrap();
        let set = ConfigSet::load(&config, &ResolveOptions::new(tmp.path())).unwrap();
        HostList::from_set(&set)
    }

    fn status(state: TunnelState, owners: &[Owner]) -> TunnelStatus {
        TunnelStatus {
            state,
            owners: owners.iter().cloned().collect::<BTreeSet<_>>(),
            log: VecDeque::new(),
            pid: None,
            failure: None,
            reconnect: None,
            running_since: None,
        }
    }

    fn data() -> AppData {
        let mut data = AppData::new(PathBuf::from("app"));
        let now = SystemTime::now();
        // Recorded oldest first; the history is newest first.
        for (i, alias) in ["gone", "db", "w1", "web", "db"].iter().enumerate() {
            data.history
                .record(alias, now + Duration::from_secs(i as u64));
        }
        for (id, name, host) in [
            ("t-a", "API", "web"),
            ("t-b", "DB", "db"),
            ("t-c", "Cache", "db"),
        ] {
            data.tunnels.tunnels.push(Tunnel::ssh(
                id,
                name,
                host,
                vec![Forward {
                    kind: ForwardKind::L,
                    bind: "127.0.0.1".into(),
                    listen: 6443,
                    target: Some("localhost:6443".into()),
                }],
            ));
        }
        data.tunnels.groups.push(Group {
            id: "g-1".into(),
            name: "Work".into(),
            tunnels: vec!["t-b".into(), "t-c".into()],
            start_on_launch: false,
        });
        data
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Item { label, action } => {
                    format!("{label}{}", if action.is_none() { " [off]" } else { "" })
                }
                Entry::Check { label, checked, .. } => {
                    format!("[{}] {label}", if *checked { "x" } else { " " })
                }
                Entry::Separator => "---".into(),
                Entry::Submenu { label, entries } => {
                    format!("{label} > {}", labels(entries).join(" | "))
                }
            })
            .collect()
    }

    #[test]
    fn the_streamer_switch_turns_it_on_or_off() {
        let mut model = model(None, None, None, false);
        let check = |model: &TrayModel| {
            model
                .entries()
                .into_iter()
                .find_map(|e| match e {
                    Entry::Check {
                        checked, action, ..
                    } => Some((checked, TrayAction::parse(&action))),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(check(&model), (false, Some(TrayAction::SetStreamer(true))));
        model.streamer = true;
        assert_eq!(check(&model), (true, Some(TrayAction::SetStreamer(false))));
    }

    #[test]
    fn idle_menu_has_recent_hosts_groups_show_and_quit() {
        let hosts = hosts("Host web w1\nHost db\n");
        let model = model(Some(&data()), Some(&hosts), None, false);
        assert_eq!(
            labels(&model.entries()),
            [
                "sshelf: nothing running [off]",
                "[ ] Streamer mode",
                "---",
                // Deduplicated by host (w1 is web), missing hosts dropped.
                "Connect > db | web",
                "---",
                "Groups [off]",
                "Start Work",
                "---",
                "Show sshelf",
                "Quit",
            ]
        );
        assert_eq!(model.tooltip, "sshelf");
        assert_eq!(model.running, 0);
        assert_eq!(model.status, Status::Idle);
    }

    #[test]
    fn active_tunnels_show_who_holds_them() {
        let hosts = hosts("Host web\nHost db\n");
        let states = Snapshot {
            tunnels: BTreeMap::from([
                ("t-a".into(), status(TunnelState::Running, &[Owner::Manual])),
                (
                    "t-b".into(),
                    status(TunnelState::Running, &[Owner::Group("g-1".into())]),
                ),
                (
                    "t-c".into(),
                    status(
                        TunnelState::Failed("x".into()),
                        &[Owner::Group("g-1".into())],
                    ),
                ),
            ]),
        };
        let model = model(Some(&data()), Some(&hosts), Some(&states), true);
        let shown = labels(&model.entries());
        assert_eq!(shown[0], "sshelf: 2 running · 1 failed [off]");
        assert_eq!(shown[1], "A host key waits for you…");
        assert!(
            shown.contains(&"Stop Work (1/2 running)".to_string()),
            "{shown:?}"
        );
        assert!(shown.contains(&"Stop API".to_string()), "{shown:?}");
        assert!(
            shown.contains(&"DB (used by Work) [off]".to_string()),
            "{shown:?}"
        );
        assert!(
            shown.contains(&"Cache (used by Work) [off]".to_string()),
            "{shown:?}"
        );
        assert_eq!(model.running, 2);
        assert_eq!(model.failed, 1);
        assert_eq!(model.status, Status::Failed);
        assert_eq!(model.tooltip, "sshelf · 2 tunnels running · 1 failed");
        assert_eq!(model.tunnels[0].port, Some(6443));
    }

    #[test]
    fn nothing_launched_yet() {
        let model = model(None, None, None, false);
        assert_eq!(
            labels(&model.entries())[3],
            "Connect > No sessions launched from sshelf yet [off]"
        );
    }

    #[test]
    fn actions_round_trip_through_menu_ids() {
        for action in [
            TrayAction::Connect("web:22".into()),
            TrayAction::StartGroup("g-1".into()),
            TrayAction::StopGroup("g-1".into()),
            TrayAction::StopTunnel("t-a".into()),
            TrayAction::ShowTunnel("t-a".into()),
            TrayAction::Show,
            TrayAction::Quit,
            TrayAction::SetStreamer(true),
            TrayAction::SetStreamer(false),
        ] {
            assert_eq!(TrayAction::parse(&action.id()), Some(action));
        }
        assert_eq!(TrayAction::parse("bogus"), None);
        assert_eq!(
            TrayAction::StopTunnel("t-a".into()).command(),
            Some(Command::StopTunnel("t-a".into()))
        );
        assert_eq!(TrayAction::Quit.command(), None);
    }
}
