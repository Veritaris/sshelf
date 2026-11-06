//! A full core session against temp dirs and `fake-ssh`: load, resolve, edit
//! metadata, handle orphans, connect, react to config changes — and never
//! write to the ssh config.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

mod common;
#[path = "common/http.rs"]
mod http;

use sshelf_core::core::{Command, ConfigState, Core, Event, UpdateSource};
use sshelf_core::metadata::{HostMeta, HostsFile};
use sshelf_core::paths::Paths;
use sshelf_core::settings::Settings;
use sshelf_core::storage::Store;
use sshelf_core::supervisor::{Snapshot, TunnelState};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Group, Tunnel};

const WAIT: Duration = Duration::from_secs(10);

/// Public parts of two throwaway keys (fake-ssh's servers present `NEW`).
const OLD: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID/hisleXY3lBCSP34IG2qVLK1Fupesz68s+RrGuiBgg";
const NEW: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/867Gn6gWVC9L/2TosddbUBIfUwN1FA755XCQA20kG";

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

/// Whether something listens on `port`. Probed by connecting, never by
/// binding: a probe that binds could take the port from the process meant
/// to hold it.
fn listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
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
            ..Settings::default()
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

    // Connect checks the host key first: unknown here, so it asks, and the
    // terminal opens once the key is accepted.
    core.send(Command::Connect("web".into()));
    let prompt = wait_for(&events, "the connect's host key prompt", |e| match e {
        Event::HostKeyPrompt(p) => Some(p),
        _ => None,
    });
    assert_eq!(prompt.purpose, "connecting to web");
    core.send(Command::AcceptHostKey(prompt.id));
    wait_for(&events, "launch", |e| match e {
        Event::Launched { alias } => Some(alias),
        _ => None,
    });
    let data = wait_for(&events, "history", |e| match e {
        Event::Data(data) if !data.history.entries.is_empty() => Some(data),
        _ => None,
    });
    assert_eq!(data.history.entries[0].alias, "web");
    // fake-ssh creates the file before it writes the line: wait for a whole
    // line, not just the file.
    let deadline = Instant::now() + WAIT;
    let launched = loop {
        let text = fs::read_to_string(&record).unwrap_or_default();
        if text.ends_with('\n') || Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(launched.trim_end().split('\x1f').next_back(), Some("web"));

    // External edit: picked up by the watcher. (Storing web's host key
    // above also touched ~/.ssh, so an earlier reload may still show the
    // old hosts: wait for the one with the edit.)
    fs::write(&paths.ssh_config, "Host web\r\nHost db\r\nHost new\r\n").unwrap();
    let before = fingerprint(&paths.ssh_config);
    let aliases = wait_for(&events, "the reloaded config", |e| {
        host_aliases(e).filter(|a| a.iter().any(|a| a == "new"))
    });
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
    // Known host keys, so starts go straight through.
    fs::write(
        paths.ssh_dir.join("known_hosts"),
        format!("ok-a.example.com {NEW}\nok-b.example.com {NEW}\n"),
    )
    .unwrap();
    let before = fingerprint(&paths.ssh_config);
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(fake_ssh()),
            ..Settings::default()
        })
        .unwrap();

    let free = || common::hold_port().1;
    let port = free();
    let tunnel = Tunnel::ssh(
        "t-1",
        "API",
        "ok-a",
        vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: port,
            target: Some("localhost:6443".into()),
        }],
    );

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
        start_on_launch: false,
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
    while listening(port) {
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
    assert!(!listening(port), "tunnels must stop with the app");
    assert_eq!(fingerprint(&paths.ssh_config), before);
}

