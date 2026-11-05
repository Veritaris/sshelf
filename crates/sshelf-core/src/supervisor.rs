//! Tunnel supervisor (SPEC.md §7): runs one ssh process per tunnel on its
//! own thread, independent of the UI loop.
//!
//! - A tunnel runs while at least one owner (a manual start or a group)
//!   holds it; starting it again only adds an owner.
//! - Before spawning, every local listen port is checked; if any is taken,
//!   nothing in that request starts.
//! - A tunnel is *Running* once ssh logs `Entering interactive session.`; an
//!   exit nobody asked for makes it *Failed* with ssh's last message.
//! - Shutting down stops every tunnel (no detached processes).
//! - With a record (`start_recorded`), what runs is kept in `running.toml`;
//!   after a crash the next start takes those processes over (Unix; SPEC.md
//!   §7 *Surviving a crash*).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::child::{self, KILL_GRACE, TreeKiller};
use crate::running::{self, RunningFile, RunningTunnel};
use crate::ssh::Tool;
use crate::storage::Store;
use crate::tunnels::{Group, Tunnel};

/// Stderr lines kept per tunnel.
pub const LOG_LINES: usize = 200;
/// ssh's `-v` line once authentication and forward setup are done.
pub const READY_MARKER: &str = "Entering interactive session.";
/// How long an exit waits for ssh's remaining output to be read.
const READ_REST: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    Manual,
    /// Group id.
    Group(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Stopped,
    Starting,
    Running,
    /// Being stopped; the process has not exited yet.
    Stopping,
    /// Exited without being asked to; the reason is ssh's last message.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub text: String,
    /// `debug1:`-style output from `-v`; hidden unless asked for.
    pub debug: bool,
    /// When the line came (the log shows `HH:MM:SS`).
    pub at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelStatus {
    pub state: TunnelState,
    pub owners: BTreeSet<Owner>,
    pub log: VecDeque<LogLine>,
    pub pid: Option<u32>,
    /// Why the last run failed, while the state is `Failed`.
    pub failure: Option<FailureKind>,
    /// A pending automatic restart (`auto_reconnect`, SPEC.md §7); the state
    /// stays `Failed` until it happens.
    pub reconnect: Option<Reconnect>,
    /// When the current run reached `Running` (its uptime); `None` while it
    /// doesn't run.
    pub running_since: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    /// 1 for the first retry after a drop.
    pub attempt: u32,
    pub at: SystemTime,
}

/// What a failed run ran into, from ssh's messages (SPEC.md §8, §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The server's key differs from the stored one.
    HostKeyChanged,
    /// No stored key (e.g. for a jump host).
    HostKeyUnknown,
    /// No offered key was accepted (BatchMode: agent or passphrase-less keys only).
    Auth,
    Other,
}

/// Classifies the last run in `log` (the lines after the last start).
pub fn classify_failure(log: &VecDeque<LogLine>) -> FailureKind {
    let start = log
        .iter()
        .rposition(|l| l.text.starts_with("starting ssh"))
        .map_or(0, |i| i + 1);
    classify_lines(log.iter().skip(start).map(|l| l.text.as_str()))
}

