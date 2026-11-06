//! kubectl tunnels in the supervisor (SPEC.md §7 *kubectl tunnels*) against
//! `fake-ssh --as kubectl`: Starting → Running only once every forward is
//! up, each failure kind, which of them reconnect, drops (kubectl exiting or
//! staying up after `lost connection to pod`), a group mixing ssh and
//! kubectl, and a missing kubectl. No test talks to a real cluster.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

mod common;

use sshelf_core::ssh::Tool;
use sshelf_core::supervisor::{
    FailureKind, GroupState, Owner, Request, Snapshot, Supervisor, SupervisorEvent, TunnelState,
};
use sshelf_core::tunnels::{
    DEFAULT_BIND, Forward, ForwardKind, Group, Kube, KubeForward, Resource, Tunnel,
};

const WAIT: Duration = Duration::from_secs(15);

fn fake() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

/// fake-ssh acting as kubectl.
fn fake_kubectl() -> Tool {
    Tool {
        prefix: vec!["--as".into(), "kubectl".into()],
        ..Tool::new(fake())
    }
}

struct Harness {
    supervisor: Supervisor,
    events: Receiver<SupervisorEvent>,
    last: Arc<Snapshot>,
}

impl Harness {
    /// With fake ssh and, if `kubectl`, fake kubectl.
    fn new(kubectl: bool) -> Self {
        let (tx, events) = mpsc::channel();
        let supervisor = Supervisor::start(Some(fake().into()), move |event| {
            let _ = tx.send(event);
        });
        if kubectl {
            supervisor.send(Request::SetKubectl(Some(fake_kubectl())));
        }
        Self {
            supervisor,
            events,
            last: Arc::new(Snapshot::default()),
        }
    }

    fn send(&self, request: Request) {
        self.supervisor.send(request);
    }

    fn start(&self, tunnel: &Tunnel) {
        self.send(Request::Start {
            tunnel: tunnel.clone(),
            owner: Owner::Manual,
        });
    }

    fn until(&mut self, what: &str, check: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let deadline = Instant::now() + WAIT;
        while !check(&self.last) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(SupervisorEvent::Snapshot(s)) => self.last = s,
                Ok(SupervisorEvent::Error(e)) => {
                    panic!("unexpected error while waiting for {what}: {e}")
                }
                Ok(SupervisorEvent::Adopted(_)) => {}
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
                Ok(SupervisorEvent::Adopted(_)) => {}
                Err(_) => panic!("timed out waiting for an error"),
            }
        }
    }
}

fn listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

fn free_port() -> u16 {
    common::hold_port().1
}

/// A kubectl tunnel to `<kind>/<name>` with one forward per port; `name`
/// picks the fake's behaviour.
fn kube(id: &str, context: &str, resource: &str, ports: &[u16]) -> Tunnel {
    Tunnel::kubectl(
        id,
        &format!("kube {id}"),
        Kube {
            context: context.into(),
            namespace: "fake-api".into(),
            resource: Resource::parse(resource).unwrap(),
            address: DEFAULT_BIND.into(),
            forwards: ports
                .iter()
                .map(|&listen| KubeForward {
                    listen,
                    target: "80".into(),
                })
                .collect(),
        },
    )
}

fn reconnecting(id: &str) -> impl Fn(&Snapshot) -> bool + '_ {
    move |s| s.tunnels.get(id).is_some_and(|t| t.reconnect.is_some())
}

fn failed(id: &str) -> impl Fn(&Snapshot) -> bool + '_ {
    move |s| matches!(s.state(id), TunnelState::Failed(_))
}