/// A core with fake-ssh as ssh, hosts `aliases`, and `known_hosts` contents.
fn host_key_session(
    aliases: &[&str],
    known_hosts: &str,
) -> (tempfile::TempDir, Paths, Core, Receiver<Event>) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    let config: String = aliases.iter().map(|a| format!("Host {a}\n")).collect();
    fs::write(&paths.ssh_config, config).unwrap();
    if !known_hosts.is_empty() {
        fs::write(paths.ssh_dir.join("known_hosts"), known_hosts).unwrap();
    }
    let store = Store::new(&paths.app_dir);
    store
        .save(&Settings {
            ssh_path: Some(fake_ssh()),
            ..Settings::default()
        })
        .unwrap();
    let mut tunnels = sshelf_core::tunnels::TunnelsFile::default();
    for alias in aliases {
        tunnels.upsert_tunnel(Tunnel::ssh(
            &format!("t-{alias}"),
            &format!("to {alias}"),
            alias,
            vec![Forward {
                kind: ForwardKind::L,
                bind: DEFAULT_BIND.into(),
                listen: common::hold_port().1,
                target: Some("localhost:80".into()),
            }],
        ));
    }
    store.save(&tunnels).unwrap();
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    wait_for(&events, "the config", host_aliases);
    (tmp, paths, core, events)
}

fn prompt(events: &Receiver<Event>) -> Arc<sshelf_core::core::HostKeyPrompt> {
    wait_for(events, "a host key prompt", |e| match e {
        Event::HostKeyPrompt(p) => Some(p),
        _ => None,
    })
}

#[test]
fn unknown_host_key_is_asked_for_then_stored_and_started() {
    let (_tmp, paths, core, events) = host_key_session(&["newkey-a"], "");
    core.send(Command::StartTunnel("t-newkey-a".into()));
    let p = prompt(&events);
    assert_eq!(p.unknown.target.display(), "newkey-a.example.com");
    assert_eq!(p.unknown.keys.len(), 1);
    assert!(p.unknown.keys[0].fingerprint.starts_with("SHA256:"));
    assert_eq!(p.purpose, "tunnel \"to newkey-a\"");

    core.send(Command::AcceptHostKey(p.id));
    tunnel_state(&events, "t-newkey-a", TunnelState::Running);
    assert_eq!(
        fs::read_to_string(paths.ssh_dir.join("known_hosts")).unwrap(),
        format!("newkey-a.example.com {NEW}\n")
    );
}

#[test]
fn rejected_host_key_starts_nothing_and_stores_nothing() {
    let (_tmp, paths, core, events) = host_key_session(&["newkey-b"], "");
    core.send(Command::StartTunnel("t-newkey-b".into()));
    let p = prompt(&events);
    core.send(Command::RejectHostKey(p.id));
    let error = wait_for(&events, "the rejection", |e| match e {
        Event::Error(m) => Some(m),
        _ => None,
    });
    assert!(error.contains("was not accepted"), "{error}");
    assert!(!paths.ssh_dir.join("known_hosts").exists());
    drop(core);
}

#[test]
fn jump_hosts_are_asked_for_first() {
    let (_tmp, paths, core, events) = host_key_session(&["newkey-via-x"], "");
    core.send(Command::StartTunnel("t-newkey-via-x".into()));
    let first = prompt(&events);
    assert_eq!(first.unknown.target.display(), "newkey-jump.example.com");
    assert_eq!(first.unknown.hop_of.as_deref(), Some("newkey-via-x"));
    core.send(Command::AcceptHostKey(first.id));
    let second = prompt(&events);
    assert_eq!(second.unknown.target.display(), "newkey-via-x.example.com");
    assert_eq!(second.unknown.hop_of, None);
    core.send(Command::AcceptHostKey(second.id));
    tunnel_state(&events, "t-newkey-via-x", TunnelState::Running);
    let stored = fs::read_to_string(paths.ssh_dir.join("known_hosts")).unwrap();
    assert_eq!(stored.lines().count(), 2, "{stored}");
}

