//! Host health checks through the core (SPEC.md §12, M8) against fake-ssh,
//! which answers `… <alias> true` by alias.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sshelf_core::core::{Command, Core, Event};
use sshelf_core::health::{Health, HealthSnapshot};
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

#[test]
fn checks_run_a_few_at_a_time_and_classify_every_outcome() {
    let tmp = tempfile::tempdir().unwrap();
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
    let (core, events) = Core::start(paths, Arc::new(|| {}));

    let all: Vec<String> = HOSTS.iter().map(|h| h.to_string()).collect();
    core.send(Command::CheckHealth(all.clone()));
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
