//! Tray icon and menu (SPEC.md §10 "Tray"): recent hosts to connect to,
//! groups to start or stop, active tunnels to stop, Show and Quit.
//!
//! [`model`] builds the menu from app state (pure, tested); [`Tray`] owns the
//! platform icon, rebuilds its menu only when the model changes, and turns
//! clicks into [`TrayAction`]s. Everything here runs on the main thread from
//! `App::logic`, which eframe keeps calling while the window is hidden; tray
//! clicks request a repaint to get there.

use std::sync::mpsc::{self, Receiver};

use sshconf::HostList;
use sshelf_core::core::{AppData, Command};
use sshelf_core::supervisor::{GroupState, Owner, Snapshot, TunnelState};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::close_guard;
use crate::icon;

/// Recent hosts in the menu.
pub const RECENT_HOSTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    Connect(String),
    StartGroup(String),
    StopGroup(String),
    StopTunnel(String),
    Show,
    Quit,
}

impl TrayAction {
    /// The menu item id.
    pub fn id(&self) -> String {
        match self {
            TrayAction::Connect(alias) => format!("connect:{alias}"),
            TrayAction::StartGroup(id) => format!("group-start:{id}"),
            TrayAction::StopGroup(id) => format!("group-stop:{id}"),
            TrayAction::StopTunnel(id) => format!("tunnel-stop:{id}"),
            TrayAction::Show => "show".into(),
            TrayAction::Quit => "quit".into(),
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
            TrayAction::Show | TrayAction::Quit => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A clickable item, or a disabled line of information (`None`).
    Item {
        label: String,
        action: Option<TrayAction>,
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
        action: Some(action),
    }
}

fn info(label: impl Into<String>) -> Entry {
    Entry::Item {
        label: label.into(),
        action: None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayModel {
    pub entries: Vec<Entry>,
    pub tooltip: String,
    /// Tunnels starting or running (the icon shows a dot).
    pub running: usize,
}

/// The menu for the current state.
pub fn model(
    data: Option<&AppData>,
    hosts: Option<&HostList>,
    states: Option<&Snapshot>,
) -> TrayModel {
    let empty = Snapshot::default();
    let states = states.unwrap_or(&empty);
    let mut entries = Vec::new();

    // Recent hosts launched from sshelf that still exist, newest first.
    let mut recent: Vec<String> = Vec::new();
    if let (Some(data), Some(hosts)) = (data, hosts) {
        for launch in &data.history.entries {
            if let Some(host) = hosts.find(&launch.alias)
                && !recent.iter().any(|r| r == host.primary())
            {
                recent.push(host.primary().to_string());
            }
            if recent.len() == RECENT_HOSTS {
                break;
            }
        }
    }
    let connect: Vec<Entry> = if recent.is_empty() {
        vec![info("No sessions launched from sshelf yet")]
    } else {
        recent
            .into_iter()
            .map(|alias| item(alias.clone(), TrayAction::Connect(alias)))
            .collect()
    };
    entries.push(Entry::Submenu {
        label: "Connect".into(),
        entries: connect,
    });

    if let Some(data) = data
        && !data.tunnels.groups.is_empty()
    {
        entries.push(Entry::Separator);
        entries.push(info("Groups"));
        for group in &data.tunnels.groups {
            let name = &group.name;
            entries.push(match states.group_state(group) {
                GroupState::Stopped => item(
                    format!("Start {name}"),
                    TrayAction::StartGroup(group.id.clone()),
                ),
                GroupState::Starting => item(
                    format!("Stop {name} (starting)"),
                    TrayAction::StopGroup(group.id.clone()),
                ),
                GroupState::Running => item(
                    format!("Stop {name}"),
                    TrayAction::StopGroup(group.id.clone()),
                ),
                GroupState::Degraded { running, total } => item(
                    format!("Stop {name} ({running}/{total} running)"),
                    TrayAction::StopGroup(group.id.clone()),
                ),
            });
        }
    }

    let mut active = Vec::new();
    if let Some(data) = data {
        for tunnel in &data.tunnels.tunnels {
            let Some(status) = states.tunnels.get(&tunnel.id) else {
                continue;
            };
            let name = &tunnel.name;
            let entry = match &status.state {
                TunnelState::Stopped => continue,
                TunnelState::Stopping => info(format!("{name} (stopping)")),
                state if status.owners.contains(&Owner::Manual) => {
                    let suffix = match state {
                        TunnelState::Starting => " (starting)",
                        TunnelState::Failed(_) if status.reconnect.is_some() => " (reconnecting)",
                        TunnelState::Failed(_) => " (failed)",
                        _ => "",
                    };
                    item(
                        format!("Stop {name}{suffix}"),
                        TrayAction::StopTunnel(tunnel.id.clone()),
                    )
                }
                _ => {
                    let groups: Vec<&str> = status
                        .owners
                        .iter()
                        .filter_map(|o| match o {
                            Owner::Group(id) => data.tunnels.group(id).map(|g| g.name.as_str()),
                            Owner::Manual => None,
                        })
                        .collect();
                    info(format!("{name} (used by {})", groups.join(", ")))
                }
            };
            active.push(entry);
        }
    }
    if !active.is_empty() {
        entries.push(Entry::Separator);
        entries.push(info("Active tunnels"));
        entries.extend(active);
    }

    entries.push(Entry::Separator);
    entries.push(item("Show sshelf", TrayAction::Show));
    entries.push(item("Quit", TrayAction::Quit));

    let running = close_guard::running_count(Some(states));
    let tooltip = match running {
        0 => "sshelf".to_string(),
        1 => "sshelf: 1 tunnel running".to_string(),
        n => format!("sshelf: {n} tunnels running"),
    };
    TrayModel {
        entries,
        tooltip,
        running,
    }
}

/// What the platform tray reports.
enum TrayEvent {
    Menu(String),
    /// The icon itself was clicked.
    Clicked,
}

/// The platform tray icon.
pub struct Tray {
    icon: TrayIcon,
    shown: Option<TrayModel>,
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
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = tx.send(TrayEvent::Clicked);
                icon_ctx.request_repaint();
            }
        }));
        let icon = TrayIconBuilder::new()
            .with_icon(tray_icon(false)?)
            .with_tooltip("sshelf")
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            shown: None,
            events,
        })
    }

    /// Shows `model`, rebuilding the menu only when it changed.
    pub fn update(&mut self, model: TrayModel) {
        if self.shown.as_ref() == Some(&model) {
            return;
        }
        let previous = self.shown.as_ref();
        if previous.is_none_or(|p| (p.running > 0) != (model.running > 0))
            && let Ok(icon) = tray_icon(model.running > 0)
        {
            let _ = self.icon.set_icon(Some(icon));
        }
        if previous.is_none_or(|p| p.tooltip != model.tooltip) {
            let _ = self.icon.set_tooltip(Some(&model.tooltip));
        }
        if previous.is_none_or(|p| p.entries != model.entries) {
            let menu = Menu::new();
            for entry in build(&model.entries) {
                let _ = menu.append(entry.as_ref());
            }
            self.icon.set_menu(Some(Box::new(menu)));
        }
        self.shown = Some(model);
    }

    /// Clicks since the last call; a click on the icon shows the window.
    pub fn actions(&mut self) -> Vec<TrayAction> {
        self.events
            .try_iter()
            .filter_map(|event| match event {
                TrayEvent::Menu(id) => TrayAction::parse(&id),
                TrayEvent::Clicked => Some(TrayAction::Show),
            })
            .collect()
    }
}

