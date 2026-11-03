//! Adding a generated key to the agent (SPEC.md §11) without touching the
//! developer's real agent: `fake-ssh` is copied into a temp dir as both `ssh`
//! and `ssh-add`, so the companion lookup finds the copy, which records how
//! it was run (`FAKE_SSH_RECORD`). Its own test binary, so setting that
//! variable cannot reach other tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use sshelf_core::core::{Command, Core, Event};
use sshelf_core::paths::Paths;
use sshelf_core::settings::Settings;
use sshelf_core::storage::Store;

fn exe(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_fake-ssh"), &path).unwrap();
    path
}

fn notice_or_error(events: &Receiver<Event>) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left).expect("a notice or an error") {
            Event::Notice(text) => return Ok(text),
            Event::Error(text) => return Err(text),
            _ => {}
        }
    }
}

fn recorded(file: &Path, lines: usize) -> Vec<Vec<String>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = fs::read_to_string(file).unwrap_or_default();
        if text.lines().count() >= lines {
            return text
                .lines()
                .map(|l| l.split('\x1f').map(str::to_string).collect())
                .collect();
        }
        assert!(Instant::now() < deadline, "not launched");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn keys_go_to_the_agent_directly_or_through_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let ssh = exe(&bin, "ssh");
    exe(&bin, "ssh-add");
    let record = tmp.path().join("record.txt");
    // SAFETY: the only test in this binary; set before any thread starts.
    unsafe { std::env::set_var("FAKE_SSH_RECORD", &record) };

    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(ssh),
            // A full template: `{ssh}` becomes ssh-add, `{alias}` its arguments.
            terminal_command: "{ssh} {alias}".into(),
            ..Settings::default()
        })
        .unwrap();
    let (core, events) = Core::start(paths, Arc::new(|| {}));
    let key = tmp.path().join("id_ed25519_web");

    // Without a passphrase: ssh-add runs directly.
    core.send(Command::AddKeyToAgent {
        path: key.clone(),
        encrypted: false,
    });
    assert_eq!(
        notice_or_error(&events),
        Ok("Added id_ed25519_web to the ssh agent".to_string())
    );
    // (`ssh-add -l` runs too: the agent status at startup and after adding.)
    let adds = |runs: &[Vec<String>]| -> Vec<Vec<String>> {
        runs.iter()
            .filter(|r| r[0] == "ssh-add" && r.last() == Some(&key.display().to_string()))
            .cloned()
            .collect()
    };
    let ssh_add = adds(&recorded(&record, 2));
    assert_eq!(ssh_add.len(), 1, "{ssh_add:?}");
    if cfg!(target_os = "macos") {
        assert!(ssh_add[0].contains(&"--apple-use-keychain".to_string()));
    }

    // With one: ssh-add runs in the terminal, where the passphrase is typed.
    core.send(Command::AddKeyToAgent {
        path: key.clone(),
        encrypted: true,
    });
    let notice = notice_or_error(&events).unwrap();
    assert!(
        notice.starts_with("Opened a terminal for ssh-add"),
        "{notice}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while adds(&recorded(&record, 1)).len() < 2 {
        assert!(
            Instant::now() < deadline,
            "ssh-add was not run in the terminal"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // A path from `ssh -G` (the host detail's *Load key into agent*) may
    // start with `~`; no shell expands it, so core does.
    core.send(Command::AddKeyToAgent {
        path: PathBuf::from("~/.ssh/id_web"),
        encrypted: false,
    });
    assert_eq!(
        notice_or_error(&events),
        Ok("Added id_web to the ssh agent".to_string())
    );
    let expanded = tmp.path().join("home").join(".ssh/id_web");
    let runs = recorded(&record, 1);
    assert!(
        runs.iter()
            .any(|r| r.last() == Some(&expanded.display().to_string())),
        "{runs:?}"
    );
    drop(core);
}
