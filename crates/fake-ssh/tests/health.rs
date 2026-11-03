//! Host health checks through the core (SPEC.md §12, M8) against fake-ssh,
//! which answers `… <alias> true` by alias.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sshelf_core::core::{Command, Core, Event};
use sshelf_core::health::{self, Health, HealthSnapshot};
use sshelf_core::paths::Paths;
use sshelf_core::settings::Settings;
use sshelf_core::storage::Store;

const HOSTS: [&str; 6] = [
    "ok-a",
    "fail-auth-b",
    "changed-c",
    "unknownkey-d",
    "unreachable-e",
    "ok-f",
];

/// A core whose config has [`HOSTS`] and whose ssh is fake-ssh.
fn start(tmp: &tempfile::TempDir) -> (Core, std::sync::mpsc::Receiver<Event>) {
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    let config: String = HOSTS.iter().map(|h| format!("Host {h}\n")).collect();
    fs::write(&paths.ssh_config, config).unwrap();
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))),
            ..Settings::default()
        })
        .unwrap();
    Core::start(paths, Arc::new(|| {}))
}

fn all_hosts() -> Vec<String> {
    HOSTS.iter().map(|h| h.to_string()).collect()
}

#[test]
fn checks_run_a_few_at_a_time_and_classify_every_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, events) = start(&tmp);
    core.send(Command::CheckHealth(all_hosts()));
    // Asking again while they run queues nothing twice.
    core.send(Command::CheckHealth(vec!["ok-a".into()]));

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut most_checking = 0;
    let done: Arc<HealthSnapshot> = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Event::Health(snapshot) = events.recv_timeout(left).expect("health results") {
            most_checking = most_checking.max(snapshot.checking.len());
            if snapshot.checking.is_empty() && snapshot.reports.len() == HOSTS.len() {
                break snapshot;
            }
        }
    };
    assert_eq!(most_checking, HOSTS.len(), "all queued at once");
    let health = |alias: &str| done.reports[alias].health.clone();
    assert!(matches!(health("ok-a"), Health::Ok { .. }));
    assert!(matches!(health("ok-f"), Health::Ok { .. }));
    assert_eq!(health("fail-auth-b"), Health::AuthFailed);
    assert_eq!(health("changed-c"), Health::HostKeyChanged);
    assert_eq!(health("unknownkey-d"), Health::HostKeyUnknown);
    assert!(
        matches!(health("unreachable-e"), Health::Unreachable(ref why) if why.contains("timed out")),
        "{:?}",
        health("unreachable-e")
    );
    drop(core);
}

#[test]
fn cancelling_drops_the_waiting_checks_and_lets_the_running_finish() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, events) = start(&tmp);
    core.send(Command::CheckHealth(all_hosts()));
    core.send(Command::CancelHealth);

    let deadline = Instant::now() + Duration::from_secs(30);
    let done: Arc<HealthSnapshot> = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Event::Health(snapshot) = events.recv_timeout(left).expect("health results")
            && snapshot.checking.is_empty()
            && !snapshot.reports.is_empty()
        {
            break snapshot;
        }
    };
    // The first few had started; the rest were still waiting.
    let reported: Vec<&str> = HOSTS
        .iter()
        .copied()
        .filter(|h| done.reports.contains_key(*h))
        .collect();
    assert_eq!(reported, HOSTS[..health::MAX_PARALLEL], "{reported:?}");
    drop(core);
}
