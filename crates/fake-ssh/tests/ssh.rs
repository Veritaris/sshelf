//! sshelf-core's process handling against `fake-ssh` (SPEC.md §7: tests that
//! spawn processes live here to get `CARGO_BIN_EXE_fake-ssh`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sshelf_core::process::{self, RunError};
use sshelf_core::ssh::{SshTools, ToolSource};

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

fn tools() -> SshTools {
    SshTools::locate(Some(&fake_ssh())).expect("fake-ssh exists")
}

#[test]
fn run_captures_output_and_exit_code() {
    let mut cmd = process::command(&fake_ssh());
    cmd.env("FAKE_SSH_STDERR", "boom").env("FAKE_SSH_EXIT", "3");
    let out = process::run(cmd, Duration::from_secs(10)).unwrap();
    assert_eq!(out.code, Some(3));
    assert_eq!(out.stderr.trim(), "boom");
    assert_eq!(out.stdout, "");
}

#[test]
fn run_kills_the_process_on_timeout() {
    let mut cmd = process::command(&fake_ssh());
    cmd.env("FAKE_SSH_SLEEP_MS", "20000");
    let started = Instant::now();
    let err = process::run(cmd, Duration::from_millis(300)).unwrap_err();
    assert!(matches!(err, RunError::Timeout { .. }), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn run_reports_a_missing_program() {
    let cmd = process::command(&fake_ssh().with_file_name("does-not-exist"));
    let err = process::run(cmd, Duration::from_secs(1)).unwrap_err();
    assert!(matches!(err, RunError::Spawn { .. }), "{err}");
}

#[test]
fn version_reads_the_first_stderr_line() {
    let tools = tools();
    assert_eq!(tools.source, ToolSource::Override);
    assert_eq!(tools.version().unwrap(), "OpenSSH_fake_1.0, fake-ssh");
}

#[test]
fn resolve_parses_ssh_g_output() {
    let resolved = tools().resolve("web").unwrap();
    assert_eq!(resolved.get("hostname"), Some("web.example.com"));
    assert_eq!(resolved.get("user"), Some("fake"));
    let identities = resolved
        .options
        .iter()
        .filter(|(k, _)| k == "identityfile")
        .count();
    assert_eq!(identities, 2);
}

#[test]
fn resolve_passes_dash_aliases_as_aliases() {
    let resolved = tools().resolve("-weird").unwrap();
    assert_eq!(resolved.get("host"), Some("-weird"));
}

#[test]
fn resolve_reports_ssh_errors() {
    let err = tools().resolve("fail-me").unwrap_err();
    assert!(err.contains("Bad configuration option"), "{err}");
}
