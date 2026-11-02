//! A full core session against temp dirs and `fake-ssh`: load, resolve, edit
//! metadata, handle orphans, connect, react to config changes — and never
//! write to the ssh config.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use sshelf_core::core::{Command, ConfigState, Core, Event};
use sshelf_core::metadata::{HostMeta, HostsFile};
use sshelf_core::paths::Paths;
use sshelf_core::settings::Settings;
use sshelf_core::storage::Store;
use sshelf_core::supervisor::{Snapshot, TunnelState};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Group, Tunnel};

const WAIT: Duration = Duration::from_secs(10);

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

/// Waits for the first event `pick` accepts, skipping others.
fn wait_for<T>(
    events: &Receiver<Event>,
    what: &str,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = events
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

fn host_aliases(event: Event) -> Option<Vec<String>> {
    let Event::Config(snapshot) = event else {
        return None;
    };
    let ConfigState::Loaded { hosts, .. } = &snapshot.state else {
        return None;
    };
    Some(
        hosts
            .hosts
            .iter()
            .map(|h| h.primary().to_string())
            .collect(),
    )
}

fn fingerprint(path: &Path) -> (Vec<u8>, SystemTime) {
    (
        fs::read(path).unwrap(),
        fs::metadata(path).unwrap().modified().unwrap(),
    )
}

#[test]
fn full_session_never_writes_the_ssh_config() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let paths = Paths::new(&home, tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::write(
        &paths.ssh_config,
        "# mine\r\nHost web\r\n    HostName web.example.com\r\nHost db\r\n",
    )
    .unwrap();

    let record = tmp.path().join("launches.txt");
    let store = Store::new(&paths.app_dir);
    store
        .save(&Settings {
            ssh_path: Some(fake_ssh()),
            terminal_command: format!(
                "\"{}\" --record \"{}\" {{alias}}",
                fake_ssh().display(),
                record.display()
            ),
        })
        .unwrap();
    let mut hosts = HostsFile::default();
    hosts.hosts.insert(
        "gone".into(),
        HostMeta {
            notes: "for a removed host".into(),
            ..HostMeta::default()
        },
    );
    store.save(&hosts).unwrap();

    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));

    let aliases = wait_for(&events, "the config", host_aliases);
    assert_eq!(aliases, ["web", "db"]);
    let orphans = wait_for(&events, "app data", |e| match e {
        Event::Data(data) => Some(data.orphans.clone()),
        _ => None,
    });
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].key, "gone");
    let version = wait_for(&events, "ssh info", |e| match e {
        Event::Ssh(info) => Some(info.version.clone()),
        _ => None,
    });
    assert_eq!(version, Some(Ok("OpenSSH_fake_1.0, fake-ssh".to_string())));

    core.send(Command::Resolve("web".into()));
    let resolved = wait_for(&events, "ssh -G", |e| match e {
        Event::Resolved { alias, result, .. } if alias == "web" => Some(result),
        _ => None,
    });
    assert_eq!(resolved.unwrap().get("hostname"), Some("web.example.com"));

    core.send(Command::SetMetadata {
        alias: "web".into(),
        meta: HostMeta {
            tags: vec!["prod".into()],
            favorite: true,
            ..HostMeta::default()
        },
    });
    let data = wait_for(&events, "saved metadata", |e| match e {
        Event::Data(data) if data.hosts.hosts.contains_key("web") => Some(data),
        _ => None,
    });
    assert!(data.hosts.hosts["web"].favorite);
    let on_disk = fs::read_to_string(paths.app_dir.join("hosts.toml")).unwrap();
    assert!(on_disk.contains("[hosts.web]"), "{on_disk}");

    core.send(Command::RelinkOrphan {
        key: "gone".into(),
        to: "db".into(),
    });
    let data = wait_for(&events, "relinked orphan", |e| match e {
        Event::Data(data) if data.orphans.is_empty() => Some(data),
        _ => None,
    });
    assert_eq!(data.hosts.hosts["db"].notes, "for a removed host");

    core.send(Command::Connect("web".into()));
    wait_for(&events, "launch", |e| match e {
        Event::Launched { alias } => Some(alias),
        _ => None,
    });
    let data = wait_for(&events, "history", |e| match e {
        Event::Data(data) if !data.history.entries.is_empty() => Some(data),
        _ => None,
    });
    assert_eq!(data.history.entries[0].alias, "web");
    let deadline = Instant::now() + WAIT;
    while !record.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let launched = fs::read_to_string(&record).expect("the launch was recorded");
    assert_eq!(launched.trim_end().split('\x1f').next_back(), Some("web"));

    // External edit: picked up by the watcher.
    fs::write(&paths.ssh_config, "Host web\r\nHost db\r\nHost new\r\n").unwrap();
    let before = fingerprint(&paths.ssh_config);
    let aliases = wait_for(&events, "the reloaded config", host_aliases);
    assert_eq!(aliases, ["web", "db", "new"]);

    // More writes to app data; the ssh config stays untouched.
    core.send(Command::SetMetadata {
        alias: "new".into(),
        meta: HostMeta {
            notes: "added later".into(),
            ..HostMeta::default()
        },
    });
    wait_for(&events, "metadata for the new host", |e| match e {
        Event::Data(data) if data.hosts.hosts.contains_key("new") => Some(()),
        _ => None,
    });
    core.send(Command::DeleteOrphan { key: "nope".into() });
    wait_for(&events, "error for a missing entry", |e| match e {
        Event::Error(message) => Some(message),
        _ => None,
    });
    drop(core);
    assert_eq!(fingerprint(&paths.ssh_config), before);
}