#[test]
fn running_only_after_every_forward_is_up() {
    let mut h = Harness::new(true);
    let (a, b) = (free_port(), free_port());
    let t = kube("k", "", "service/slow-web", &[a, b]);
    h.start(&t);
    // The first forward is up while the second still waits.
    let s = h.until("the first forward", |s| {
        s.tunnels
            .get("k")
            .is_some_and(|t| t.log.iter().any(|l| l.text.contains("Forwarding from")))
    });
    assert_eq!(s.state("k"), TunnelState::Starting);
    let s = h.until("running", |s| s.state("k") == TunnelState::Running);
    let log: Vec<&str> = s.tunnels["k"].log.iter().map(|l| l.text.as_str()).collect();
    assert!(log[0].starts_with("starting kubectl (pid "), "{log:?}");
    assert!(log.contains(&format!("Forwarding from 127.0.0.1:{b} -> 80").as_str()));
    assert!(listening(a) && listening(b));
    h.send(Request::Stop {
        tunnel_id: "k".into(),
        owner: Owner::Manual,
    });
    h.until("stopped", |s| s.state("k") == TunnelState::Stopped);
    assert!(!listening(a));
}

#[test]
fn localhost_prints_each_port_twice_and_counts_it_once() {
    let mut h = Harness::new(true);
    let port = free_port();
    let mut t = kube("k", "", "pod/handling-web", &[port]);
    t.kube.address = "localhost".into();
    h.start(&t);
    let s = h.until("a handled connection", |s| {
        s.tunnels
            .get("k")
            .is_some_and(|t| t.log.iter().any(|l| l.text.starts_with("Handling")))
    });
    assert_eq!(s.state("k"), TunnelState::Running);
    let log = &s.tunnels["k"].log;
    assert!(log.iter().any(|l| l.text.contains("[::1]")));
    // "Handling connection for" is a debug line.
    assert!(
        log.iter()
            .find(|l| l.text.starts_with("Handling"))
            .unwrap()
            .debug
    );
}

/// Each kind of failure from kubectl's own lines, and whether
/// auto-reconnect retries it.
#[test]
fn failures_are_classified_and_only_some_reconnect() {
    let mut h = Harness::new(true);
    let cases = [
        ("gone", "", "pod/gone-web", FailureKind::ResourceGone, false),
        (
            "auth",
            "",
            "service/unauth-web",
            FailureKind::NotLoggedIn,
            false,
        ),
        (
            "ctx",
            "missing-ctx",
            "service/web",
            FailureKind::NoContext,
            false,
        ),
        (
            "net",
            "",
            "service/unreachable-web",
            FailureKind::ClusterUnreachable,
            true,
        ),
    ];
    for (id, context, resource, kind, reconnects) in cases {
        let mut t = kube(id, context, resource, &[free_port()]);
        t.auto_reconnect = true;
        h.start(&t);
        let s = h.until(id, failed(id));
        assert_eq!(s.tunnels[id].failure, Some(kind), "{id}");
        if reconnects {
            h.until("a reconnect", reconnecting(id));
        } else {
            assert_eq!(s.tunnels[id].reconnect, None, "{id}");
        }
        h.send(Request::Remove {
            tunnel_id: id.into(),
        });
    }
}

