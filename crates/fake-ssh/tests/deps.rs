//! Tunnel dependencies in the supervisor (SPEC.md §7 *Order and
//! dependencies*, M15), with fake-ssh as ssh and as kubectl: a dependent
//! waits for its dependency, isn't started when that fails, keeps running
//! when it drops, stops first; a tunnel started alone pulls its dependencies
//! in (`NeededBy`) and lets them go; a group owns its outside dependencies;
//! a loop never starts. No test talks to a real server or cluster.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

mod common;
#[path = "common/holder.rs"]
mod holder;

use sshelf_core::ssh::Tool;
use sshelf_core::supervisor::{
    FailureKind, Owner, Request, Snapshot, Supervisor, SupervisorEvent, TunnelState,
};
use sshelf_core::tunnels::{
    DEFAULT_BIND, Forward, ForwardKind, Group, Kube, KubeForward, Resource, Tunnel,
};

const WAIT: Duration = Duration::from_secs(20);

fn fake() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

struct Harness {
    supervisor: Supervisor,
    events: Receiver<SupervisorEvent>,
    /// Every snapshot seen, oldest first.
    seen: Vec<Arc<Snapshot>>,
}

impl Harness {
    /// fake-ssh as ssh and as kubectl, knowing `tunnels`.
    fn new(tunnels: &[&Tunnel]) -> Self {
        Self::with(None, tunnels)
    }

    fn with(app_dir: Option<PathBuf>, tunnels: &[&Tunnel]) -> Self {
        let (tx, events) = mpsc::channel();
        let send = move |event| {
            let _ = tx.send(event);
        };
        let supervisor = match app_dir {
            Some(dir) => Supervisor::start_recorded(Some(fake().into()), dir, send),
            None => Supervisor::start(Some(fake().into()), send),
        };
        supervisor.send(Request::SetKubectl(Some(Tool {
            prefix: vec!["--as".into(), "kubectl".into()],
            ..Tool::new(fake())
        })));
        supervisor.send(Request::SetTunnels(
            tunnels.iter().map(|t| (*t).clone()).collect(),
        ));
        Self {
            supervisor,
            events,
            seen: vec![Arc::new(Snapshot::default())],
        }
    }

    fn send(&self, request: Request) {
        self.supervisor.send(request);
    }

    fn last(&self) -> &Snapshot {
        self.seen.last().expect("a snapshot")
    }

    fn until(&mut self, what: &str, check: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let deadline = Instant::now() + WAIT;
        while !check(self.last()) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(SupervisorEvent::Snapshot(s)) => self.seen.push(s),
                Ok(SupervisorEvent::Error(e)) => panic!(
                    "unexpected error while waiting for {what}: {}",
                    holder::explain(&e)
                ),
                Ok(SupervisorEvent::Adopted(_)) => {}
                Err(_) => panic!("timed out waiting for {what}; last: {:#?}", self.last()),
            }
        }
        Arc::clone(self.seen.last().expect("a snapshot"))
    }
}

/// An ssh tunnel to `host` (its name picks fake-ssh's behaviour).
fn ssh(id: &str, host: &str) -> Tunnel {
    Tunnel::ssh(
        id,
        &format!("ssh {id}"),
        host,
        vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: common::hold_port().1,
            target: Some("localhost:6443".into()),
        }],
    )
}

/// A kubectl tunnel to `resource` (its name picks the fake's behaviour).
fn kube(id: &str, resource: &str) -> Tunnel {
    Tunnel::kubectl(
        id,
        &format!("kube {id}"),
        Kube {
            context: String::new(),
            namespace: "fake-mail".into(),
            resource: Resource::parse(resource).unwrap(),
            address: DEFAULT_BIND.into(),
            forwards: vec![KubeForward {
                listen: common::hold_port().1,
                target: "8025".into(),
            }],
        },
    )
}

fn on(mut tunnel: Tunnel, deps: &[&Tunnel]) -> Tunnel {
    tunnel.depends_on = deps.iter().map(|t| t.id.clone()).collect();
    tunnel
}

fn group(id: &str, tunnels: &[&Tunnel]) -> Group {
    Group {
        id: id.into(),
        name: format!("group {id}"),
        tunnels: tunnels.iter().map(|t| t.id.clone()).collect(),
        start_on_launch: false,
        start_deps_without_asking: false,
    }
}

fn start_group(h: &Harness, g: &Group, tunnels: &[&Tunnel]) {
    h.send(Request::StartGroup {
        group: g.clone(),
        tunnels: tunnels.iter().map(|t| (*t).clone()).collect(),
    });
}

fn is(id: &str, state: TunnelState) -> impl Fn(&Snapshot) -> bool + '_ {
    move |s| s.state(id) == state
}

