//! Config editing (SPEC.md §4, §5). Validation runs against the real
//! `ssh -G -F <temp file>`: it only reads the temp file (`-F` skips the
//! system config) and never connects, so it is safe on every CI runner. Core
//! flows (save, conflicts, new hosts) run against `fake-ssh` on temp dirs.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use sshelf_core::config_edit::{Change, Edit, Problem, validate};
use sshelf_core::core::{Command, ConfigSnapshot, Core, Event};
use sshelf_core::history::History;
use sshelf_core::metadata::{HostMeta, HostsFile};
use sshelf_core::paths::Paths;
use sshelf_core::settings::Settings;
use sshelf_core::ssh::SshTools;
use sshelf_core::storage::{Store, Versioned};
use sshelf_core::supervisor::TunnelState;
use sshelf_core::tunnels::{Tunnel, TunnelsFile};

const WAIT: Duration = Duration::from_secs(10);
/// The host key fake-ssh's servers present.
const NEW_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/867Gn6gWVC9L/2TosddbUBIfUwN1FA755XCQA20kG";

fn ssh() -> PathBuf {
    SshTools::locate(None)
        .expect("the system ssh is installed")
        .ssh
}

fn edit(base: &str, lines: std::ops::Range<usize>, text: &str) -> Edit {
    Edit {
        id: 1,
        file: "config".into(),
        base: base.as_bytes().to_vec(),
        change: Change::Replace {
            lines,
            text: text.to_string(),
        },
        alias: "web".into(),
    }
}

fn messages(problems: &[Problem]) -> Vec<(Option<usize>, &str)> {
    problems
        .iter()
        .map(|p| (p.line, p.message.as_str()))
        .collect()
}

#[test]
fn a_valid_edit_has_no_problems() {
    let base = "Host web\n    HostName web.example.com\n";
    let e = edit(
        base,
        0..2,
        "Host web\n    HostName web.example.com\n    Port 2222",
    );
    assert_eq!(validate(&ssh(), &e).unwrap(), []);
}

#[test]
fn ssh_errors_are_mapped_to_block_lines() {
    let base = "# top\nHost other\n    Port 1\nHost web\n    HostName w\n";
    let e = edit(
        base,
        3..5,
        "Host web\n    HostName w\n    Port abc\n    Bogus 1",
    );
    let problems = validate(&ssh(), &e).unwrap();
    let lines: Vec<_> = messages(&problems).into_iter().map(|(l, _)| l).collect();
    assert_eq!(lines, [Some(2), Some(3)], "{problems:?}");
    assert!(problems[0].message.contains("abc"), "{problems:?}");
    assert!(
        problems[1].message.to_lowercase().contains("bogus"),
        "{problems:?}"
    );
}

#[test]
fn invalid_lines_are_reported_without_waiting_for_ssh() {
    let base = "Host web\n";
    let e = edit(base, 0..1, "Host web\n    HostName \"open");
    let problems = validate(&ssh(), &e).unwrap();
    assert_eq!(problems[0].line, Some(1));
    assert_eq!(problems[0].message, "unterminated quote");
}

#[test]
fn problems_that_were_already_there_do_not_block() {
    // An old bad option elsewhere in the file must not make every edit unsaveable.
    let base = "Host old\n    Frobnicate 1\nHost web\n    HostName w\n";
    let e = edit(base, 2..4, "Host web\n    HostName w2");
    assert_eq!(validate(&ssh(), &e).unwrap(), []);
    // A new copy of the same mistake in the edited block does block.
    let e = edit(base, 2..4, "Host web\n    HostName w2\n    Frobnicate 1");
    let problems = validate(&ssh(), &e).unwrap();
    assert_eq!(messages(&problems).len(), 1, "{problems:?}");
    assert_eq!(problems[0].line, Some(2));
}