/// What ssh's messages say went wrong (also used by health checks).
pub fn classify_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> FailureKind {
    let run: Vec<&str> = lines.into_iter().collect();
    let any = |needles: &[&str]| run.iter().any(|l| needles.iter().any(|n| l.contains(n)));
    if any(&[
        "REMOTE HOST IDENTIFICATION HAS CHANGED",
        "has changed and you have requested strict checking",
    ]) {
        FailureKind::HostKeyChanged
    } else if any(&["Host key verification failed"]) {
        FailureKind::HostKeyUnknown
    } else if any(&[
        "Permission denied (publickey",
        "Too many authentication failures",
        "No more authentication methods",
    ]) {
        FailureKind::Auth
    } else {
        FailureKind::Other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupState {
    Stopped,
    Starting,
    Running,
    /// Some tunnels are not running (e.g. failed): `running` of `total`.
    Degraded {
        running: usize,
        total: usize,
    },
}

/// Status of every tunnel the supervisor has seen, by tunnel id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub tunnels: BTreeMap<String, TunnelStatus>,
}

impl Snapshot {
    pub fn state(&self, tunnel_id: &str) -> TunnelState {
        self.tunnels
            .get(tunnel_id)
            .map_or(TunnelState::Stopped, |s| s.state.clone())
    }

    pub fn group_state(&self, group: &Group) -> GroupState {
        let owner = Owner::Group(group.id.clone());
        let members: Vec<&TunnelStatus> = group
            .tunnels
            .iter()
            .filter_map(|id| self.tunnels.get(id))
            .filter(|s| s.owners.contains(&owner))
            .collect();
        if members.is_empty() {
            return GroupState::Stopped;
        }
        let total = group.tunnels.len();
        let running = members
            .iter()
            .filter(|s| s.state == TunnelState::Running)
            .count();
        let failed = members
            .iter()
            .any(|s| matches!(s.state, TunnelState::Failed(_)));
        if running == total {
            GroupState::Running
        } else if !failed && members.iter().any(|s| s.state == TunnelState::Starting) {
            GroupState::Starting
        } else {
            GroupState::Degraded { running, total }
        }
    }
}

#[derive(Debug, Clone)]
pub enum Request {
    /// Adds `owner`; spawns unless already running or starting.
    Start {
        tunnel: Tunnel,
        owner: Owner,
    },
    /// Removes `owner`; stops the tunnel when no owners remain.
    Stop {
        tunnel_id: String,
        owner: Owner,
    },
    /// Owns every tunnel of the group; (re)starts the stopped and failed ones
    /// together, or none if a port is taken. Also "Retry failed".
    StartGroup {
        group: Group,
        tunnels: Vec<Tunnel>,
    },
    StopGroup {
        group_id: String,
    },
    /// Stops the tunnel whatever its owners (e.g. before deleting it).
    Remove {
        tunnel_id: String,
    },
    /// ssh for processes started from now on.
    SetSsh(Option<Tool>),
    /// Takes over what `running.toml` lists from before a crash, as far as
    /// it is still the same process running the same `tunnels`' ssh (SPEC.md
    /// §7 *Surviving a crash*); the rest is dropped from the file, never
    /// killed. Send once, before anything starts. Until then the file is
    /// left as it is.
    Adopt {
        tunnels: Vec<Tunnel>,
    },
}

#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    Snapshot(Arc<Snapshot>),
    /// A request that could not be carried out (e.g. a port conflict).
    Error(String),
    /// `Adopt` took over this many tunnels.
    Adopted(usize),
}

enum Message {
    Request(Request),
    Line {
        id: String,
        generation: u64,
        text: String,
    },
    Exited {
        id: String,
        generation: u64,
        code: Option<i32>,
    },
    Shutdown,
}

/// Handle to the supervisor thread; dropping it stops every tunnel.
pub struct Supervisor {
    tx: Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// A supervisor that keeps no record of its tunnels (tests, the tour).
    pub fn start(ssh: Option<Tool>, on_event: impl Fn(SupervisorEvent) + Send + 'static) -> Self {
        Self::launch(ssh, None, on_event)
    }

    /// A supervisor that records its running tunnels in `running.toml` in
    /// `app_dir`, so a later start can take them over (`Request::Adopt`).
    pub fn start_recorded(
        ssh: Option<Tool>,
        app_dir: PathBuf,
        on_event: impl Fn(SupervisorEvent) + Send + 'static,
    ) -> Self {
        Self::launch(ssh, Some(Store::new(app_dir)), on_event)
    }

    fn launch(
        ssh: Option<Tool>,
        record: Option<Store>,
        on_event: impl Fn(SupervisorEvent) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker_tx = tx.clone();
        let thread = thread::Builder::new()
            .name("sshelf-supervisor".into())
            .spawn(move || {
                Worker {
                    ssh,
                    tx: worker_tx,
                    on_event: Box::new(on_event),
                    entries: BTreeMap::new(),
                    next_generation: 0,
                    changed: false,
                    record,
                    recorded: None,
                }
                .run(rx)
            })
            .expect("spawning the supervisor thread");
        Self {
            tx,
            thread: Some(thread),
        }
    }