#[test]
fn unreachable_hosts_report_why_and_start_nothing() {
    let (_tmp, _paths, core, events) = host_key_session(&["unreachable-a"], "");
    core.send(Command::StartTunnel("t-unreachable-a".into()));
    let error = wait_for(&events, "the fetch error", |e| match e {
        Event::Error(m) => Some(m),
        _ => None,
    });
    assert!(error.contains("cannot get the host key"), "{error}");
    assert!(error.contains("timed out"), "{error}");
    drop(core);
}

#[test]
fn changed_host_key_is_reviewed_and_replaced() {
    let old_line = format!("changed-a.example.com {OLD}\n");
    let (_tmp, paths, core, events) = host_key_session(&["changed-a"], &old_line);
    core.send(Command::StartTunnel("t-changed-a".into()));
    let s = wait_for(&events, "the changed-key failure", |e| match e {
        Event::Tunnels(s) if matches!(s.state("t-changed-a"), TunnelState::Failed(_)) => Some(s),
        _ => None,
    });
    assert_eq!(
        s.tunnels["t-changed-a"].failure,
        Some(sshelf_core::supervisor::FailureKind::HostKeyChanged)
    );

    core.send(Command::ReviewChangedKey("t-changed-a".into()));
    let review = wait_for(&events, "the review", |e| match e {
        Event::HostKeyChanged(p) => Some(p),
        _ => None,
    });
    assert_eq!(review.subject, "tunnel \u{201c}to changed-a\u{201d}");
    assert_ne!(
        review.changed.old[0].fingerprint,
        review.changed.new[0].fingerprint
    );
    assert_eq!(
        review.changed.user_files,
        [paths.ssh_dir.join("known_hosts")]
    );

    core.send(Command::ReplaceHostKey(review.id));
    // Restarted after the replacement (fake-ssh's server keeps failing).
    wait_for(&events, "the restart", |e| match e {
        Event::Tunnels(s) if s.state("t-changed-a") == TunnelState::Starting => Some(()),
        _ => None,
    });
    let stored = fs::read_to_string(paths.ssh_dir.join("known_hosts")).unwrap();
    assert_eq!(stored, format!("changed-a.example.com {NEW}\n"));
    let backups: Vec<String> = fs::read_dir(&paths.ssh_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".sshelf-") || n.ends_with(".old"))
        .collect();
    assert_eq!(
        backups.len(),
        2,
        "sshelf's backup and ssh-keygen's .old: {backups:?}"
    );
    drop(core);
}

/// The health banner's review: a changed key gets the same dialog as a
/// failed tunnel's, and replacing it starts nothing.
#[test]
fn host_review_finds_a_changed_key_and_only_replaces_it() {
    let old_line = format!("changed-b.example.com {OLD}\n");
    let (_tmp, paths, core, events) = host_key_session(&["changed-b"], &old_line);
    core.send(Command::ReviewHostKey("changed-b".into()));
    let review = wait_for(&events, "the review", |e| match e {
        Event::HostKeyChanged(p) => Some(p),
        _ => None,
    });
    assert_eq!(review.subject, "host \u{201c}changed-b\u{201d}");
    core.send(Command::ReplaceHostKey(review.id));
    let notice = wait_for(&events, "the replacement", |e| match e {
        Event::Notice(m) => Some(m),
        Event::Tunnels(s) if s.state("t-changed-b") != TunnelState::Stopped => {
            panic!("a host review must not start the tunnel")
        }
        _ => None,
    });
    assert_eq!(notice, "Replaced the host key of changed-b.example.com");
    let stored = fs::read_to_string(paths.ssh_dir.join("known_hosts")).unwrap();
    assert_eq!(stored, format!("changed-b.example.com {NEW}\n"));
    drop(core);
}

