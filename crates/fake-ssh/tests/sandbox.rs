//! The tour's sandbox Core (SPEC.md §10 *Onboarding tour*, §8 *Tour
//! sandbox*): its ssh and companions are fake-ssh with a prefix argument and
//! a scenario file, terminals don't open, host keys aren't checked, and a key
//! install goes to the fake server and restarts the failed tunnel.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

mod common;

use sshelf_core::core::{Command, Core, Event, SandboxOptions};
use sshelf_core::paths::Paths;
use sshelf_core::ssh::{Tool, ToolSource};
use sshelf_core::storage::Store;
use sshelf_core::supervisor::{Snapshot, TunnelState};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Group, Tunnel, TunnelsFile};

const WAIT: Duration = Duration::from_secs(10);

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

fn state_of(events: &Receiver<Event>, id: &str, want: fn(&TunnelState) -> bool) -> TunnelState {
    wait_for(events, id, |e| match e {
        Event::Tunnels(s) if want(&s.state(id)) => Some(s.state(id)),
        _ => None,
    })
}

fn tunnel(id: &str, host: &str) -> Tunnel {
    Tunnel {
        id: id.into(),
        name: id.into(),
        host: host.into(),
        forwards: vec![Forward {
            kind: ForwardKind::L,
            bind: DEFAULT_BIND.into(),
            listen: common::hold_port().1,
            target: Some("localhost:80".into()),
        }],
        auto_reconnect: false,
    }
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    core: Core,
    events: Receiver<Event>,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let paths = Paths::new(&dir.join("home"), dir.join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::write(
        &paths.ssh_config,
        "Host web-01\n    HostName web-01.example.com\nHost backup-nas\n",
    )
    .unwrap();
    let scenarios = dir.join("scenarios.toml");
    fs::write(
        &scenarios,
        "[hosts.web-01]\nscenario = \"ok\"\n\
         [hosts.backup-nas]\nscenario = \"fail-auth\"\nafter_install = \"ok\"\n",
    )
    .unwrap();
    let mut tunnels = TunnelsFile::default();
    tunnels.upsert_tunnel(tunnel("t-web", "web-01"));
    tunnels.upsert_tunnel(tunnel("t-backup", "backup-nas"));
    tunnels.upsert_group(Group {
        id: "g-1".into(),
        name: "launch".into(),
        tunnels: vec!["t-web".into()],
        start_on_launch: true,
    });
    Store::new(&paths.app_dir).save(&tunnels).unwrap();

    let ssh = Tool {
        // The test binary skips the embedded flag, as `sshelf` needs it.
        program: PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh")),
        prefix: vec!["--fake-ssh".into()],
        env: vec![("FAKE_SSH_SCENARIOS".into(), scenarios.into_os_string())],
    };
    let (core, events) = Core::start_with(paths, Some(SandboxOptions { ssh }), Arc::new(|| {}));
    Sandbox {
        _tmp: tmp,
        dir,
        core,
        events,
    }
}

#[test]
fn the_sandbox_runs_fake_ssh_and_opens_no_terminal() {
    let s = sandbox();
    let info = wait_for(&s.events, "ssh info", |e| match e {
        Event::Ssh(info) => Some(info),
        _ => None,
    });
    let tools = info.tools.as_ref().unwrap();
    assert_eq!(tools.source, ToolSource::Sandbox);
    assert_eq!(
        info.version.as_ref().unwrap().as_deref(),
        Ok(fake_ssh::VERSION_LINE)
    );
    assert!(info.service.is_none(), "the agent's service is left alone");

    s.core.send(Command::Connect("web-01".into()));
    let command = wait_for(&s.events, "would run", |e| match e {
        Event::TourWouldRun { command } => Some(command),
        Event::Launched { .. } => panic!("a terminal was opened"),
        _ => None,
    });
    assert_eq!(command, "ssh web-01");

    s.core.send(Command::AddKeyToAgent {
        path: "~/.ssh/id_ed25519".into(),
        encrypted: false,
    });
    let command = wait_for(&s.events, "would run", |e| match e {
        Event::TourWouldRun { command } => Some(command),
        _ => None,
    });
    assert!(command.starts_with("ssh-add "), "{command}");
}

#[test]
fn tunnels_start_without_host_key_prompts_and_no_group_starts_on_launch() {
    let s = sandbox();
    s.core.send(Command::StartTunnel("t-web".into()));
    let snapshot: Arc<Snapshot> = wait_for(&s.events, "web running", |e| match e {
        Event::HostKeyPrompt(_) => panic!("the sandbox asked about a host key"),
        Event::Tunnels(s) if s.state("t-web") == TunnelState::Running => Some(s),
        _ => None,
    });
    // Only the manual start: the start-on-launch group never ran.
    assert_eq!(
        snapshot.tunnels["t-web"].owners.len(),
        1,
        "{:?}",
        snapshot.tunnels["t-web"].owners
    );
}

#[test]
fn installing_a_key_fixes_the_fake_server_and_restarts_the_tunnel() {
    let s = sandbox();
    s.core.send(Command::StartTunnel("t-backup".into()));
    state_of(&s.events, "t-backup", |st| {
        matches!(st, TunnelState::Failed(_))
    });

    s.core.send(Command::InstallKey {
        alias: "backup-nas".into(),
        public_line: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAtest backup (sshelf)".into(),
    });
    wait_for(&s.events, "the notice", |e| match e {
        Event::Notice(text) if text.contains("backup-nas") => Some(()),
        Event::TourWouldRun { .. } => panic!("the install would have opened a terminal"),
        _ => None,
    });
    state_of(&s.events, "t-backup", |st| *st == TunnelState::Running);
    let keys = fs::read_to_string(
        s.dir
            .join("servers")
            .join("backup-nas")
            .join("authorized_keys"),
    )
    .unwrap();
    assert!(keys.contains("backup (sshelf)"), "{keys}");
}