    pub fn send(&self, request: Request) {
        let _ = self.tx.send(Message::Request(request));
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Local ports of `tunnels` that are taken (or wanted twice), as messages.
pub fn check_ports(tunnels: &[&Tunnel]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut wanted: Vec<(String, u16, &str)> = Vec::new();
    for tunnel in tunnels {
        for (addr, port) in tunnel.forwards.iter().filter_map(|f| f.local_listen()) {
            if let Some((_, _, other)) = wanted.iter().find(|(a, p, _)| *a == addr && *p == port) {
                problems.push(format!(
                    "port {port} is used by both \"{other}\" and \"{}\"",
                    tunnel.name
                ));
                continue;
            }
            if TcpListener::bind((addr.as_str(), port)).is_err() {
                problems.push(format!(
                    "port {port} on {addr} is already in use (needed by \"{}\")",
                    tunnel.name
                ));
            }
            wanted.push((addr, port, &tunnel.name));
        }
    }
    problems
}

struct Process {
    generation: u64,
    killer: TreeKiller,
    /// `child::start_time` of its pid (`running.toml`); `None` where the OS
    /// isn't covered.
    started: Option<u64>,
}

struct Entry {
    tunnel: Tunnel,
    status: TunnelStatus,
    process: Option<Process>,
    /// Start again once the current (stopping) process has exited.
    restart: bool,
    /// Automatic restarts since the tunnel last ran stably.
    attempt: u32,
    /// When the pending automatic restart is due.
    due: Option<Instant>,
}

/// First wait before an automatic restart; it doubles up to the maximum.
pub const RECONNECT_FIRST: Duration = Duration::from_secs(2);
pub const RECONNECT_MAX: Duration = Duration::from_secs(60);
/// A run this long counts as stable: the next drop starts at the first wait.
pub const RECONNECT_STABLE: Duration = Duration::from_secs(60);

/// The wait before automatic restart `attempt` (1-based): 2, 4, 8 … 60 s.
pub fn reconnect_delay(attempt: u32) -> Duration {
    let doubled = RECONNECT_FIRST.saturating_mul(1 << attempt.saturating_sub(1).min(16));
    doubled.min(RECONNECT_MAX)
}

struct Worker {
    ssh: Option<Tool>,
    tx: Sender<Message>,
    on_event: Box<dyn Fn(SupervisorEvent) + Send>,
    entries: BTreeMap<String, Entry>,
    next_generation: u64,
    changed: bool,
    /// Where `running.toml` goes, if it is kept.
    record: Option<Store>,
    /// What it holds now; `None` until `Adopt` has read it (it is left alone
    /// until then).
    recorded: Option<RunningFile>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        loop {
            // Sleep until a message arrives or the next restart is due.
            let message = match self.entries.values().filter_map(|e| e.due).min() {
                Some(due) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(message) => Some(message),
                    Err(_) => return,
                },
            };
            if let Some(message) = message {
                if !self.handle(message) {
                    return self.shutdown(&rx);
                }
                // Coalesce bursts (e.g. many log lines) into one snapshot.
                while let Ok(message) = rx.try_recv() {
                    if !self.handle(message) {
                        return self.shutdown(&rx);
                    }
                }
            }
            self.reconnect_due();
            self.publish();
            self.write_record();
        }
    }

