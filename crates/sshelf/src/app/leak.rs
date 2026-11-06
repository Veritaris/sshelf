//! The leak test (SPEC.md §12 M11 *Nothing leaks*): sshelf with streamer
//! mode on, over a config, tunnels and notes whose sensitive values are all
//! distinctive markers. No screen visited may show a marker in its drawn
//! text, its accessibility names, the tray menus, the window title or a
//! toast. With streamer mode off the same screens do show them, so the test
//! can't pass by showing nothing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sshelf_core::core::{ChangedKeyPrompt, HostKeyPrompt};
use sshelf_core::health::{Health, HealthReport, HealthSnapshot};
use sshelf_core::history::{History, Launch};
use sshelf_core::hostkeys::{ChangedKey, KeyInfo, KeyTarget, UnknownKey};
use sshelf_core::metadata::{HostMeta, HostsFile};
use sshelf_core::paths::Paths;
use sshelf_core::settings::HostListStyle;
use sshelf_core::storage::Store;
use sshelf_core::supervisor::{LogLine, Owner, Snapshot, TunnelState, TunnelStatus};
use sshelf_core::tunnels::{Forward, ForwardKind, Group, Tunnel, TunnelsFile};

use super::{SshelfApp, Startup};
use crate::hostkey_modal::HostKeyModals;
use crate::sidebar::View;
use crate::test_util::{self, fake_ssh_binary};
use crate::tray::Entry;
use crate::tray_menu;

/// The home folder's name: every path under it carries the marker.
const HOME: &str = "lkq-home";

/// Every sensitive value below, each one distinctive.
const MARKERS: &[&str] = &[
    HOME,
    "LkqfpXo3",
    "ILkqpkXo3",
    "lkq-host.internal",
    "10.77.3.41",
    "lkq-user",
    "52817",
    "lkq_key",
    "lkq-jump.example.org",
    "lkq-proxy.example.org",
    "lkq-target.internal",
    "54321",
    "54123",
    "10.77.9.9",
];

/// Aliases and tags: shown, unless *Hide host aliases and tags* is on.
const NAME_MARKERS: &[&str] = &["lkqa-", "lkqtag"];

/// The host most screens show.
const WEB: &str = "lkqa-web";

/// An alias that is an IP address is masked whenever streamer mode is on.
const CONFIG: &str = "\
Host lkqa-web
    HostName lkq-host.internal
    User lkq-user
    Port 52817
    IdentityFile ~/.ssh/lkq_key
    ProxyJump lkq-jump.example.org

Host lkqa-db
    HostName 10.77.3.41
    ProxyCommand nc lkq-proxy.example.org 22

Host 10.77.9.9
    User lkq-user
";

