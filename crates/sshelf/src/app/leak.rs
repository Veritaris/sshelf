//! The leak test (SPEC.md §12 M11 *Nothing leaks*): sshelf with streamer
//! mode on, over a config, tunnels and notes whose sensitive values are all
//! distinctive markers. No screen visited may show a marker in its drawn
//! text, its accessibility names, the tray menus, the window title or a
//! toast. With streamer mode off the same screens do show them, so the test
//! can't pass by showing nothing.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use sshelf_core::metadata::{HostMeta, HostsFile};
use sshelf_core::paths::Paths;
use sshelf_core::storage::Store;
use sshelf_core::tunnels::{Forward, ForwardKind, Group, Tunnel, TunnelsFile};

use super::{SshelfApp, Startup};
use crate::sidebar::View;
use crate::test_util::{self, fake_ssh_binary};
use crate::tray::Entry;
use crate::tray_menu;

/// The home folder's name: every path under it carries the marker.
const HOME: &str = "lkq-home";

/// Every sensitive value below, each one distinctive.
const MARKERS: &[&str] = &[
    HOME,
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
];

const CONFIG: &str = "\
Host web
    HostName lkq-host.internal
    User lkq-user
    Port 52817
    IdentityFile ~/.ssh/lkq_key
    ProxyJump lkq-jump.example.org

Host db
    HostName 10.77.3.41
    ProxyCommand nc lkq-proxy.example.org 22
";

/// The app over the marker config, with fake-ssh as its ssh and streamer
/// mode as `streamer` (no tray: tests never create one).
fn app(root: &Path, streamer: bool) -> Harness<'static, SshelfApp> {
    let home = root.join(HOME);
    let paths = Paths::new(&home, home.join(".config").join("sshelf"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::create_dir_all(&paths.app_dir).unwrap();
    fs::write(&paths.ssh_config, CONFIG).unwrap();
    fs::write(
        paths.app_dir.join("settings.toml"),
        format!(
            "version = 1\nssh_path = '{}'\nreduce_motion = true\ntour = \"done\"\n\
             streamer_mode = {streamer}\n",
            fake_ssh_binary().display()
        ),
    )
    .unwrap();
    let store = Store::new(&paths.app_dir);
    let mut hosts = HostsFile::default();
    hosts.hosts.insert(
        "web".into(),
        HostMeta {
            tags: vec!["prod".into()],
            notes: "Jump via lkq-jump.example.org, then 10.77.3.41.".into(),
            ..HostMeta::default()
        },
    );
    store.save(&hosts).unwrap();
    store
        .save(&TunnelsFile {
            tunnels: vec![Tunnel {
                id: "t-web".into(),
                name: "web admin".into(),
                host: "web".into(),
                forwards: vec![Forward {
                    kind: ForwardKind::L,
                    bind: "127.0.0.1".into(),
                    listen: 54123,
                    target: Some("lkq-target.internal:54321".into()),
                }],
                auto_reconnect: false,
            }],
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

/// Puts the app on a screen.
type Setup = fn(&mut SshelfApp);

/// The screens and dialogs visited, by name, each set up on the app. Each
/// M11 step adds the screens it masks.
fn screens() -> Vec<(&'static str, Setup)> {
    vec![
        ("settings", |app| app.view = View::Settings),
        ("hosts", |app| app.view = View::Hosts),
    ]
}

/// What each screen shows: (screen, its text), the tray last.
fn visit(harness: &mut Harness<'_, SshelfApp>) -> Vec<(&'static str, String)> {
    let mut shown = Vec::new();
    for (name, setup) in screens() {
        setup(harness.state_mut());
        frames(harness);
        shown.push((name, window_text(harness)));
    }
    shown.push(("tray", tray_text(harness.state())));
    shown
}

fn markers_in(text: &str) -> Vec<&'static str> {
    MARKERS
        .iter()
        .copied()
        .filter(|m| text.contains(m))
        .collect()
}

#[test]
fn nothing_leaks_with_streamer_mode_on() {
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), true);
    let leaks: Vec<String> = visit(&mut harness)
        .into_iter()
        .filter_map(|(screen, text)| {
            let found = markers_in(&text);
            (!found.is_empty()).then(|| format!("{screen}: {found:?}"))
        })
        .collect();
    assert!(leaks.is_empty(), "leaked:\n{}", leaks.join("\n"));
}

/// The markers are on screen to begin with: streamer mode off shows them.
#[test]
fn the_markers_show_with_streamer_mode_off() {
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), false);
    let shown: String = visit(&mut harness)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert!(shown.contains(HOME), "the home path shows in Settings");
}