/// An unknown key asks like a start; once stored, the review finds every
/// key matching.
#[test]
fn host_review_asks_for_an_unknown_key_then_reports_a_match() {
    let (_tmp, paths, core, events) = host_key_session(&["newkey-c"], "");
    core.send(Command::ReviewHostKey("newkey-c".into()));
    let p = prompt(&events);
    assert_eq!(p.purpose, "host \"newkey-c\"");
    core.send(Command::AcceptHostKey(p.id));
    let notice = wait_for(&events, "the match", |e| match e {
        Event::Notice(m) => Some(m),
        _ => None,
    });
    assert_eq!(notice, "The stored host keys of newkey-c match its servers");
    assert_eq!(
        fs::read_to_string(paths.ssh_dir.join("known_hosts")).unwrap(),
        format!("newkey-c.example.com {NEW}\n")
    );
    drop(core);
}

#[test]
fn routes_list_the_jump_hosts_first_hop_first() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::write(
        &paths.ssh_config,
        "Host db-via-gw\nHost db-jump\nHost plain\n",
    )
    .unwrap();
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(fake_ssh()),
            ..Settings::default()
        })
        .unwrap();
    let (core, events) = Core::start(paths, Arc::new(|| {}));
    wait_for(&events, "the config", host_aliases);

    let route = |alias: &str| {
        core.send(Command::ResolveRoute(alias.into()));
        wait_for(&events, "a route", |e| match e {
            Event::Route {
                alias: a, result, ..
            } if a == alias => Some(result),
            _ => None,
        })
    };
    // fake-ssh: `<x>-via-<y>` jumps through `<x>-jump`.
    let hops = route("db-via-gw").unwrap();
    let names: Vec<&str> = hops.iter().map(|h| h.destination.as_str()).collect();
    assert_eq!(names, ["db-jump"]);
    assert_eq!(hops[0].hostname, "db-jump.example.com");
    assert!(route("plain").unwrap().is_empty());
    drop(core);
}

#[test]
fn groups_marked_for_launch_start_with_sshelf() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::write(&paths.ssh_config, "Host ok-a\nHost ok-b\n").unwrap();
    fs::write(
        paths.ssh_dir.join("known_hosts"),
        format!("ok-a.example.com {NEW}\nok-b.example.com {NEW}\n"),
    )
    .unwrap();
    let store = Store::new(&paths.app_dir);
    store
        .save(&Settings {
            ssh_path: Some(fake_ssh()),
            ..Settings::default()
        })
        .unwrap();
    let mut file = sshelf_core::tunnels::TunnelsFile::default();
    for (id, host) in [("t-a", "ok-a"), ("t-b", "ok-b")] {
        file.upsert_tunnel(Tunnel::ssh(
            id,
            id,
            host,
            vec![Forward {
                kind: ForwardKind::L,
                bind: DEFAULT_BIND.into(),
                listen: common::hold_port().1,
                target: Some("localhost:80".into()),
            }],
        ));
    }
    file.upsert_group(Group {
        id: "g-launch".into(),
        name: "At launch".into(),
        tunnels: vec!["t-a".into()],
        start_on_launch: true,
    });
    file.upsert_group(Group {
        id: "g-manual".into(),
        name: "By hand".into(),
        tunnels: vec!["t-b".into()],
        start_on_launch: false,
    });
    store.save(&file).unwrap();

    let (core, events) = Core::start(paths, Arc::new(|| {}));
    let states = tunnel_state(&events, "t-a", TunnelState::Running);
    assert!(
        states.tunnels["t-a"]
            .owners
            .contains(&sshelf_core::supervisor::Owner::Group("g-launch".into()))
    );
    assert_eq!(states.state("t-b"), TunnelState::Stopped);
    drop(core);
}

