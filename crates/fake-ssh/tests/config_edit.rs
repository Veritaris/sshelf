//! Config editing (SPEC.md §4, §5). Validation runs against the real
//! `ssh -G -F <temp file>`: it only reads the temp file (`-F` skips the
//! system config) and never connects, so it is safe on every CI runner. Core
//! flows (save, conflicts, new hosts) run against `fake-ssh` on temp dirs.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use sshelf_core::config_edit::{Change, Edit, Problem, validate};
use sshelf_core::core::{Command, ConfigSnapshot, Core, Event};
use sshelf_core::paths::Paths;
use sshelf_core::settings::Settings;
use sshelf_core::ssh::SshTools;
use sshelf_core::storage::Store;

const WAIT: Duration = Duration::from_secs(10);

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
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    if let Some(text) = config_text {
        fs::write(&paths.ssh_config, text).unwrap();
    }
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))),
            ..Settings::default()
        })
        .unwrap();
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    (tmp, paths, core, events)
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
    assert_eq!(snapshot.writable_files(), std::slice::from_ref(&paths.ssh_config));
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