    /// Writes `running.toml` when what runs changed (and once `Adopt` has
    /// read it): the tunnels starting or running, with their processes.
    fn write_record(&mut self) {
        if self.record.is_none() || self.recorded.is_none() {
            return;
        }
        let file = RunningFile {
            tunnels: self
                .entries
                .iter()
                .filter(|(_, e)| {
                    matches!(e.status.state, TunnelState::Starting | TunnelState::Running)
                })
                .filter_map(|(id, e)| {
                    let process = e.process.as_ref()?;
                    Some(RunningTunnel {
                        id: id.clone(),
                        pid: e.status.pid?,
                        started: process.started.unwrap_or(0),
                        args: e.tunnel.ssh_args(),
                        owners: e.status.owners.iter().map(running::owner_key).collect(),
                    })
                })
                .collect(),
        };
        if self.recorded.as_ref() == Some(&file) {
            return;
        }
        let saved = self.record.as_ref().map(|store| store.save(&file));
        // Remembered either way: a file that can't be written is reported
        // once, not on every change.
        self.recorded = Some(file);
        if let Some(Err(e)) = saved {
            self.error(format!("Running tunnels can't be recorded: {e}"));
        }
    }

    /// `Request::Adopt`: reads `running.toml` and takes over what still runs
    /// as recorded; the file is rewritten with what was taken over.
    fn adopt(&mut self, tunnels: Vec<Tunnel>) {
        let Some(store) = &mut self.record else {
            return;
        };
        if self.recorded.is_some() {
            return;
        }
        let file = store.load::<RunningFile>().data;
        let mut adopted = 0;
        for recorded in &file.tunnels {
            if self.adopt_one(recorded, &tunnels) {
                adopted += 1;
            }
        }
        // Forces the rewrite: what wasn't taken over goes.
        self.recorded = Some(RunningFile {
            tunnels: file.tunnels,
        });
        self.changed = true;
        if adopted > 0 {
            (self.on_event)(SupervisorEvent::Adopted(adopted));
        }
    }

    /// Takes over `recorded` if it is still the same process (same start
    /// time) running what its tunnel runs now (same arguments).
    fn adopt_one(&mut self, recorded: &RunningTunnel, tunnels: &[Tunnel]) -> bool {
        let Some(tunnel) = tunnels.iter().find(|t| t.id == recorded.id) else {
            return false;
        };
        let same = !self.entries.contains_key(&recorded.id)
            && tunnel.ssh_args() == recorded.args
            && child::start_time(recorded.pid) == Some(recorded.started);
        if !same {
            return false;
        }
        #[cfg(unix)]
        {
            self.take_over(tunnel.clone(), recorded);
            true
        }
        // Windows: tunnels die with sshelf; `start_time` is never known.
        #[cfg(not(unix))]
        false
    }