/// kubectl (M13) found on the core's own `PATH` (fake-ssh copied as
/// `kubectl`), reported with its version and kubeconfig, and a kubectl
/// tunnel started through the core: no ssh, no host key check.
#[test]
fn kubectl_is_found_and_runs_kubectl_tunnels() {
    use sshelf_core::kubectl::KubectlSource;
    use sshelf_core::tunnels::{Kube, KubeForward, Resource};

    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let kubectl = bin.join(format!("kubectl{}", std::env::consts::EXE_SUFFIX));
    fs::copy(fake_ssh(), &kubectl).unwrap();
    let mut paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    paths.kube.path = bin.clone().into_os_string();

    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    let info = wait_for(&events, "kubectl", |e| match e {
        Event::Kubectl(info) => Some(info),
        _ => None,
    });
    let found = info.kubectl.as_ref().unwrap();
    assert_eq!(found.tool.program, kubectl);
    assert_eq!(found.source, KubectlSource::Path);
    assert_eq!(
        info.version.as_ref().unwrap().as_ref().unwrap().client,
        fake_ssh::kubectl::CLIENT_VERSION
    );
    let view = info.contexts().unwrap();
    assert_eq!(view.current.as_deref(), Some("fake-lab"));
    // No kubeconfig in the temp home (and none from the real one).
    assert!(!info.files.files[0].1);

    let port = common::hold_port().1;
    let tunnel = Tunnel::kubectl(
        "t-k",
        "web api",
        Kube {
            context: String::new(),
            namespace: "fake-api".into(),
            resource: Resource::parse("service/fake-web").unwrap(),
            address: DEFAULT_BIND.into(),
            forwards: vec![KubeForward {
                listen: port,
                target: "80".into(),
            }],
        },
    );
    core.send(Command::SaveTunnel(tunnel));
    wait_for(&events, "the saved tunnel", |e| match e {
        Event::Data(d) if d.tunnels.tunnel("t-k").is_some() => Some(()),
        _ => None,
    });
    core.send(Command::StartTunnel("t-k".into()));
    tunnel_state(&events, "t-k", TunnelState::Running);
    assert!(listening(port));

    // An override that doesn't exist: not found, no fallback to PATH.
    core.send(Command::SaveSettings(Settings {
        kubectl_path: Some(tmp.path().join("nope")),
        ..Settings::default()
    }));
    let info = wait_for(&events, "kubectl again", |e| match e {
        Event::Kubectl(info) => Some(info),
        _ => None,
    });
    let error = info.kubectl.as_ref().unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
    drop(core);
    assert!(!listening(port), "kubectl tunnels stop with the app");
}

#[test]
fn saving_settings_publishes_them_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    let settings = Settings {
        ssh_path: Some(fake_ssh()),
        close_behavior: sshelf_core::settings::CloseBehavior::MinimizeToTray,
        ..Settings::default()
    };
    core.send(Command::SaveSettings(settings.clone()));
    wait_for(&events, "the saved settings", |e| match e {
        Event::Data(data) if data.settings == settings => Some(()),
        _ => None,
    });
    let notice = wait_for(&events, "a notice", |e| match e {
        Event::Notice(text) => Some(text),
        _ => None,
    });
    assert_eq!(notice, "Settings saved to settings.toml");
    assert_eq!(Store::new(&paths.app_dir).load::<Settings>().data, settings);
    drop(core);
}

/// The update check against a local stand-in for GitHub, from `Core`
/// (SPEC.md §10 *Update check*).
fn start_updates(core: &Core, url: &str) {
    core.send(Command::StartUpdates(UpdateSource {
        version: "0.3.2".into(),
        url: url.into(),
        first: Duration::ZERO,
    }));
}