#[test]
fn a_new_host_is_validated_where_it_is_inserted() {
    let e = Edit {
        id: 2,
        file: "config".into(),
        base: b"Host *\n    User me\n".to_vec(),
        change: Change::Insert {
            text: "Host fresh\n    Port nope".into(),
        },
        alias: "fresh".into(),
    };
    let problems = validate(&ssh(), &e).unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].line, Some(1));

    let e = Edit {
        base: Vec::new(),
        change: Change::Insert {
            text: "Host fresh\n    Port 22".into(),
        },
        ..e
    };
    assert_eq!(validate(&ssh(), &e).unwrap(), []);
}

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

fn config(events: &Receiver<Event>) -> Arc<ConfigSnapshot> {
    wait_for(events, "the config", |e| match e {
        Event::Config(snapshot) => Some(snapshot),
        _ => None,
    })
}

/// What edit `id` came back with: `Ok(true)` saved, `Ok(false)` conflict,
/// `Err` problems or an error.
fn outcome(events: &Receiver<Event>, id: u64) -> Result<bool, String> {
    wait_for(events, "an edit result", |e| match e {
        Event::EditSaved { id: i } if i == id => Some(Ok(true)),
        Event::EditConflict { id: i, .. } if i == id => Some(Ok(false)),
        Event::EditChecked { id: i, result } if i == id => Some(Err(match result {
            Ok(problems) => format!("{problems:?}"),
            Err(e) => e,
        })),
        _ => None,
    })
}

/// A core over a temp home whose ssh is `fake-ssh`.
fn session(config_text: Option<&str>) -> (tempfile::TempDir, Paths, Core, Receiver<Event>) {
    session_with(config_text, |_, _| {})
}

/// Like [`session`]; `setup` prepares app data before the core starts.
fn session_with(
    config_text: Option<&str>,
    setup: impl FnOnce(&Store, &Paths),
) -> (tempfile::TempDir, Paths, Core, Receiver<Event>) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    if let Some(text) = config_text {
        fs::write(&paths.ssh_config, text).unwrap();
    }
    let store = Store::new(&paths.app_dir);
    store
        .save(&Settings {
            ssh_path: Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))),
            ..Settings::default()
        })
        .unwrap();
    setup(&store, &paths);
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    (tmp, paths, core, events)
}

/// App data for the rename tests: metadata, a tunnel and history for `ok-web`.
fn rename_data(store: &Store, paths: &Paths) {
    let mut hosts = HostsFile::default();
    hosts.hosts.insert(
        "ok-web".into(),
        HostMeta {
            notes: "the web box".into(),
            ..HostMeta::default()
        },
    );
    store.save(&hosts).unwrap();
    let mut tunnels = TunnelsFile::default();
    tunnels.upsert_tunnel(Tunnel {
        id: "t-web".into(),
        name: "web".into(),
        host: "ok-web".into(),
        forwards: Vec::new(),
    });
    store.save(&tunnels).unwrap();
    let mut history = History::default();
    history.record("ok-web", SystemTime::now());
    store.save(&history).unwrap();
    // A known host key, so the tunnel starts without asking.
    fs::write(
        paths.ssh_dir.join("known_hosts"),
        format!("ok-web.example.com {NEW_KEY}\n"),
    )
    .unwrap();
}

fn load<T: Versioned>(paths: &Paths) -> T {
    Store::new(&paths.app_dir).load::<T>().data
}

