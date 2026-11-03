//! Supervisor semantics (SPEC.md §7) against `fake-ssh`: owners and
//! reference counting, port pre-checks, degraded groups, overlapping groups,
//! failure reasons, logs, and stopping everything on shutdown.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use sshelf_core::supervisor::{
    FailureKind, GroupState, LOG_LINES, Owner, Request, Snapshot, Supervisor, SupervisorEvent,
    TunnelState,
};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Group, Tunnel};

const WAIT: Duration = Duration::from_secs(15);

struct Harness {
    supervisor: Supervisor,
    events: Receiver<SupervisorEvent>,
    last: Arc<Snapshot>,
}

impl Harness {
    fn new() -> Self {
        let (tx, events) = mpsc::channel();
        let supervisor = Supervisor::start(
            Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))),
            move |event| {
                let _ = tx.send(event);
            },
        );
        Self {
            supervisor,
            events,
            last: Arc::new(Snapshot::default()),
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
                Ok(SupervisorEvent::Error(e)) => {
                    panic!("unexpected error while waiting for {what}: {e}")
                }
                Err(_) => panic!("timed out waiting for {what}; last: {:#?}", self.last),
            }
        }
        Arc::clone(&self.last)
    }

    fn error(&mut self) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(SupervisorEvent::Error(e)) => return e,
                Ok(SupervisorEvent::Snapshot(s)) => self.last = s,
                Err(_) => panic!("timed out waiting for an error"),
            }
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn tunnel(id: &str, host: &str, port: u16) -> Tunnel {
    Tunnel {
        id: id.into(),
        name: format!("tunnel {id}"),
        host: host.into(),
        forwards: vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: port,
            target: Some("localhost:80".into()),
        }],
        auto_reconnect: false,
    }
}

fn group(id: &str, tunnels: &[&Tunnel]) -> Group {
    Group {
        id: id.into(),
        name: format!("group {id}"),
        tunnels: tunnels.iter().map(|t| t.id.clone()).collect(),
        start_on_launch: false,
    }
}

fn start_group(h: &Harness, g: &Group, tunnels: &[&Tunnel]) {
    h.send(Request::StartGroup {
        group: g.clone(),
        tunnels: tunnels.iter().map(|t| (*t).clone()).collect(),
    });
}

fn running(id: &str) -> impl Fn(&Snapshot) -> bool + '_ {
    move |s| s.state(id) == TunnelState::Running
}

fn stopped(id: &str) -> impl Fn(&Snapshot) -> bool + '_ {
    move |s| s.state(id) == TunnelState::Stopped
}