/// A taken port is refused by the pre-check, as for ssh. One taken after the
/// check is kubectl's own message, retried like a taken port for ssh.
#[test]
fn a_taken_port_is_refused_first_and_kubectls_message_is_retried() {
    let mut h = Harness::new(true);
    let (_held, port) = common::hold_port();
    // The pre-check refuses it first.
    let mut t = kube("busy", "", "service/web", &[port]);
    t.auto_reconnect = true;
    h.start(&t);
    let error = h.error();
    assert!(error.contains(&port.to_string()), "{error}");
    // kubectl's own message, as from a start the pre-check didn't catch.
    let failure = sshelf_core::supervisor::classify_kubectl_lines([
        "Unable to listen on port 18080: Listeners failed to create",
        "error: unable to listen on any of the requested ports: [{18080 80}]",
    ]);
    assert_eq!(failure, FailureKind::PortInUse);
    assert!(failure.reconnects());
    // The fake says the same as kubectl when it can't bind.
    let out = std::process::Command::new(fake())
        .args([
            "--as",
            "kubectl",
            "--namespace",
            "n",
            "port-forward",
            "service/web",
        ])
        .arg(format!("{port}:80"))
        .args(["--address", "127.0.0.1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        sshelf_core::supervisor::classify_kubectl_lines(stderr.lines()),
        FailureKind::PortInUse,
        "{stderr}"
    );
}

#[test]
fn a_dropped_pod_reconnects_as_a_new_process() {
    let mut h = Harness::new(true);
    let mut t = kube("drop", "", "service/drop-web", &[free_port()]);
    t.auto_reconnect = true;
    h.start(&t);
    let first = h
        .until("running", |s| s.state("drop") == TunnelState::Running)
        .tunnels["drop"]
        .pid;
    let s = h.until("the drop", reconnecting("drop"));
    let TunnelState::Failed(reason) = s.state("drop") else {
        panic!("not failed: {:?}", s.state("drop"));
    };
    assert!(reason.contains("lost connection to pod"), "{reason}");
    assert_eq!(s.tunnels["drop"].failure, Some(FailureKind::Other));
    let again = h.until("running again", |s| {
        s.state("drop") == TunnelState::Running && s.tunnels["drop"].pid != first
    });
    assert!(again.tunnels["drop"].pid.is_some());
}

/// kubectl versions that keep running after losing the pod forward nothing:
/// the supervisor ends them and reconnects.
#[test]
fn a_kubectl_that_stays_up_after_losing_its_pod_is_ended() {
    let mut h = Harness::new(true);
    let port = free_port();
    let mut t = kube("stay", "", "deployment/drop-stay-web", &[port]);
    t.auto_reconnect = true;
    h.start(&t);
    h.until("running", |s| s.state("stay") == TunnelState::Running);
    let s = h.until("the drop", reconnecting("stay"));
    assert!(matches!(s.state("stay"), TunnelState::Failed(r) if r.contains("lost connection")));
    assert_eq!(s.tunnels["stay"].pid, None);
    h.send(Request::Remove {
        tunnel_id: "stay".into(),
    });
}

#[test]
fn without_kubectl_it_fails_and_doesnt_retry() {
    let mut h = Harness::new(false);
    let mut t = kube("none", "", "service/web", &[free_port()]);
    t.auto_reconnect = true;
    h.start(&t);
    let s = h.until("failed", failed("none"));
    assert_eq!(
        s.state("none"),
        TunnelState::Failed("kubectl not found".into())
    );
    assert_eq!(s.tunnels["none"].failure, Some(FailureKind::KubectlMissing));
    assert_eq!(s.tunnels["none"].reconnect, None);
}

#[test]
fn a_group_mixes_ssh_and_kubectl() {
    let mut h = Harness::new(true);
    let ssh = Tunnel::ssh(
        "s",
        "ssh one",
        "ok",
        vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: free_port(),
            target: Some("localhost:80".into()),
        }],
    );
    let up = kube("k", "", "service/web", &[free_port()]);
    let gone = kube("g", "", "pod/gone-web", &[free_port()]);
    let group = Group {
        id: "mixed".into(),
        name: "Mixed".into(),
        tunnels: vec!["s".into(), "k".into(), "g".into()],
        start_on_launch: false,
    };
    h.send(Request::StartGroup {
        group: group.clone(),
        tunnels: vec![ssh, up, gone],
    });
    let s = h.until("degraded", |s| {
        s.group_state(&group)
            == GroupState::Degraded {
                running: 2,
                total: 3,
            }
    });
    assert_eq!(s.state("s"), TunnelState::Running);
    assert_eq!(s.state("k"), TunnelState::Running);
    h.send(Request::StopGroup {
        group_id: "mixed".into(),
    });
    h.until("stopped", |s| {
        s.state("s") == TunnelState::Stopped && s.state("k") == TunnelState::Stopped
    });
}