fn backups(paths: &Paths) -> Vec<Vec<u8>> {
    match fs::read_dir(paths.app_dir.join("backups")) {
        Ok(dir) => dir.map(|e| fs::read(e.unwrap().path()).unwrap()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn edits_are_validated_written_minimally_and_backed_up() {
    let original =
        "# mine\r\nHost web\r\n\tHostName web.example.com # main\r\n\r\nHost *\r\n\tUser me\r\n";
    let (_tmp, paths, core, events) = session(Some(original));
    let snapshot = config(&events);
    let file = snapshot.set().unwrap().files[0].clone();
    let block = file.block_at(1).unwrap();
    let edit = |id, text: &str| Edit {
        id,
        file: paths.ssh_config.clone(),
        base: original.as_bytes().to_vec(),
        change: Change::Replace {
            lines: block.lines.clone(),
            text: text.to_string(),
        },
        alias: "web".into(),
    };

    // An invalid edit is never written.
    core.send(Command::SaveEdit {
        edit: edit(
            1,
            "Host web\n\tHostName web.example.com # main\n\tBogus 1\n",
        ),
        overwrite: false,
    });
    let problem = outcome(&events, 1).unwrap_err();
    assert!(problem.contains("line: Some(2)"), "{problem}");
    assert_eq!(fs::read(&paths.ssh_config).unwrap(), original.as_bytes());
    assert!(backups(&paths).is_empty());

    // A valid one changes exactly the edited line, keeping CRLF.
    core.send(Command::SaveEdit {
        edit: edit(2, "Host web\n\tHostName web2.example.com # main\n"),
        overwrite: false,
    });
    assert_eq!(outcome(&events, 2), Ok(true));
    let saved = original.replace("web.example.com", "web2.example.com");
    assert_eq!(fs::read_to_string(&paths.ssh_config).unwrap(), saved);
    assert_eq!(backups(&paths), [original.as_bytes()]);

    // The base is now stale: conflict, nothing written.
    core.send(Command::SaveEdit {
        edit: edit(3, "Host web\n\tHostName web3.example.com # main\n"),
        overwrite: false,
    });
    assert_eq!(outcome(&events, 3), Ok(false));
    assert_eq!(fs::read_to_string(&paths.ssh_config).unwrap(), saved);

    // Overwriting is the user's explicit choice; still one backup per session.
    core.send(Command::SaveEdit {
        edit: edit(4, "Host web\n\tHostName web3.example.com # main\n"),
        overwrite: true,
    });
    assert_eq!(outcome(&events, 4), Ok(true));
    assert!(
        fs::read_to_string(&paths.ssh_config)
            .unwrap()
            .contains("web3")
    );
    assert_eq!(backups(&paths).len(), 1);
    drop(core);
}

#[test]
fn only_loaded_writable_files_are_edited() {
    let (tmp, paths, core, events) = session(Some("Host web\n"));
    config(&events);
    let outside = tmp.path().join("elsewhere");
    fs::write(&outside, "Host x\n").unwrap();
    core.send(Command::SaveEdit {
        edit: Edit {
            id: 1,
            file: outside.clone(),
            base: b"Host x\n".to_vec(),
            change: Change::Insert {
                text: "Host y".into(),
            },
            alias: "y".into(),
        },
        overwrite: true,
    });
    let error = outcome(&events, 1).unwrap_err();
    assert!(
        error.contains("not part of the loaded ssh config"),
        "{error}"
    );
    assert_eq!(fs::read(&outside).unwrap(), b"Host x\n");

    let mut permissions = fs::metadata(&paths.ssh_config).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&paths.ssh_config, permissions.clone()).unwrap();
    core.send(Command::Reload);
    let snapshot = config(&events);
    assert!(snapshot.read_only(0).is_some());
    assert!(snapshot.writable_files().is_empty());
    core.send(Command::SaveEdit {
        edit: Edit {
            id: 2,
            file: paths.ssh_config.clone(),
            base: b"Host web\n".to_vec(),
            change: Change::Insert {
                text: "Host y".into(),
            },
            alias: "y".into(),
        },
        overwrite: false,
    });
    assert!(outcome(&events, 2).unwrap_err().contains("read-only"));
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&paths.ssh_config, permissions).unwrap();
    drop(core);
}

#[test]
fn a_new_host_creates_the_missing_config() {
    let (_tmp, paths, core, events) = session(None);
    let snapshot = config(&events);
    assert_eq!(
        snapshot.writable_files(),
        std::slice::from_ref(&paths.ssh_config)
    );
    core.send(Command::SaveEdit {
        edit: Edit {
            id: 1,
            file: paths.ssh_config.clone(),
            base: Vec::new(),
            change: Change::Insert {
                text: "Host fresh\n    HostName fresh.example.com\n".into(),
            },
            alias: "fresh".into(),
        },
        overwrite: false,
    });
    let snapshot = config(&events);
    assert_eq!(outcome(&events, 1), Ok(true));
    let hosts = snapshot.hosts().unwrap();
    assert_eq!(hosts.hosts[0].primary(), "fresh");
    assert_eq!(
        fs::read(&paths.ssh_config).unwrap(),
        b"Host fresh\n    HostName fresh.example.com\n"
    );
    drop(core);
}

const RENAME_CONFIG: &str =
    "Host  ok-web w1 # main\n    HostName web.example.com\nHost *\n    User me\n";

fn rename(core: &Core, id: u64, alias: &str, to: &str) {
    core.send(Command::RenameHost {
        id,
        alias: alias.into(),
        to: to.into(),
    });
}

#[test]
fn rename_changes_the_header_and_all_app_data() {
    let (_tmp, paths, core, events) = session_with(Some(RENAME_CONFIG), rename_data);
    config(&events);
    rename(&core, 1, "ok-web", "ok-www");
    assert_eq!(outcome(&events, 1), Ok(true));
    assert_eq!(
        fs::read_to_string(&paths.ssh_config).unwrap(),
        RENAME_CONFIG.replace("ok-web", "ok-www")
    );
    assert_eq!(
        load::<HostsFile>(&paths).hosts["ok-www"].notes,
        "the web box"
    );
    assert!(!load::<HostsFile>(&paths).hosts.contains_key("ok-web"));
    assert_eq!(load::<TunnelsFile>(&paths).tunnels[0].host, "ok-www");
    assert_eq!(load::<History>(&paths).entries[0].alias, "ok-www");
    // Nothing is left behind as an orphan.
    core.send(Command::Reload);
    let orphans = wait_for(&events, "app data", |e| match e {
        Event::Data(d) => Some(d.orphan_count()),
        _ => None,
    });
    assert_eq!(orphans, 0);
    drop(core);
}

#[test]
fn rename_refuses_taken_names_and_running_tunnels() {
    let (_tmp, paths, core, events) = session_with(Some(RENAME_CONFIG), rename_data);
    config(&events);
    rename(&core, 1, "ok-web", "w1");
    assert!(outcome(&events, 1).unwrap_err().contains("already a host"));
    rename(&core, 2, "nope", "x");
    assert!(outcome(&events, 2).unwrap_err().contains("no host"));

    core.send(Command::StartTunnel("t-web".into()));
    wait_for(&events, "the tunnel running", |e| match e {
        Event::Tunnels(s) if s.state("t-web") == TunnelState::Running => Some(()),
        _ => None,
    });
    rename(&core, 3, "ok-web", "ok-www");
    let error = outcome(&events, 3).unwrap_err();
    assert!(error.contains("stop \"web\""), "{error}");
    assert_eq!(
        fs::read_to_string(&paths.ssh_config).unwrap(),
        RENAME_CONFIG
    );
    drop(core);
}

#[test]
fn rename_is_undone_when_app_data_cannot_be_saved() {
    let (_tmp, paths, core, events) = session_with(Some(RENAME_CONFIG), |store, paths| {
        rename_data(store, paths);
        // A history.toml from a newer sshelf: shown, but never written.
        fs::write(
            paths.app_dir.join("history.toml"),
            "version = 99\n[[entry]]\nalias = \"ok-web\"\nat = \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
    });
    config(&events);
    let hosts_before = fs::read(paths.app_dir.join("hosts.toml")).unwrap();
    let tunnels_before = fs::read(paths.app_dir.join("tunnels.toml")).unwrap();
    rename(&core, 1, "ok-web", "ok-www");
    let error = outcome(&events, 1).unwrap_err();
    assert!(error.contains("the config was restored"), "{error}");
    assert_eq!(
        fs::read_to_string(&paths.ssh_config).unwrap(),
        RENAME_CONFIG
    );
    assert_eq!(
        fs::read(paths.app_dir.join("hosts.toml")).unwrap(),
        hosts_before
    );
    assert_eq!(
        fs::read(paths.app_dir.join("tunnels.toml")).unwrap(),
        tunnels_before
    );
    drop(core);
}