/// The first snapshot in which `id` has a process.
fn first_spawn<'a>(seen: &'a [Arc<Snapshot>], id: &str) -> &'a Snapshot {
    seen.iter()
        .find(|s| s.tunnels.get(id).is_some_and(|t| t.pid.is_some()))
        .expect("it spawned")
}

/// The k3s API over ssh and a kubectl tunnel through it (the owner's Dev):
/// started alone, mailpit pulls k3s in, waits for it, and lets it go.
#[test]
fn a_dependent_waits_for_its_dependency_and_lets_it_go() {
    let k3s = ssh("k3s", "slow-k3s");
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let mut h = Harness::new(&[&k3s, &mail]);
    h.send(Request::Start {
        tunnel: mail.clone(),
        owner: Owner::Manual,
    });
    let waiting = h.until("mailpit waiting", |s| {
        s.tunnels
            .get("mail")
            .is_some_and(|t| t.after.as_deref() == Some("k3s") && t.pid.is_none())
    });
    assert_eq!(waiting.state("mail"), TunnelState::Starting);
    assert!(
        waiting.tunnels["k3s"]
            .owners
            .contains(&Owner::NeededBy("mail".into()))
    );
    h.until("mailpit running", is("mail", TunnelState::Running));
    // It spawned only once k3s ran.
    assert_eq!(
        first_spawn(&h.seen, "mail").state("k3s"),
        TunnelState::Running
    );

    h.send(Request::Stop {
        tunnel_id: "mail".into(),
        owner: Owner::Manual,
    });
    h.until("both stopped", |s| {
        s.state("mail") == TunnelState::Stopped && s.state("k3s") == TunnelState::Stopped
    });
}

/// k3s fails: what depends on it, directly or through others, isn't
/// started; *Retry failed* (the group again) starts them in order once it
/// works.
#[test]
fn a_failed_dependency_leaves_dependents_not_started_until_retried() {
    let k3s = ssh("k3s", "fail-auth-k3s");
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let web = on(ssh("web", "ok-web"), &[&mail]);
    let mut h = Harness::new(&[&k3s, &mail, &web]);
    let dev = group("dev", &[&k3s, &mail, &web]);
    start_group(&h, &dev, &[&k3s, &mail, &web]);
    let s = h.until("web not started", |s| {
        matches!(s.state("web"), TunnelState::Failed(_))
    });
    assert_eq!(s.tunnels["k3s"].failure, Some(FailureKind::Auth));
    assert_eq!(s.tunnels["mail"].failure, Some(FailureKind::NotStarted));
    assert_eq!(
        s.state("mail"),
        TunnelState::Failed("Not started \u{b7} ssh k3s failed".into())
    );
    assert_eq!(
        s.state("web"),
        TunnelState::Failed("Not started \u{b7} kube mail wasn't started".into())
    );
    assert!(s.tunnels["mail"].pid.is_none() && s.tunnels["mail"].reconnect.is_none());

    // Fixed (the host works now) and retried.
    let k3s = Tunnel {
        host: "ok-k3s".into(),
        ..k3s
    };
    h.send(Request::SetTunnels(vec![
        k3s.clone(),
        mail.clone(),
        web.clone(),
    ]));
    start_group(&h, &dev, &[&k3s, &mail, &web]);
    h.until("all running", |s| {
        ["k3s", "mail", "web"]
            .iter()
            .all(|id| s.state(id) == TunnelState::Running)
    });
    let mail_up = first_spawn(&h.seen, "mail");
    assert_eq!(mail_up.state("k3s"), TunnelState::Running);
    let web_up = first_spawn(&h.seen, "web");
    assert_eq!(web_up.state("mail"), TunnelState::Running);
}

/// A dependency that drops and reconnects: what started while it was
/// reconnecting waits and starts once it runs again.
#[test]
fn a_reconnecting_dependency_starts_its_waiting_dependents() {
    let mut k3s = ssh("k3s", "die-later-k3s");
    k3s.auto_reconnect = true;
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let mut h = Harness::new(&[&k3s, &mail]);
    h.send(Request::Start {
        tunnel: k3s.clone(),
        owner: Owner::Manual,
    });
    h.until("k3s reconnecting", |s| {
        s.tunnels.get("k3s").is_some_and(|t| t.reconnect.is_some())
    });
    h.send(Request::Start {
        tunnel: mail.clone(),
        owner: Owner::Manual,
    });
    h.until("mailpit running", is("mail", TunnelState::Running));
    let mail_up = first_spawn(&h.seen, "mail");
    assert_eq!(mail_up.state("k3s"), TunnelState::Running);
}

/// A dependency that drops doesn't take its dependents down.
#[test]
fn a_dropped_dependency_doesnt_stop_its_dependents() {
    let k3s = ssh("k3s", "die-later-k3s");
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let mut h = Harness::new(&[&k3s, &mail]);
    let dev = group("dev", &[&k3s, &mail]);
    start_group(&h, &dev, &[&k3s, &mail]);
    h.until("k3s dropped", |s| {
        matches!(s.state("k3s"), TunnelState::Failed(_))
    });
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(SupervisorEvent::Snapshot(s)) = h.events.try_recv() {
        h.seen.push(s);
    }
    assert_eq!(h.last().state("mail"), TunnelState::Running);
}

