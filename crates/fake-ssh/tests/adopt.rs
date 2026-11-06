//! Surviving a crash (SPEC.md §7, M12) against `fake-ssh`: the supervisor
//! records what runs in `running.toml`; after a crash the next start takes
//! those processes over (Unix), and never anything else.
//!
//! The crash is real: the test runs this test binary again as a sshelf
//! (`crashing_sshelf`) that starts a tunnel and then aborts.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

mod common;

use sshelf_core::running::{RunningFile, RunningTunnel};
use sshelf_core::storage::Store;
use sshelf_core::supervisor::{Owner, Request, Snapshot, Supervisor, SupervisorEvent, TunnelState};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Tunnel};

const WAIT: Duration = Duration::from_secs(15);

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

struct Harness {
    supervisor: Supervisor,
    events: Receiver<SupervisorEvent>,
    last: Arc<Snapshot>,
    adopted: Option<usize>,
}

impl Harness {
    /// A supervisor that records in `dir`, as sshelf's own does.
    fn recorded(dir: &Path) -> Self {
        let (tx, events) = mpsc::channel();
        let supervisor =
            Supervisor::start_recorded(Some(fake_ssh().into()), dir.to_path_buf(), move |event| {
                let _ = tx.send(event);
            });
        Self {
            supervisor,
            events,
            last: Arc::new(Snapshot::default()),
            adopted: None,
        }
    }

    fn send(&self, request: Request) {
        self.supervisor.send(request);
    }

    /// Waits until `check` accepts a snapshot; returns it.
    fn until(&mut self, what: &str, check: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let deadline = Instant::now() + WAIT;
        while !check(&self.last) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(SupervisorEvent::Snapshot(s)) => self.last = s,
                Ok(SupervisorEvent::Error(e)) => panic!("error while waiting for {what}: {e}"),
                Ok(SupervisorEvent::Adopted(n)) => self.adopted = Some(n),
                Err(_) => panic!("timed out waiting for {what}; last: {:#?}", self.last),
            }
        }
        Arc::clone(&self.last)
    }
}

fn tunnel(id: &str, port: u16) -> Tunnel {
    Tunnel::ssh(
        id,
        &format!("tunnel {id}"),
        "ok",
        vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: port,
            target: Some("localhost:80".into()),
        }],
    )
}

fn record(dir: &Path) -> RunningFile {
    Store::new(dir).load::<RunningFile>().data
}