    /// Runs `tunnel` as the process `recorded` names: Running, with its
    /// recorded owners; a thread reports its exit.
    #[cfg(unix)]
    fn take_over(&mut self, tunnel: Tunnel, recorded: &RunningTunnel) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let pid = recorded.pid;
        let id = tunnel.id.clone();
        let mut owners: BTreeSet<Owner> = recorded
            .owners
            .iter()
            .filter_map(|key| running::owner_from_key(key))
            .collect();
        if owners.is_empty() {
            owners.insert(Owner::Manual);
        }
        let tx = self.tx.clone();
        let waiter_id = id.clone();
        thread::spawn(move || {
            child::wait_exit(pid);
            // Its own last words went to the sshelf that crashed.
            let _ = tx.send(Message::Line {
                id: waiter_id.clone(),
                generation,
                text: "ssh exited".to_string(),
            });
            let _ = tx.send(Message::Exited {
                id: waiter_id,
                generation,
                code: None,
            });
        });
        let mut log = VecDeque::new();
        push_log(
            &mut log,
            format!(
                "Taken over after sshelf restarted (pid {pid}); earlier output isn't available"
            ),
        );
        self.entries.insert(
            id,
            Entry {
                tunnel,
                status: TunnelStatus {
                    state: TunnelState::Running,
                    owners,
                    log,
                    pid: Some(pid),
                    failure: None,
                    reconnect: None,
                    running_since: Some(Instant::now()),
                },
                process: Some(Process {
                    generation,
                    killer: TreeKiller::adopt(pid),
                    started: Some(recorded.started),
                }),
                restart: false,
                attempt: 0,
                due: None,
            },
        );
        self.changed = true;
    }

    /// Restarts tunnels whose reconnect wait is over and that are still
    /// wanted; a taken port waits for the next attempt.
    fn reconnect_due(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.due.is_some_and(|d| d <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            let entry = self.entries.get_mut(&id).expect("a due entry");
            entry.due = None;
            entry.status.reconnect = None;
            self.changed = true;
            let wanted = !entry.status.owners.is_empty()
                && matches!(entry.status.state, TunnelState::Failed(_));
            if !wanted {
                continue;
            }
            let problems = check_ports(&[&entry.tunnel]);
            if problems.is_empty() {
                self.spawn(&id);
            } else {
                let entry = self.entries.get_mut(&id).expect("a due entry");
                push_log(&mut entry.status.log, problems.join("; "));
                self.schedule_reconnect(&id);
            }
        }
    }

    /// Plans the next automatic restart of `id`, with backoff.
    fn schedule_reconnect(&mut self, id: &str) {
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        entry.attempt += 1;
        let delay = reconnect_delay(entry.attempt);
        entry.due = Some(Instant::now() + delay);
        entry.status.reconnect = Some(Reconnect {
            attempt: entry.attempt,
            at: SystemTime::now() + delay,
        });
        push_log(
            &mut entry.status.log,
            format!(
                "reconnecting in {} s (attempt {})",
                delay.as_secs(),
                entry.attempt
            ),
        );
        self.changed = true;
    }

    /// `false` on shutdown.
    fn handle(&mut self, message: Message) -> bool {
        match message {
            Message::Request(request) => self.request(request),
            Message::Line {
                id,
                generation,
                text,
            } => self.line(&id, generation, text),
            Message::Exited {
                id,
                generation,
                code,
            } => self.exited(&id, generation, code),
            Message::Shutdown => return false,
        }
        true
    }

    fn publish(&mut self) {
        if !self.changed {
            return;
        }
        self.changed = false;
        let snapshot = Snapshot {
            tunnels: self
                .entries
                .iter()
                .map(|(id, e)| (id.clone(), e.status.clone()))
                .collect(),
        };
        (self.on_event)(SupervisorEvent::Snapshot(Arc::new(snapshot)));
    }

    fn error(&self, message: String) {
        (self.on_event)(SupervisorEvent::Error(message));
    }

    fn request(&mut self, request: Request) {
        match request {
            Request::Start { tunnel, owner } => self.start(vec![tunnel], owner),
            Request::Stop { tunnel_id, owner } => self.release(&tunnel_id, &owner),
            Request::StartGroup { group, tunnels } => {
                let wanted: Vec<Tunnel> = tunnels
                    .into_iter()
                    .filter(|t| group.tunnels.contains(&t.id))
                    .collect();
                self.start(wanted, Owner::Group(group.id))
            }
            Request::StopGroup { group_id } => {
                let owner = Owner::Group(group_id);
                let ids: Vec<String> = self
                    .entries
                    .iter()
                    .filter(|(_, e)| e.status.owners.contains(&owner))
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    self.release(&id, &owner);
                }
            }
            Request::Remove { tunnel_id } => {
                if let Some(entry) = self.entries.get_mut(&tunnel_id) {
                    entry.status.owners.clear();
                    entry.restart = false;
                    if let Some(process) = entry.process.take() {
                        process.killer.kill();
                    }
                    self.entries.remove(&tunnel_id);
                    self.changed = true;
                }
            }
            Request::SetSsh(ssh) => self.ssh = ssh,
            Request::Adopt { tunnels } => self.adopt(tunnels),
        }
    }

    /// Adds `owner` to every tunnel; spawns those not running, or none of
    /// them when a port is taken.
    fn start(&mut self, tunnels: Vec<Tunnel>, owner: Owner) {
        let to_spawn: Vec<&Tunnel> = tunnels
            .iter()
            .filter(|t| {
                self.entries.get(&t.id).is_none_or(|e| {
                    matches!(
                        e.status.state,
                        TunnelState::Stopped | TunnelState::Failed(_)
                    )
                })
            })
            .collect();
        let problems = check_ports(&to_spawn);
        if !problems.is_empty() {
            return self.error(format!("Nothing started: {}", problems.join("; ")));
        }
        let to_spawn: Vec<String> = to_spawn.iter().map(|t| t.id.clone()).collect();

        for tunnel in tunnels {
            let id = tunnel.id.clone();
            let entry = self.entries.entry(id.clone()).or_insert_with(|| Entry {
                tunnel: tunnel.clone(),
                status: TunnelStatus {
                    state: TunnelState::Stopped,
                    owners: BTreeSet::new(),
                    log: VecDeque::new(),
                    pid: None,
                    failure: None,
                    reconnect: None,
                    running_since: None,
                },
                process: None,
                restart: false,
                attempt: 0,
                due: None,
            });
            entry.status.owners.insert(owner.clone());
            self.changed = true;
            if entry.status.state == TunnelState::Stopping {
                entry.tunnel = tunnel;
                entry.restart = true;
            } else if to_spawn.contains(&id) {
                // Started by hand: a pending automatic restart is moot.
                entry.tunnel = tunnel;
                entry.due = None;
                entry.attempt = 0;
                self.spawn(&id);
            }
        }
    }

    fn release(&mut self, id: &str, owner: &Owner) {
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        if !entry.status.owners.remove(owner) {
            return;
        }
        self.changed = true;
        if !entry.status.owners.is_empty() {
            return;
        }
        entry.restart = false;
        entry.due = None;
        entry.attempt = 0;
        entry.status.reconnect = None;
        match &entry.process {
            Some(process) => {
                process.killer.kill();
                entry.status.state = TunnelState::Stopping;
            }
            None => entry.status.state = TunnelState::Stopped,
        }
    }

    fn spawn(&mut self, id: &str) {
        let Some(ssh) = self.ssh.clone() else {
            return self.fail(id, "no ssh binary".to_string());
        };
        let entry = self.entries.get_mut(id).expect("spawn of a known tunnel");
        let mut cmd = ssh.command();
        cmd.args(entry.tunnel.ssh_args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let spawned = match child::spawn_tree(cmd) {
            Ok(spawned) => spawned,
            Err(e) => return self.fail(id, format!("cannot start {}: {e}", ssh.display())),
        };
        self.next_generation += 1;
        let generation = self.next_generation;
        let mut child = spawned.child;
        let pid = child.id();

        // The reader says when it has read everything, so the exit is
        // reported after ssh's last lines (they decide the failure kind).
        let (read_all, all_read) = mpsc::channel::<()>();
        if let Some(stderr) = child.stderr.take() {
            let tx = self.tx.clone();
            let id = id.to_string();
            thread::spawn(move || {
                // Dropped when this thread ends, which wakes the waiter.
                let _read_all = read_all;
                for line in BufReader::new(stderr).lines() {
                    let Ok(text) = line else { break };
                    let message = Message::Line {
                        id: id.clone(),
                        generation,
                        text,
                    };
                    if tx.send(message).is_err() {
                        break;
                    }
                }
            });
        }
        let tx = self.tx.clone();
        let waiter_id = id.to_string();
        thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            // Its last lines first. Bounded: a grandchild (a ProxyCommand)
            // can keep the pipe open after ssh itself has exited.
            let _ = all_read.recv_timeout(READ_REST);
            let _ = tx.send(Message::Exited {
                id: waiter_id,
                generation,
                code,
            });
        });

        let entry = self.entries.get_mut(id).expect("spawn of a known tunnel");
        entry.process = Some(Process {
            generation,
            killer: spawned.killer,
            started: child::start_time(pid),
        });
        entry.status.state = TunnelState::Starting;
        entry.status.failure = None;
        entry.status.reconnect = None;
        entry.status.pid = Some(pid);
        push_log(&mut entry.status.log, format!("starting ssh (pid {pid})"));
        self.changed = true;
    }

    fn fail(&mut self, id: &str, reason: String) {
        if let Some(entry) = self.entries.get_mut(id) {
            push_log(&mut entry.status.log, reason.clone());
            entry.status.state = TunnelState::Failed(reason);
            entry.status.failure = Some(FailureKind::Other);
            entry.status.pid = None;
            self.changed = true;
        }
    }

    fn current(&mut self, id: &str, generation: u64) -> Option<&mut Entry> {
        self.entries.get_mut(id).filter(|e| {
            e.process
                .as_ref()
                .is_some_and(|p| p.generation == generation)
        })
    }

    fn line(&mut self, id: &str, generation: u64, text: String) {
        let Some(entry) = self.current(id, generation) else {
            return;
        };
        if entry.status.state == TunnelState::Starting && text.contains(READY_MARKER) {
            entry.status.state = TunnelState::Running;
            entry.status.running_since = Some(Instant::now());
        }
        push_log(&mut entry.status.log, text);
        self.changed = true;
    }

    fn exited(&mut self, id: &str, generation: u64, code: Option<i32>) {
        let Some(entry) = self.current(id, generation) else {
            return;
        };
        entry.process = None;
        entry.status.pid = None;
        // Not running any more: no uptime (the backoff below still reads it).
        let running_since = entry.status.running_since.take();
        if entry.status.state == TunnelState::Stopping {
            entry.status.state = TunnelState::Stopped;
            push_log(&mut entry.status.log, "stopped".to_string());
            let restart = entry.restart && !entry.status.owners.is_empty();
            entry.restart = false;
            let tunnel = entry.tunnel.clone();
            self.changed = true;
            if restart {
                let problems = check_ports(&[&tunnel]);
                if problems.is_empty() {
                    self.spawn(id);
                } else {
                    self.fail(id, problems.join("; "));
                }
            }
            return;
        }
        let reason = entry
            .status
            .log
            .iter()
            .rev()
            .find(|l| !l.debug && !l.text.starts_with("starting ssh"))
            .map(|l| l.text.clone())
            .unwrap_or_else(|| format!("ssh exited with code {code:?}"));
        let failure = classify_failure(&entry.status.log);
        entry.status.state = TunnelState::Failed(reason);
        entry.status.failure = Some(failure);
        // A drop after a stable run starts the backoff over.
        if running_since.is_some_and(|since| since.elapsed() >= RECONNECT_STABLE) {
            entry.attempt = 0;
        }
        // Auth and host key failures need the user; retrying cannot help.
        let retry = entry.tunnel.auto_reconnect
            && !entry.status.owners.is_empty()
            && failure == FailureKind::Other;
        self.changed = true;
        if retry {
            self.schedule_reconnect(id);
        }
    }

    /// Stops every tunnel and waits (bounded) for the processes to exit.
    fn shutdown(&mut self, rx: &Receiver<Message>) {
        let mut waiting: BTreeMap<String, u64> = BTreeMap::new();
        for (id, entry) in &mut self.entries {
            if let Some(process) = entry.process.take() {
                process.killer.kill();
                waiting.insert(id.clone(), process.generation);
                // Dropping the killer finishes the job (Windows: closes the job).
                drop(process);
            }
        }
        let deadline = Instant::now() + KILL_GRACE + Duration::from_secs(1);
        while !waiting.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Message::Exited { id, generation, .. }) => {
                    if waiting.get(&id) == Some(&generation) {
                        waiting.remove(&id);
                    }
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        // A clean quit: nothing left to take over.
        self.write_record();
    }
}