fn tray_icon(running: bool) -> Result<tray_icon::Icon, String> {
    tray_icon::Icon::from_rgba(icon::rgba(running), icon::SIZE, icon::SIZE)
        .map_err(|e| e.to_string())
}

/// Menu items for `entries`.
fn build(entries: &[Entry]) -> Vec<Box<dyn IsMenuItem>> {
    entries
        .iter()
        .map(|entry| -> Box<dyn IsMenuItem> {
            match entry {
                Entry::Item {
                    label,
                    action: Some(action),
                } => Box::new(MenuItem::with_id(action.id(), menu_text(label), true, None)),
                Entry::Item {
                    label,
                    action: None,
                } => Box::new(MenuItem::new(menu_text(label), false, None)),
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
    use sshelf_core::tunnels::{Group, Tunnel};

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
            data.tunnels.tunnels.push(Tunnel {
                id: id.into(),
                name: name.into(),
                host: host.into(),
                forwards: Vec::new(),
                auto_reconnect: false,
            });
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
                Entry::Separator => "---".into(),
                Entry::Submenu { label, entries } => {
                    format!("{label} > {}", labels(entries).join(" | "))
                }
            })
            .collect()
    }

    #[test]
    fn idle_menu_has_recent_hosts_groups_show_and_quit() {
        let hosts = hosts("Host web w1\nHost db\n");
        let model = model(Some(&data()), Some(&hosts), None);
        assert_eq!(
            labels(&model.entries),
            [
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
        let model = model(Some(&data()), Some(&hosts), Some(&states));
        let shown = labels(&model.entries);
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
        assert_eq!(model.tooltip, "sshelf: 2 tunnels running");
    }

    #[test]
    fn nothing_launched_yet() {
        let model = model(None, None, None);
        assert_eq!(
            labels(&model.entries)[0],
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
            TrayAction::Show,
            TrayAction::Quit,
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