/// Stopping goes bottom-up: k3s stops only once mailpit has exited; a
/// tunnel another owner holds keeps running.
#[test]
fn stopping_goes_bottom_up_and_respects_other_owners() {
    let k3s = ssh("k3s", "ok-k3s");
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let mut h = Harness::new(&[&k3s, &mail]);
    let dev = group("dev", &[&k3s, &mail]);
    start_group(&h, &dev, &[&k3s, &mail]);
    h.until("both running", |s| {
        s.state("k3s") == TunnelState::Running && s.state("mail") == TunnelState::Running
    });
    h.send(Request::StopGroup {
        group_id: "dev".into(),
    });
    h.until("both stopped", |s| {
        s.state("k3s") == TunnelState::Stopped && s.state("mail") == TunnelState::Stopped
    });
    let k3s_down = h
        .seen
        .iter()
        .find(|s| s.state("k3s") == TunnelState::Stopped && s.tunnels.contains_key("k3s"))
        .expect("k3s stopped");
    // The order itself; "Stopping after mail" can be too short to land in a
    // snapshot, so it isn't checked here.
    assert_eq!(k3s_down.state("mail"), TunnelState::Stopped);

    // Started by hand too: stopping the group leaves it up.
    start_group(&h, &dev, &[&k3s, &mail]);
    h.send(Request::Start {
        tunnel: k3s.clone(),
        owner: Owner::Manual,
    });
    h.until("both running again", |s| {
        s.state("k3s") == TunnelState::Running && s.state("mail") == TunnelState::Running
    });
    h.send(Request::StopGroup {
        group_id: "dev".into(),
    });
    h.until("mailpit stopped", is("mail", TunnelState::Stopped));
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(SupervisorEvent::Snapshot(s)) = h.events.try_recv() {
        h.seen.push(s);
    }
    assert_eq!(h.last().state("k3s"), TunnelState::Running);
}

/// A group whose member depends on a tunnel outside it owns that tunnel
/// while it runs ("needed by Dev") and lets it go when it stops.
#[test]
fn a_group_pulls_in_and_releases_an_outside_dependency() {
    let k3s = ssh("k3s", "ok-k3s");
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let mut h = Harness::new(&[&k3s, &mail]);
    let dev = group("dev", &[&mail]);
    start_group(&h, &dev, &[&mail]);
    let s = h.until("both running", |s| {
        s.state("k3s") == TunnelState::Running && s.state("mail") == TunnelState::Running
    });
    assert!(
        s.tunnels["k3s"]
            .owners
            .contains(&Owner::Group("dev".into()))
    );
    h.send(Request::StopGroup {
        group_id: "dev".into(),
    });
    h.until("both stopped", |s| {
        s.state("k3s") == TunnelState::Stopped && s.state("mail") == TunnelState::Stopped
    });
}

/// A hand-edited loop: none of its tunnels starts.
#[test]
fn a_dependency_loop_never_starts() {
    let a = ssh("a", "ok-a");
    let b = on(ssh("b", "ok-b"), &[&a]);
    let a = on(a, &[&b]);
    let mut h = Harness::new(&[&a, &b]);
    h.send(Request::Start {
        tunnel: a.clone(),
        owner: Owner::Manual,
    });
    let s = h.until("a failed", |s| {
        matches!(s.state("a"), TunnelState::Failed(_))
    });
    assert_eq!(s.state("a"), TunnelState::Failed("Dependency loop".into()));
    assert_eq!(s.tunnels["a"].failure, Some(FailureKind::DependencyLoop));
    assert!(
        h.seen
            .iter()
            .all(|s| s.tunnels.values().all(|t| t.pid.is_none()))
    );
}

/// `running.toml` keeps `NeededBy`, so a takeover after a crash knows why
/// a dependency runs.
#[test]
fn running_toml_keeps_needed_by() {
    let dir = tempfile::tempdir().unwrap();
    let k3s = ssh("k3s", "ok-k3s");
    let mail = on(kube("mail", "service/mailpit"), &[&k3s]);
    let mut h = Harness::with(Some(dir.path().to_path_buf()), &[&k3s, &mail]);
    h.send(Request::Adopt {
        tunnels: Vec::new(),
    });
    h.send(Request::Start {
        tunnel: mail.clone(),
        owner: Owner::Manual,
    });
    h.until("mailpit running", is("mail", TunnelState::Running));
    let deadline = Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(dir.path().join("running.toml")).unwrap_or_default();
        if text.contains("needed-by:mail") {
            break;
        }
        assert!(Instant::now() < deadline, "{text}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