fn push_log(log: &mut VecDeque<LogLine>, text: String) {
    let debug = text.starts_with("debug");
    log.push_back(LogLine {
        text,
        debug,
        at: SystemTime::now(),
    });
    while log.len() > LOG_LINES {
        log.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnels::{DEFAULT_BIND, Forward, ForwardKind};

    fn tunnel(id: &str, port: u16) -> Tunnel {
        Tunnel {
            id: id.into(),
            name: format!("tunnel {id}"),
            host: "h".into(),
            forwards: vec![Forward {
                kind: ForwardKind::L,
                bind: DEFAULT_BIND.into(),
                listen: port,
                target: Some("localhost:1".into()),
            }],
            auto_reconnect: false,
        }
    }

    fn status(state: TunnelState, owners: &[&Owner]) -> TunnelStatus {
        TunnelStatus {
            state,
            owners: owners.iter().map(|o| (*o).clone()).collect(),
            log: VecDeque::new(),
            pid: None,
            failure: None,
            reconnect: None,
            running_since: None,
        }
    }

    fn log(lines: &[&str]) -> VecDeque<LogLine> {
        let mut log = VecDeque::new();
        for line in lines {
            push_log(&mut log, line.to_string());
        }
        log
    }

    #[test]
    fn reconnect_waits_double_up_to_a_minute() {
        let secs: Vec<u64> = (1..=8).map(|a| reconnect_delay(a).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(reconnect_delay(u32::MAX), RECONNECT_MAX);
    }

    #[test]
    fn failures_are_classified_from_the_last_run() {
        let changed = log(&[
            "starting ssh (pid 1)",
            "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @",
            "Host key verification failed.",
        ]);
        assert_eq!(classify_failure(&changed), FailureKind::HostKeyChanged);
        let unknown = log(&["starting ssh (pid 1)", "Host key verification failed."]);
        assert_eq!(classify_failure(&unknown), FailureKind::HostKeyUnknown);
        let auth = log(&[
            "starting ssh (pid 1)",
            "web: Permission denied (publickey).",
        ]);
        assert_eq!(classify_failure(&auth), FailureKind::Auth);
        // An earlier run's messages do not count.
        let retried = log(&[
            "starting ssh (pid 1)",
            "web: Permission denied (publickey).",
            "starting ssh (pid 2)",
            "Error: remote port forwarding failed for listen port 6443",
        ]);
        assert_eq!(classify_failure(&retried), FailureKind::Other);
    }

    #[test]
    fn port_check_finds_taken_and_duplicate_ports() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let taken = held.local_addr().unwrap().port();
        // Below every OS's ephemeral range, so nothing else on the machine
        // is handed it between this check and `check_ports`.
        let free = (20_000..32_000)
            .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        let (a, b, c) = (tunnel("a", taken), tunnel("b", free), tunnel("c", free));
        let problems = check_ports(&[&a, &b, &c]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("already in use"), "{problems:?}");
        assert!(problems[1].contains("used by both"), "{problems:?}");
        assert!(check_ports(&[&b]).is_empty());
    }

    #[test]
    fn group_states() {
        let g = Owner::Group("g".into());
        let group = Group {
            id: "g".into(),
            name: "G".into(),
            tunnels: vec!["a".into(), "b".into()],
            start_on_launch: false,
        };
        let snapshot = |a: TunnelStatus, b: TunnelStatus| Snapshot {
            tunnels: [("a".into(), a), ("b".into(), b)].into(),
        };
        let other = Owner::Group("other".into());
        assert_eq!(
            snapshot(
                status(TunnelState::Running, &[&other]),
                status(TunnelState::Stopped, &[])
            )
            .group_state(&group),
            GroupState::Stopped
        );
        assert_eq!(
            snapshot(
                status(TunnelState::Running, &[&g]),
                status(TunnelState::Running, &[&g, &other])
            )
            .group_state(&group),
            GroupState::Running
        );
        assert_eq!(
            snapshot(
                status(TunnelState::Running, &[&g]),
                status(TunnelState::Starting, &[&g])
            )
            .group_state(&group),
            GroupState::Starting
        );
        assert_eq!(
            snapshot(
                status(TunnelState::Running, &[&g]),
                status(TunnelState::Failed("x".into()), &[&g])
            )
            .group_state(&group),
            GroupState::Degraded {
                running: 1,
                total: 2
            }
        );
    }

    #[test]
    fn log_is_capped_and_marks_debug_lines() {
        let mut log = VecDeque::new();
        for i in 0..(LOG_LINES + 10) {
            push_log(&mut log, format!("line {i}"));
        }
        push_log(&mut log, "debug1: x".into());
        assert_eq!(log.len(), LOG_LINES);
        assert_eq!(log.front().unwrap().text, "line 11");
        assert!(log.back().unwrap().debug);
    }
}