fn tunnel_state(events: &Receiver<Event>, id: &str, want: TunnelState) -> Arc<Snapshot> {
    wait_for(events, &format!("{id} {want:?}"), |e| match e {
        Event::Tunnels(s) if s.state(id) == want => Some(s),
        _ => None,
    })
}

#[test]
fn tunnels_and_groups_through_the_core() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let paths = Paths::new(&home, tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::write(&paths.ssh_config, "Host ok-a\nHost ok-b\n").unwrap();
    let before = fingerprint(&paths.ssh_config);
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(fake_ssh()),
            ..Settings::default()
        })
        .unwrap();

    let free = || {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let port = free();
    let tunnel = Tunnel {
        id: "t-1".into(),
        name: "API".into(),
        host: "ok-a".into(),
        forwards: vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: port,
            target: Some("localhost:6443".into()),
        }],
    };

    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    wait_for(&events, "the config", host_aliases);

    // Invalid tunnels are refused with the reasons.
    core.send(Command::SaveTunnel(Tunnel {
        host: "nope".into(),
        ..tunnel.clone()
    }));
    let error = wait_for(&events, "a refused tunnel", |e| match e {
        Event::Error(m) => Some(m),
        _ => None,
    });
    assert!(error.contains("no host `nope`"), "{error}");

    core.send(Command::SaveTunnel(tunnel.clone()));
    wait_for(&events, "the saved tunnel", |e| match e {
        Event::Data(d) if d.tunnels.tunnel("t-1").is_some() => Some(()),
        _ => None,
    });
    core.send(Command::SaveGroup(Group {
        id: "g-1".into(),
        name: "Work".into(),
        tunnels: vec!["t-1".into()],
    }));
    wait_for(&events, "the saved group", |e| match e {
        Event::Data(d) if d.tunnels.group("g-1").is_some() => Some(()),
        _ => None,
    });

    core.send(Command::StartTunnel("t-1".into()));
    tunnel_state(&events, "t-1", TunnelState::Running);
    core.send(Command::SaveTunnel(tunnel.clone()));
    let error = wait_for(&events, "edit refused while running", |e| match e {
        Event::Error(m) => Some(m),
        _ => None,
    });
    assert!(error.contains("before editing"), "{error}");

    core.send(Command::StartGroup("g-1".into()));
    wait_for(&events, "two owners", |e| match e {
        Event::Tunnels(s) if s.tunnels["t-1"].owners.len() == 2 => Some(()),
        _ => None,
    });
    core.send(Command::StopTunnel("t-1".into()));
    let s = wait_for(&events, "the group still holding it", |e| match e {
        Event::Tunnels(s) if s.tunnels["t-1"].owners.len() == 1 => Some(s),
        _ => None,
    });
    assert_eq!(s.state("t-1"), TunnelState::Running);

    core.send(Command::DeleteTunnel("t-1".into()));
    let data = wait_for(&events, "the deleted tunnel", |e| match e {
        Event::Data(d) if d.tunnels.tunnel("t-1").is_none() => Some(d),
        _ => None,
    });
    assert!(
        data.tunnels.groups[0].tunnels.is_empty(),
        "removed from the group"
    );
    let deadline = Instant::now() + WAIT;
    while std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "the deleted tunnel kept running");
        std::thread::sleep(Duration::from_millis(25));
    }

    // A tunnel still running at shutdown is stopped with the app.
    let port = free();
    core.send(Command::SaveTunnel(Tunnel {
        id: "t-2".into(),
        host: "ok-b".into(),
        forwards: vec![Forward {
            listen: port,
            ..tunnel.forwards[0].clone()
        }],
        ..tunnel
    }));
    core.send(Command::StartTunnel("t-2".into()));
    tunnel_state(&events, "t-2", TunnelState::Running);
    drop(core);
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "tunnels must stop with the app"
    );
    assert_eq!(fingerprint(&paths.ssh_config), before);
}

#[test]
fn missing_config_is_not_an_error_and_is_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let paths = Paths::new(&home, tmp.path().join("app"));
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    wait_for(&events, "a missing config", |e| match e {
        Event::Config(snapshot) => matches!(snapshot.state, ConfigState::Missing).then_some(()),
        _ => None,
    });

    std::thread::sleep(Duration::from_millis(300));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    fs::write(&paths.ssh_config, "Host first\n").unwrap();
    let aliases = wait_for(&events, "the created config", host_aliases);
    assert_eq!(aliases, ["first"]);
    drop(core);
}