#[test]
fn the_update_check_asks_follows_its_switch_and_keeps_a_skip() {
    let server = http::serve(http::release("0.4.0"));
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let saved = Settings {
        ssh_path: Some(fake_ssh()),
        update_check_noted: true,
        ..Settings::default()
    };
    Store::new(&paths.app_dir).save(&saved).unwrap();
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    start_updates(&core, &server.url);
    let found = wait_for(&events, "the check", |e| match e {
        Event::Updates(s) if s.checked_this_run && !s.checking => Some(s),
        _ => None,
    });
    assert_eq!(found.announced().unwrap().version, "0.4.0");
    let heads = server.heads();
    assert_eq!(heads.len(), 1);
    assert!(heads[0].contains("sshelf/0.3.2"), "{}", heads[0]);
    // Off at once: saved, and the next check is not scheduled.
    core.send(Command::SetUpdateCheck(false));
    let off = wait_for(&events, "off", |e| match e {
        Event::Updates(s) if !s.enabled => Some(s),
        _ => None,
    });
    assert_eq!(off.next_check, None);
    assert!(
        !Store::new(&paths.app_dir)
            .load::<Settings>()
            .data
            .update_check
    );
    // A save from older settings (a draft, the tour's state) keeps both.
    core.send(Command::SaveSettings(Settings {
        reduce_motion: true,
        update_check_noted: false,
        ..saved.clone()
    }));
    wait_for(&events, "the save", |e| match e {
        Event::Data(data) if data.settings.reduce_motion => Some(()),
        _ => None,
    });
    let stored = Store::new(&paths.app_dir).load::<Settings>().data;
    assert!(
        !stored.update_check && stored.update_check_noted,
        "{stored:?}"
    );
    core.send(Command::SkipUpdate("0.4.0".into()));
    wait_for(&events, "skipped", |e| match e {
        Event::Updates(s) if s.skipped.as_deref() == Some("0.4.0") => Some(()),
        _ => None,
    });
    let file = fs::read_to_string(paths.app_dir.join("updates.toml")).unwrap();
    assert!(file.contains("skipped = \"0.4.0\""), "{file}");
    assert!(file.contains("version = 1"), "{file}");
    drop(core);
    assert_eq!(server.heads().len(), 1);
}

#[test]
fn the_first_check_waits_for_the_first_run_note() {
    let server = http::serve(http::release("0.4.0"));
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let saved = Settings {
        ssh_path: Some(fake_ssh()),
        ..Settings::default()
    };
    Store::new(&paths.app_dir).save(&saved).unwrap();
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    start_updates(&core, &server.url);
    let first = wait_for(&events, "the status", |e| match e {
        Event::Updates(s) => Some(s),
        _ => None,
    });
    assert!(first.enabled && !first.auto, "{first:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(server.heads().is_empty());
    core.send(Command::NoteUpdateCheck { keep_on: true });
    wait_for(&events, "the check", |e| match e {
        Event::Updates(s) if s.checked_this_run && !s.checking => Some(()),
        _ => None,
    });
    let settings = Store::new(&paths.app_dir).load::<Settings>().data;
    assert!(settings.update_check && settings.update_check_noted);
    // *Turn off* instead: noted, and off.
    core.send(Command::NoteUpdateCheck { keep_on: false });
    wait_for(&events, "turned off", |e| match e {
        Event::Updates(s) if !s.enabled => Some(()),
        _ => None,
    });
    let settings = Store::new(&paths.app_dir).load::<Settings>().data;
    assert!(!settings.update_check && settings.update_check_noted);
    drop(core);
}

#[test]
fn streamer_mode_applies_at_once_and_keeps_the_other_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let saved = Settings {
        ssh_path: Some(fake_ssh()),
        reduce_motion: true,
        ..Settings::default()
    };
    Store::new(&paths.app_dir).save(&saved).unwrap();
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    core.send(Command::SetStreamer {
        on: true,
        hide_aliases: true,
    });
    let expected = Settings {
        streamer_mode: true,
        streamer_hide_aliases: true,
        ..saved
    };
    let mut notices = Vec::new();
    wait_for(&events, "streamer mode on", |e| match e {
        Event::Data(data) if data.settings == expected => Some(()),
        Event::Notice(text) => {
            notices.push(text);
            None
        }
        _ => None,
    });
    assert!(
        notices.is_empty(),
        "the app shows its own toast: {notices:?}"
    );
    assert_eq!(Store::new(&paths.app_dir).load::<Settings>().data, expected);
    drop(core);
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