#[test]
fn starting_twice_shares_one_process() {
    let mut h = Harness::new();
    let port = free_port();
    let t = tunnel("a", "ok", port);
    h.send(Request::Start {
        tunnel: t.clone(),
        owner: Owner::Manual,
    });
    let pid = h.until("running", running("a")).tunnels["a"].pid;
    assert!(pid.is_some());
    assert!(!port_is_free(port));

    let g = group("g", &[&t]);
    start_group(&h, &g, &[&t]);
    let s = h.until("two owners", |s| s.tunnels["a"].owners.len() == 2);
    assert_eq!(s.tunnels["a"].pid, pid, "no second process");
    assert_eq!(s.state("a"), TunnelState::Running);

    // Manual stop: the group still holds it.
    h.send(Request::Stop {
        tunnel_id: "a".into(),
        owner: Owner::Manual,
    });
    let s = h.until("one owner", |s| s.tunnels["a"].owners.len() == 1);
    assert_eq!(s.state("a"), TunnelState::Running);
    assert!(s.tunnels["a"].owners.contains(&Owner::Group("g".into())));

    h.send(Request::StopGroup {
        group_id: "g".into(),
    });
    h.until("stopped", stopped("a"));
    let deadline = Instant::now() + WAIT;
    while !port_is_free(port) {
        assert!(Instant::now() < deadline, "the port was not released");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn port_conflict_starts_nothing() {
    let mut h = Harness::new();
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let (a, b) = (tunnel("a", "ok", free_port()), tunnel("b", "ok", taken));
    let g = group("g", &[&a, &b]);
    start_group(&h, &g, &[&a, &b]);
    let error = h.error();
    assert!(error.contains(&format!("port {taken}")), "{error}");
    assert!(error.contains("tunnel b"), "{error}");
    assert!(h.last.tunnels.is_empty(), "{:#?}", h.last);
}

#[test]
fn partial_failure_degrades_the_group_and_retry_restarts_only_failures() {
    let mut h = Harness::new();
    let (ok, bad) = (
        tunnel("ok", "ok-host", free_port()),
        tunnel("bad", "fail-auth-host", free_port()),
    );
    let g = group("g", &[&ok, &bad]);
    start_group(&h, &g, &[&ok, &bad]);
    let s = h.until("degraded", |s| {
        s.group_state(&g)
            == GroupState::Degraded {
                running: 1,
                total: 2,
            }
    });
    let TunnelState::Failed(reason) = s.state("bad") else {
        panic!("not failed: {:?}", s.state("bad"));
    };
    assert!(reason.contains("Permission denied"), "{reason}");
    assert_eq!(s.tunnels["bad"].failure, Some(FailureKind::Auth));
    let pid = s.tunnels["ok"].pid;

    // "Retry failed" is starting the group again.
    start_group(&h, &g, &[&ok, &bad]);
    let s = h.until("bad retried", |s| {
        s.tunnels["bad"]
            .log
            .iter()
            .filter(|l| l.text.starts_with("starting ssh"))
            .count()
            == 2
    });
    assert_eq!(
        s.tunnels["ok"].pid, pid,
        "the running tunnel was left alone"
    );
    h.until("degraded again", |s| {
        s.group_state(&g)
            == GroupState::Degraded {
                running: 1,
                total: 2,
            }
    });
}

#[test]
fn overlapping_groups_keep_shared_tunnels_up() {
    let mut h = Harness::new();
    let (a, b, c) = (
        tunnel("a", "ok", free_port()),
        tunnel("b", "ok", free_port()),
        tunnel("c", "ok", free_port()),
    );
    let (g1, g2) = (group("g1", &[&a, &b]), group("g2", &[&b, &c]));
    start_group(&h, &g1, &[&a, &b]);
    start_group(&h, &g2, &[&b, &c]);
    h.until("both groups running", |s| {
        s.group_state(&g1) == GroupState::Running && s.group_state(&g2) == GroupState::Running
    });

    h.send(Request::StopGroup {
        group_id: "g1".into(),
    });
    let s = h.until("g1 stopped", |s| s.state("a") == TunnelState::Stopped);
    assert_eq!(s.state("b"), TunnelState::Running);
    assert_eq!(
        s.tunnels["b"].owners.iter().collect::<Vec<_>>(),
        [&Owner::Group("g2".into())],
        "b is still used by g2"
    );
    assert_eq!(s.state("c"), TunnelState::Running);
    assert_eq!(s.group_state(&g1), GroupState::Stopped);
    assert_eq!(s.group_state(&g2), GroupState::Running);
}

#[test]
fn slow_tunnels_start_then_run_and_drops_fail_with_ssh_message() {
    let mut h = Harness::new();
    let slow = tunnel("slow", "slow-host", free_port());
    h.send(Request::Start {
        tunnel: slow,
        owner: Owner::Manual,
    });
    h.until("starting", |s| s.state("slow") == TunnelState::Starting);
    h.until("running", running("slow"));

    let dies = tunnel("dies", "die-later-host", free_port());
    h.send(Request::Start {
        tunnel: dies,
        owner: Owner::Manual,
    });
    let s = h.until("failed", |s| {
        matches!(s.state("dies"), TunnelState::Failed(_))
    });
    assert_eq!(
        s.state("dies"),
        TunnelState::Failed("Connection to die-later-host closed by remote host.".into())
    );
    assert!(
        s.tunnels["dies"].owners.contains(&Owner::Manual),
        "owners survive failure"
    );
}

#[test]
fn changed_host_keys_are_recognised() {
    let mut h = Harness::new();
    h.send(Request::Start {
        tunnel: tunnel("c", "changed-host", free_port()),
        owner: Owner::Manual,
    });
    let s = h.until("failed", |s| matches!(s.state("c"), TunnelState::Failed(_)));
    assert_eq!(s.tunnels["c"].failure, Some(FailureKind::HostKeyChanged));
    assert_eq!(
        s.state("c"),
        TunnelState::Failed("Host key verification failed.".into())
    );
}

#[test]
fn log_keeps_the_last_lines_and_marks_debug() {
    let mut h = Harness::new();
    h.send(Request::Start {
        tunnel: tunnel("n", "noisy-host", free_port()),
        owner: Owner::Manual,
    });
    let s = h.until("running", running("n"));
    let log = &s.tunnels["n"].log;
    assert_eq!(log.len(), LOG_LINES);
    assert!(
        log.back()
            .unwrap()
            .text
            .contains("Entering interactive session.")
    );
    assert!(log.iter().any(|l| l.debug));
    assert!(
        log.iter()
            .any(|l| !l.debug && l.text.starts_with("Authenticated to"))
    );
}

#[test]
fn restart_while_stopping_waits_for_the_old_process() {
    let mut h = Harness::new();
    let port = free_port();
    let t = tunnel("a", "ok", port);
    h.send(Request::Start {
        tunnel: t.clone(),
        owner: Owner::Manual,
    });
    let first = h.until("running", running("a")).tunnels["a"].pid;
    h.send(Request::Stop {
        tunnel_id: "a".into(),
        owner: Owner::Manual,
    });
    h.send(Request::Start {
        tunnel: t,
        owner: Owner::Manual,
    });
    let s = h.until("running again", |s| {
        s.state("a") == TunnelState::Running && s.tunnels["a"].pid != first
    });
    assert!(s.tunnels["a"].pid.is_some());
}

#[test]
fn shutdown_stops_every_tunnel() {
    let mut h = Harness::new();
    let ports = [free_port(), free_port()];
    for (i, port) in ports.iter().enumerate() {
        h.send(Request::Start {
            tunnel: tunnel(&format!("t{i}"), "ok", *port),
            owner: Owner::Manual,
        });
    }
    h.until("both running", |s| {
        s.state("t0") == TunnelState::Running && s.state("t1") == TunnelState::Running
    });
    drop(h);
    let deadline = Instant::now() + WAIT;
    while !ports.iter().all(|p| port_is_free(*p)) {
        assert!(Instant::now() < deadline, "tunnels survived shutdown");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn removing_a_running_tunnel_stops_it() {
    let mut h = Harness::new();
    let port = free_port();
    h.send(Request::Start {
        tunnel: tunnel("a", "ok", port),
        owner: Owner::Group("g".into()),
    });
    h.until("running", running("a"));
    h.send(Request::Remove {
        tunnel_id: "a".into(),
    });
    h.until("gone", |s| !s.tunnels.contains_key("a"));
    let deadline = Instant::now() + WAIT;
    while !port_is_free(port) {
        assert!(Instant::now() < deadline, "the process survived removal");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn reconnecting(id: &str, attempt: u32) -> impl Fn(&Snapshot) -> bool + '_ {
    move |s| {
        s.tunnels
            .get(id)
            .and_then(|t| t.reconnect)
            .is_some_and(|r| r.attempt == attempt)
    }
}

#[test]
fn dropped_tunnels_reconnect_with_backoff() {
    let mut h = Harness::new();
    let drops = Tunnel {
        auto_reconnect: true,
        ..tunnel("drops", "die-later-host", free_port())
    };
    h.send(Request::Start {
        tunnel: drops,
        owner: Owner::Manual,
    });
    h.until("running", running("drops"));
    let s = h.until("first reconnect planned", reconnecting("drops", 1));
    assert!(matches!(s.state("drops"), TunnelState::Failed(_)));
    // After the 2 s wait it starts again by itself, then drops again: the
    // next wait is longer.
    h.until("running again", running("drops"));
    let s = h.until("second reconnect planned", reconnecting("drops", 2));
    let log: Vec<&str> = s.tunnels["drops"]
        .log
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(log.contains(&"reconnecting in 2 s (attempt 1)"), "{log:?}");
    assert!(log.contains(&"reconnecting in 4 s (attempt 2)"), "{log:?}");

    // Stopping cancels the pending restart.
    h.send(Request::Stop {
        tunnel_id: "drops".into(),
        owner: Owner::Manual,
    });
    let s = h.until("stopped", stopped("drops"));
    assert_eq!(s.tunnels["drops"].reconnect, None);
    std::thread::sleep(Duration::from_secs(5));
    while let Ok(event) = h.events.try_recv() {
        if let SupervisorEvent::Snapshot(s) = event {
            assert_eq!(
                s.state("drops"),
                TunnelState::Stopped,
                "restarted after Stop"
            );
        }
    }
}

#[test]
fn auth_failures_are_not_retried() {
    let mut h = Harness::new();
    let denied = Tunnel {
        auto_reconnect: true,
        ..tunnel("denied", "fail-auth-host", free_port())
    };
    h.send(Request::Start {
        tunnel: denied,
        owner: Owner::Manual,
    });
    let s = h.until("failed", |s| {
        matches!(s.state("denied"), TunnelState::Failed(_))
    });
    assert_eq!(s.tunnels["denied"].failure, Some(FailureKind::Auth));
    assert_eq!(s.tunnels["denied"].reconnect, None);
}