/// Waits until `running.toml` in `dir` satisfies `check`.
fn record_until(dir: &Path, what: &str, check: impl Fn(&RunningFile) -> bool) -> RunningFile {
    let deadline = Instant::now() + WAIT;
    loop {
        let file = record(dir);
        if check(&file) {
            return file;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {file:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What runs is recorded; a clean quit leaves nothing to take over.
#[test]
fn running_tunnels_are_recorded_and_a_clean_quit_clears_them() {
    let dir = tempfile::tempdir().unwrap();
    let port = common::hold_port().1;
    let mut h = Harness::recorded(dir.path());
    h.send(Request::Adopt {
        tunnels: Vec::new(),
    });
    let t = tunnel("a", port);
    h.send(Request::Start {
        tunnel: t.clone(),
        owner: Owner::Manual,
    });
    let pid = h
        .until("running", |s| s.state("a") == TunnelState::Running)
        .tunnels["a"]
        .pid;
    let file = record_until(dir.path(), "the record", |f| f.tunnels.len() == 1);
    let entry = &file.tunnels[0];
    assert_eq!((entry.id.as_str(), Some(entry.pid)), ("a", pid));
    assert_eq!(entry.args, t.ssh_args());
    assert_eq!(entry.owners, ["manual"]);
    drop(h);
    assert_eq!(record(dir.path()), RunningFile::default());
}

/// Entries that no longer run as recorded are dropped, and nothing is
/// taken over (on Windows never: tunnels die with sshelf there).
#[test]
fn entries_that_no_longer_run_as_recorded_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let t = tunnel("a", common::hold_port().1);
    // This test's own process, but "started" at another time: a reused pid.
    let stale = RunningFile {
        tunnels: vec![RunningTunnel {
            id: "a".into(),
            pid: std::process::id(),
            started: 1,
            program: None,
            args: t.ssh_args(),
            owners: vec!["manual".into()],
        }],
    };
    Store::new(dir.path()).save(&stale).unwrap();
    let mut h = Harness::recorded(dir.path());
    h.send(Request::Adopt {
        tunnels: vec![t.clone()],
    });
    record_until(dir.path(), "the record cleared", |f| f.tunnels.is_empty());
    let s = h.until("a snapshot", |_| true);
    assert!(!s.tunnels.contains_key("a"));
    assert_eq!(h.adopted, None);
}

#[cfg(unix)]
mod after_a_crash {
    use std::process::Command;

    use sshelf_core::child;

    use super::*;

    const CRASH_DIR: &str = "SSHELF_TEST_CRASH_DIR";
    const CRASH_PORT: &str = "SSHELF_TEST_CRASH_PORT";
    const CRASH_RECONNECT: &str = "SSHELF_TEST_CRASH_RECONNECT";
    /// Run a kubectl tunnel (M13) instead of an ssh one.
    const CRASH_KUBECTL: &str = "SSHELF_TEST_CRASH_KUBECTL";

    fn fake_kubectl() -> sshelf_core::ssh::Tool {
        sshelf_core::ssh::Tool {
            prefix: vec!["--as".into(), "kubectl".into()],
            ..sshelf_core::ssh::Tool::new(fake_ssh())
        }
    }

    fn kube_tunnel(id: &str, port: u16) -> Tunnel {
        use sshelf_core::tunnels::{Kube, KubeForward, Resource};
        Tunnel::kubectl(
            id,
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
        )
    }

    /// The sshelf that crashes: run by the tests below (and a no-op
    /// otherwise). It starts tunnel `a`, waits until it is recorded, and
    /// aborts: no destructors, no shutdown, like a real crash.
    #[test]
    fn crashing_sshelf() {
        let Some(dir) = std::env::var_os(CRASH_DIR) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let port = std::env::var(CRASH_PORT).unwrap().parse().unwrap();
        let mut h = Harness::recorded(&dir);
        h.send(Request::Adopt {
            tunnels: Vec::new(),
        });
        let mut t = if std::env::var_os(CRASH_KUBECTL).is_some() {
            h.send(Request::SetKubectl(Some(fake_kubectl())));
            kube_tunnel("a", port)
        } else {
            tunnel("a", port)
        };
        t.auto_reconnect = std::env::var_os(CRASH_RECONNECT).is_some();
        h.send(Request::Start {
            tunnel: t,
            owner: Owner::Manual,
        });
        h.until("running", |s| s.state("a") == TunnelState::Running);
        record_until(&dir, "the record", |f| f.tunnels.len() == 1);
        std::process::abort();
    }

    /// Runs the crashing sshelf on `dir`; returns what it left running.
    fn crash(dir: &Path, port: u16, reconnect: bool) -> RunningTunnel {
        crash_with(dir, port, reconnect, false)
    }

    fn crash_with(dir: &Path, port: u16, reconnect: bool, kubectl: bool) -> RunningTunnel {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["after_a_crash::crashing_sshelf", "--exact", "--nocapture"])
            .env(CRASH_DIR, dir)
            .env(CRASH_PORT, port.to_string());
        if reconnect {
            command.env(CRASH_RECONNECT, "1");
        }
        if kubectl {
            command.env(CRASH_KUBECTL, "1");
        }
        let status = command.status().unwrap();
        assert!(!status.success(), "it crashed");
        let file = record(dir);
        assert_eq!(file.tunnels.len(), 1, "it left its tunnel recorded");
        let left = file.tunnels[0].clone();
        assert!(child::start_time(left.pid).is_some(), "and running");
        left
    }

    fn gone(pid: u32) -> bool {
        let deadline = Instant::now() + WAIT;
        while child::start_time(pid).is_some() {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// Kills what a test left running (it leads its process group).
    fn kill_group(pid: u32) {
        child::TreeKiller::adopt(pid).kill();
    }

    #[test]
    fn a_tunnel_left_by_a_crash_is_taken_over_and_can_be_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let port = common::hold_port().1;
        let left = crash(dir.path(), port, false);

        let mut h = Harness::recorded(dir.path());
        h.send(Request::Adopt {
            tunnels: vec![tunnel("a", port)],
        });
        let s = h.until("taken over", |s| s.state("a") == TunnelState::Running);
        let status = &s.tunnels["a"];
        assert_eq!(status.pid, Some(left.pid), "the same process");
        assert!(status.owners.contains(&Owner::Manual));
        assert!(status.log.back().unwrap().text.starts_with("Taken over"));
        assert_eq!(h.adopted, Some(1));
        let file = record_until(dir.path(), "still recorded", |f| f.tunnels.len() == 1);
        assert_eq!(file.tunnels[0].pid, left.pid);

        h.send(Request::Stop {
            tunnel_id: "a".into(),
            owner: Owner::Manual,
        });
        h.until("stopped", |s| s.state("a") == TunnelState::Stopped);
        assert!(gone(left.pid), "Stop ends the adopted process");
        record_until(dir.path(), "the record cleared", |f| f.tunnels.is_empty());
    }

    #[test]
    fn an_adopted_tunnel_that_dies_fails_and_reconnects() {
        let dir = tempfile::tempdir().unwrap();
        let port = common::hold_port().1;
        let left = crash(dir.path(), port, true);

        let mut h = Harness::recorded(dir.path());
        let mut t = tunnel("a", port);
        t.auto_reconnect = true;
        h.send(Request::Adopt {
            tunnels: vec![t.clone()],
        });
        h.until("taken over", |s| s.state("a") == TunnelState::Running);
        kill_group(left.pid);
        let s = h.until("failed", |s| {
            matches!(s.state("a"), TunnelState::Failed(_)) && s.tunnels["a"].reconnect.is_some()
        });
        assert_eq!(s.state("a"), TunnelState::Failed("ssh exited".into()));
        let s = h.until("running again", |s| s.state("a") == TunnelState::Running);
        assert_ne!(s.tunnels["a"].pid, Some(left.pid), "a new process");
    }

    /// kubectl tunnels are recorded with their program and taken over the
    /// same way; an ssh tunnel now under the same id doesn't match.
    #[test]
    fn a_kubectl_tunnel_left_by_a_crash_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let port = common::hold_port().1;
        let left = crash_with(dir.path(), port, true, true);
        assert_eq!(left.program.as_deref(), Some("kubectl"));
        assert_eq!(left.args, kube_tunnel("a", port).args());

        let mut h = Harness::recorded(dir.path());
        h.send(Request::SetKubectl(Some(fake_kubectl())));
        let mut t = kube_tunnel("a", port);
        t.auto_reconnect = true;
        h.send(Request::Adopt { tunnels: vec![t] });
        let s = h.until("taken over", |s| s.state("a") == TunnelState::Running);
        assert_eq!(s.tunnels["a"].pid, Some(left.pid));
        assert_eq!(h.adopted, Some(1));
        kill_group(left.pid);
        let s = h.until("failed", |s| {
            matches!(s.state("a"), TunnelState::Failed(_)) && s.tunnels["a"].reconnect.is_some()
        });
        assert_eq!(s.state("a"), TunnelState::Failed("kubectl exited".into()));
        let s = h.until("running again", |s| s.state("a") == TunnelState::Running);
        assert_ne!(s.tunnels["a"].pid, Some(left.pid), "a new kubectl");
        drop(h);

        // The other way round: a record of kubectl, an ssh tunnel now.
        let dir = tempfile::tempdir().unwrap();
        let port = common::hold_port().1;
        let left = crash_with(dir.path(), port, false, true);
        let mut h = Harness::recorded(dir.path());
        h.send(Request::Adopt {
            tunnels: vec![tunnel("a", port)],
        });
        record_until(dir.path(), "the record cleared", |f| f.tunnels.is_empty());
        h.until("a snapshot", |_| true);
        assert_eq!(h.adopted, None);
        drop(h);
        assert!(child::start_time(left.pid).is_some(), "left alone");
        kill_group(left.pid);
    }

    /// A tunnel changed since the crash isn't taken over, and its process is
    /// left alone (it may be the user's to deal with).
    #[test]
    fn a_changed_tunnel_is_neither_taken_over_nor_killed() {
        let dir = tempfile::tempdir().unwrap();
        let port = common::hold_port().1;
        let left = crash(dir.path(), port, false);

        let mut h = Harness::recorded(dir.path());
        let mut changed = tunnel("a", port);
        changed.forwards[0].target = Some("localhost:8080".into());
        h.send(Request::Adopt {
            tunnels: vec![changed],
        });
        record_until(dir.path(), "the record cleared", |f| f.tunnels.is_empty());
        let s = h.until("a snapshot", |_| true);
        assert!(!s.tunnels.contains_key("a"));
        assert_eq!(h.adopted, None);
        drop(h);
        assert!(child::start_time(left.pid).is_some(), "still running");
        kill_group(left.pid);
    }
}