/// The app over the marker config, with fake-ssh as its ssh, streamer mode
/// as `streamer` and hidden aliases as `hide` (no tray: tests never create
/// one).
fn app(root: &Path, streamer: bool, hide: bool) -> Harness<'static, SshelfApp> {
    let home = root.join(HOME);
    let paths = Paths::new(&home, home.join(".config").join("sshelf"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::create_dir_all(&paths.app_dir).unwrap();
    fs::write(&paths.ssh_config, CONFIG).unwrap();
    fs::write(
        paths.app_dir.join("settings.toml"),
        format!(
            "version = 1\nssh_path = '{}'\nreduce_motion = true\ntour = \"done\"\n\
             streamer_mode = {streamer}\nstreamer_hide_aliases = {hide}\n",
            fake_ssh_binary().display()
        ),
    )
    .unwrap();
    let store = Store::new(&paths.app_dir);
    let mut hosts = HostsFile::default();
    hosts.hosts.insert(
        WEB.into(),
        HostMeta {
            tags: vec!["lkqtag".into()],
            notes: "Jump via lkq-jump.example.org, then 10.77.3.41.".into(),
            ..HostMeta::default()
        },
    );
    store.save(&hosts).unwrap();
    // Launched before: the tray's recent hosts show both.
    let launch = |alias: &str| Launch {
        alias: alias.into(),
        at: "2026-01-02T03:04:05Z".into(),
    };
    store
        .save(&History {
            entries: vec![launch(WEB), launch("10.77.9.9")],
        })
        .unwrap();
    store
        .save(&TunnelsFile {
            tunnels: vec![Tunnel::ssh(
                "t-web",
                "web admin",
                WEB,
                vec![Forward {
                    kind: ForwardKind::L,
                    bind: "127.0.0.1".into(),
                    listen: 54123,
                    target: Some("lkq-target.internal:54321".into()),
                }],
            )],
            groups: vec![Group {
                id: "g-work".into(),
                name: "Work".into(),
                tunnels: vec!["t-web".into()],
                start_on_launch: false,
            }],
        })
        .unwrap();
    let temp = root.join("temp");
    fs::create_dir_all(&temp).unwrap();
    let startup = Startup {
        tray: false,
        fake_ssh: None,
        temp,
        user_paths: Some(paths),
        show: None,
    };
    let mut harness = test_util::harness()
        .with_size([1280.0, 800.0])
        .build_eframe(|cc| SshelfApp::with(cc, startup));
    settle(&mut harness, "the config and data", |app| {
        app.data.is_some() && app.config.as_ref().is_some_and(|c| c.hosts().is_some())
    });
    harness
}

/// Steps the app until `done` (its Core runs in the background).
fn settle(harness: &mut Harness<'_, SshelfApp>, what: &str, done: impl Fn(&SshelfApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A few frames, for anything the last change starts (resolves, fades).
fn frames(harness: &mut Harness<'_, SshelfApp>) {
    for _ in 0..4 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Everything the window shows now: drawn text, accessibility names and
/// values, and any window title it set.
fn window_text(harness: &Harness<'_, SshelfApp>) -> String {
    let mut text = test_util::drawn_text(harness);
    text.push_str(&test_util::accessible_text(harness));
    for viewport in harness.output().viewport_output.values() {
        text.push_str(&viewport.builder.title.clone().unwrap_or_default());
        for command in &viewport.commands {
            if let egui::ViewportCommand::Title(title) = command {
                text.push_str(title);
            }
        }
    }
    text
}

/// Both tray menus and the tooltip: the native entries, and the sshelf
/// menu drawn on its own.
fn tray_text(app: &SshelfApp) -> String {
    fn entries(list: &[Entry], out: &mut String) {
        for entry in list {
            match entry {
                Entry::Item { label, .. } | Entry::Check { label, .. } => out.push_str(label),
                Entry::Submenu { label, entries: e } => {
                    out.push_str(label);
                    entries(e, out);
                }
                Entry::Separator => {}
            }
            out.push('\n');
        }
    }
    let model = app.tray_model.clone();
    let mut text = model.tooltip.clone();
    entries(&model.entries(), &mut text);
    let size = tray_menu::size(&model);
    let menu = test_util::harness().with_size(size).build_ui(move |ui| {
        tray_menu::picture(
            ui,
            egui::Rect::from_min_size(egui::Pos2::ZERO, size),
            &model,
        )
    });
    let mut menu = menu;
    menu.step();
    text.push_str(&test_util::drawn_text(&menu));
    text.push_str(&test_util::accessible_text(&menu));
    text
}

/// A fingerprint and a public key whose hashes carry a marker.
const FINGERPRINT: &str = "SHA256:LkqfpXo3n8Qm5Vz1Yt7Wc2Rb4Ud6Ae9Gh0Jk2Lm4Np6";
const PUBLIC_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILkqpkXo3n8Qm5Vz1Yt7Wc2Rb4Ud6Ae9Gh0Jk2Lm4N me@x";

/// What ssh said when it failed, with markers in it.
const SSH_ERROR: &str = "ssh: connect to host lkq-host.internal port 52817: Connection refused";

/// The tunnel failed, with markers in its log; the other states (running)
/// are set where a screen needs them.
fn tunnel_states(state: TunnelState) -> Arc<Snapshot> {
    let line = |text: &str| LogLine {
        text: text.into(),
        debug: false,
        at: SystemTime::now(),
    };
    Arc::new(Snapshot {
        tunnels: BTreeMap::from([(
            "t-web".to_string(),
            TunnelStatus {
                state,
                owners: BTreeSet::from([Owner::Manual]),
                log: VecDeque::from([
                    line("debug1: Connecting to lkq-host.internal [10.77.3.41] port 52817."),
                    line("channel 2: open failed: connect to lkq-target.internal port 54321"),
                    line(SSH_ERROR),
                ]),
                pid: None,
                failure: None,
                reconnect: None,
                running_since: None,
            },
        )]),
    })
}

fn key_target(home: &Path) -> KeyTarget {
    KeyTarget {
        destination: WEB.into(),
        hostname: "lkq-host.internal".into(),
        port: 52817,
        lookup: "[lkq-host.internal]:52817".into(),
        user_files: vec![home.join(".ssh").join("known_hosts")],
        global_files: Vec::new(),
        jumps: vec!["lkq-jump.example.org".into()],
    }
}

fn key_info() -> KeyInfo {
    KeyInfo {
        bits: "256".into(),
        fingerprint: FINGERPRINT.into(),
        kind: "ED25519".into(),
    }
}

/// Clicks the first widget named `label`.
fn click(harness: &mut Harness<'_, SshelfApp>, label: &str) {
    harness.get_all_by_label(label).next().unwrap().click();
    frames(harness);
}

/// Puts the app on a screen.
type Setup = fn(&mut Harness<'static, SshelfApp>);

/// Back to a plain hosts view: no dialogs, editors, errors or toasts.
fn reset(harness: &mut Harness<'static, SshelfApp>) {
    let app = harness.state_mut();
    app.view = View::Hosts;
    app.error = None;
    app.close.dismiss();
    app.hostkeys = HostKeyModals::default();
    app.hosts.editor = Default::default();
    app.tunnels.close_editor();
    app.groups.close_editor();
    app.toasts = Default::default();
    app.tunnel_states = Some(tunnel_states(TunnelState::Failed(SSH_ERROR.into())));
    list_style(app, HostListStyle::List);
    frames(harness);
}

fn list_style(app: &mut SshelfApp, style: HostListStyle) {
    if let Some(data) = app.data.as_mut() {
        Arc::make_mut(data).settings.host_list_style = style;
    }
}

/// The screens and dialogs visited, by name, each set up from a plain hosts
/// view.
fn screens() -> Vec<(&'static str, Setup)> {
    vec![
        ("settings", |h| h.state_mut().view = View::Settings),
        ("hosts", |h| h.state_mut().view = View::Hosts),
        ("host detail", |h| h.state_mut().hosts.select(WEB)),
        ("host cards", |h| {
            let app = h.state_mut();
            list_style(app, HostListStyle::Cards);
            app.hosts.select(WEB);
        }),
        ("health", |h| {
            h.state_mut().hosts.select(WEB);
            let health = HealthSnapshot {
                reports: BTreeMap::from([(
                    WEB.to_string(),
                    HealthReport {
                        health: Health::Unreachable(SSH_ERROR.into()),
                        checked_at: SystemTime::now(),
                    },
                )]),
                checking: BTreeSet::new(),
            };
            h.state_mut().hosts.on_health(Arc::new(health));
        }),
        ("effective config", |h| {
            h.state_mut().hosts.select(WEB);
            frames(h);
            click(h, "Effective config");
        }),
        ("host tunnels", |h| {
            h.state_mut().hosts.select(WEB);
            frames(h);
            click(h, "Tunnels · 1");
        }),
        ("host editor", |h| {
            h.state_mut().hosts.select(WEB);
            frames(h);
            click(h, "Edit");
        }),
        ("new host", |h| click(h, "New host")),
        ("key dialog", |h| {
            let app = h.state_mut();
            let config = app.config.clone().unwrap();
            app.hosts.editor.open_keys(&config, WEB);
        }),
        ("tunnels", |h| {
            let app = h.state_mut();
            app.view = View::Tunnels;
            app.tunnels.select("t-web");
        }),
        ("tunnel editor", |h| {
            let app = h.state_mut();
            app.view = View::Tunnels;
            app.tunnels.select("t-web");
            app.tunnel_states = Some(tunnel_states(TunnelState::Stopped));
            frames(h);
            click(h, "Edit");
        }),
        ("groups", |h| {
            let app = h.state_mut();
            app.view = View::Groups;
            app.groups.select("g-work");
        }),
        ("unknown host key", |h| {
            let home = h.state().data.as_ref().unwrap().app_dir.clone();
            let prompt = HostKeyPrompt {
                id: 1,
                purpose: format!("connecting to {WEB}"),
                unknown: UnknownKey {
                    target: key_target(&home),
                    lines: vec![format!("[lkq-host.internal]:52817 {PUBLIC_KEY}")],
                    keys: vec![key_info()],
                    hop_of: None,
                },
            };
            h.state_mut().hostkeys.on_unknown(Arc::new(prompt));
        }),
        ("changed host key", |h| {
            let home = h.state().data.as_ref().unwrap().app_dir.clone();
            let prompt = ChangedKeyPrompt {
                id: 2,
                subject: format!("host “{WEB}”"),
                changed: ChangedKey {
                    target: key_target(&home),
                    user_files: vec![home.join("known_hosts")],
                    old: vec![key_info()],
                    new: vec![key_info()],
                    new_lines: vec![format!("[lkq-host.internal]:52817 {PUBLIC_KEY}")],
                    hop_of: None,
                },
            };
            h.state_mut().hostkeys.on_changed(Arc::new(prompt));
        }),
        ("close dialog", |h| {
            let app = h.state_mut();
            app.tunnel_states = Some(tunnel_states(TunnelState::Running));
            app.close.ask();
        }),
        ("error", |h| h.state_mut().error = Some(SSH_ERROR.into())),
        ("toast", |h| {
            h.state_mut()
                .toasts
                .push(format!("Saved {WEB}: lkq-host.internal, port 52817"));
        }),
    ]
}

/// What each screen shows: (screen, its text), the tray last.
fn visit(harness: &mut Harness<'static, SshelfApp>) -> Vec<(&'static str, String)> {
    let mut shown = Vec::new();
    for (name, setup) in screens() {
        reset(harness);
        setup(harness);
        frames(harness);
        shown.push((name, window_text(harness)));
    }
    reset(harness);
    harness.state_mut().tunnel_states = Some(tunnel_states(TunnelState::Running));
    frames(harness);
    shown.push(("tray", tray_text(harness.state())));
    shown
}

fn markers_in(text: &str, markers: &[&'static str]) -> Vec<&'static str> {
    markers
        .iter()
        .copied()
        .filter(|m| text.contains(m))
        .collect()
}

/// Every screen of the app (streamer mode on, aliases hidden as `hide`)
/// that shows one of `markers`, with the ones it shows.
fn leaks(hide: bool, markers: &[&'static str]) -> Vec<String> {
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), true, hide);
    visit(&mut harness)
        .into_iter()
        .filter_map(|(screen, text)| {
            let found = markers_in(&text, markers);
            (!found.is_empty()).then(|| format!("{screen}: {found:?}"))
        })
        .collect()
}

#[test]
fn nothing_leaks_with_streamer_mode_on() {
    let leaks = leaks(false, MARKERS);
    assert!(leaks.is_empty(), "leaked:\n{}", leaks.join("\n"));
}

/// *Hide host aliases and tags*: aliases and tags are hidden too.
#[test]
fn nothing_leaks_with_hidden_aliases() {
    let markers: Vec<&str> = MARKERS.iter().chain(NAME_MARKERS).copied().collect();
    let leaks = leaks(true, &markers);
    assert!(leaks.is_empty(), "leaked:\n{}", leaks.join("\n"));
}

/// The markers are on screen to begin with: streamer mode off shows them.
#[test]
fn the_markers_show_with_streamer_mode_off() {
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), false, false);
    let blank: Vec<&str> = visit(&mut harness)
        .into_iter()
        .filter(|(_, text)| markers_in(text, MARKERS).is_empty())
        .map(|(screen, _)| screen)
        .collect();
    assert!(blank.is_empty(), "no marker shown on: {blank:?}");
}
