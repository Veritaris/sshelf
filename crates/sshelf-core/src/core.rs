//! The core service (SPEC.md §2): owns every file and process. The UI sends
//! [`Command`]s and renders [`Event`]s; after each event the repaint callback
//! runs, so the UI never polls. The ssh config is written only by validated
//! edits, through [`ConfigWriter`]. Tunnels run in the [`Supervisor`], whose
//! events are relayed as [`Event::Tunnels`].

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};
use sshconf::{ConfigSet, HostList, WatchDir};

use crate::agent_service::{self, AgentService};
use crate::config_edit::{self, Change, ConfigWriter, Edit, Problem};
use crate::health::{self, Health, HealthReport, HealthSnapshot};
use crate::history::History;
use crate::hostkeys::{self, ChangedKey, Keygen, KnownHostsWriter, UnknownKey};
use crate::keygen::{self, FoundKey, GeneratedKey, KeyRequest};
use crate::kubectl::{self, KubectlInfo};
use crate::metadata::{HostMeta, HostsFile};
use crate::orphans::{self, GroupOrphan, Orphan, TunnelOrphan};
use crate::paths::Paths;
use crate::process;
use crate::redact::{self, Redactor};
use crate::rename;
use crate::settings::Settings;
use crate::ssh::{self, AgentStatus, Resolved, SshTools, Tool};
use crate::storage::{LoadStatus, Store, Versioned};
use crate::supervisor::{self, Owner, Request, Snapshot, Supervisor, SupervisorEvent, TunnelState};
use crate::terminal::{self, Terminal};
use crate::tray_host;
use crate::tunnels::{Group, Tunnel, TunnelsFile};
use crate::watch::ConfigWatcher;

/// Called after every event; the UI passes `ctx.request_repaint()`.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

/// The app window a system dialog belongs to: the dialog is modal to it
/// (Windows disables the window, macOS shows a sheet, the Linux portal
/// gets the parent).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParentWindow {
    pub window: RawWindowHandle,
    pub display: RawDisplayHandle,
}

// SAFETY: these are plain window ids, used only while the app (and its one
// window) lives, which outlasts every dialog it opens; the platform dialog
// APIs take them from any thread.
unsafe impl Send for ParentWindow {}
unsafe impl Sync for ParentWindow {}

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: see above; the window outlives the dialog.
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for ParentWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: see above.
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Re-read the ssh config (also sent by the watcher).
    Reload,
    /// Run `ssh -G` for an alias.
    Resolve(String),
    /// Replace the metadata of the host owning `alias`.
    SetMetadata {
        alias: String,
        meta: HostMeta,
    },
    RelinkOrphan {
        key: String,
        to: String,
    },
    DeleteOrphan {
        key: String,
    },
    SaveSettings(Settings),
    /// Streamer mode's two switches (SPEC.md §10 *Streamer mode*): applied
    /// at once to the saved settings, outside any Settings draft.
    SetStreamer {
        on: bool,
        hide_aliases: bool,
    },
    /// Re-detect the ssh binary, version, agent and service.
    RefreshSsh,
    /// Re-detect kubectl, its version and the kubeconfig (M13).
    RefreshKubectl,
    /// Open a terminal session for an alias.
    Connect(String),
    /// Create or update a tunnel (refused while it runs).
    SaveTunnel(Tunnel),
    /// Stop and delete a tunnel, removing it from every group.
    DeleteTunnel(String),
    /// Start a tunnel manually.
    StartTunnel(String),
    /// Release the manual start; groups using it keep it running.
    StopTunnel(String),
    /// Create or update a group (refused while it runs).
    SaveGroup(Group),
    DeleteGroup(String),
    /// Start a group; also "Retry failed".
    StartGroup(String),
    StopGroup(String),
    RelinkTunnel {
        tunnel_id: String,
        to: String,
    },
    RelinkGroupMember {
        group_id: String,
        from: String,
        to: String,
    },
    RemoveGroupMember {
        group_id: String,
        tunnel_id: String,
    },
    /// Store the key of a [`HostKeyPrompt`] and continue the start.
    AcceptHostKey(u64),
    /// Do not store the key; the start is abandoned.
    RejectHostKey(u64),
    /// Find whose host key changed for a failed tunnel (SPEC.md §9).
    ReviewChangedKey(String),
    /// Review a host's keys (the health banner): an unknown key asks like a
    /// start, a changed one like [`Command::ReviewChangedKey`]; a notice
    /// when everything matches.
    ReviewHostKey(String),
    /// Replace the old key of a [`ChangedKeyPrompt`] and start the tunnel
    /// again (for a host review: only replace it).
    ReplaceHostKey(u64),
    CancelChangedKey(u64),
    /// Validate an edit without saving it; answered by [`Event::EditChecked`].
    CheckEdit(Edit),
    /// Validate and write an edit. Refused with [`Event::EditConflict`] if the
    /// file changed since the edit started, unless `overwrite` is set.
    SaveEdit {
        edit: Edit,
        overwrite: bool,
    },
    /// Rename `alias` on its host's `Host` line and in all app data, as one
    /// operation; answered like [`Command::SaveEdit`] under `id`.
    RenameHost {
        id: u64,
        alias: String,
        to: String,
    },
    /// Generate a key pair (SPEC.md §11); answered by [`Event::KeyGenerated`].
    GenerateKey {
        id: u64,
        request: KeyRequest,
    },
    /// Open a terminal that adds a public key to `alias`'s
    /// `authorized_keys`; the first login there usually asks for a password.
    InstallKey {
        alias: String,
        public_line: String,
    },
    /// Load a key into the agent: `ssh-add` directly, or in a terminal when
    /// the key is `encrypted` (the passphrase is typed into ssh-add itself).
    AddKeyToAgent {
        path: PathBuf,
        encrypted: bool,
    },
    /// Check these hosts' health (SPEC.md §12, M8); a few at a time.
    CheckHealth(Vec<String>),
    /// Drop the health checks still waiting; the running ones finish.
    CancelHealth,
    /// Open a terminal running `ssh-keygen` for a security key (SPEC.md §11).
    GenerateHardwareKey(KeyRequest),
    /// Read the public half of a key `ssh-keygen` wrote; answered by
    /// [`Event::KeyGenerated`] under `id`.
    LoadPublicKey {
        id: u64,
        path: PathBuf,
    },
    /// Work out every jump host on the way to an alias (`ssh -G` per hop);
    /// answered by [`Event::Route`].
    ResolveRoute(String),
    /// List the key pairs in `~/.ssh` (their `.pub` files only); answered by
    /// [`Event::Keys`].
    FindKeys,
    /// Open the system file dialog to choose a key file, modal to `parent`;
    /// answered by [`Event::KeyFilePicked`] under `id`.
    PickKeyFile {
        id: u64,
        parent: Option<ParentWindow>,
    },
    Shutdown,
}

/// One jump host on a route (SPEC.md §12, M8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHop {
    /// As written in `ProxyJump`: an alias or `[user@]host[:port]`.
    pub destination: String,
    /// What ssh connects to.
    pub hostname: String,
    pub port: u16,
}

/// An unknown host key waiting for the user (SPEC.md §9).
#[derive(Debug, Clone)]
pub struct HostKeyPrompt {
    pub id: u64,
    /// What is waiting for it, e.g. `tunnel "Prod DB"`.
    pub purpose: String,
    pub unknown: UnknownKey,
}

/// A changed host key waiting for the user's decision (SPEC.md §9).
#[derive(Debug, Clone)]
pub struct ChangedKeyPrompt {
    pub id: u64,
    /// What the review is for, e.g. `tunnel “API”` or `host “web”`.
    pub subject: String,
    pub changed: ChangedKey,
}

/// What a host key check was for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StartRequest {
    Tunnel(String),
    Group(String),
    /// Connect to a host: the terminal opens once its keys are known.
    Connect(String),
    /// A host review (the health banner): once no key is unknown, the
    /// stored keys are compared with the server's.
    Review(String),
}

/// What a changed-key review is for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyFor {
    /// A failed tunnel: it starts again once the key is replaced.
    Tunnel(String),
    /// A host (the health banner): the key is only replaced.
    Host(String),
}

#[derive(Debug, Clone)]
pub enum Event {
    Config(Arc<ConfigSnapshot>),
    Data(Arc<AppData>),
    /// Streamer mode's filter for the current config and tunnels; sent
    /// before every [`Event::Data`].
    Redactor(Arc<Redactor>),
    Resolved {
        alias: String,
        /// [`ConfigSnapshot::generation`] the result belongs to.
        generation: u64,
        result: Result<Resolved, String>,
    },
    Ssh(Arc<SshInfo>),
    /// kubectl's binary, version and kubeconfig (M13), after each detection.
    Kubectl(Arc<KubectlInfo>),
    Launched {
        alias: String,
    },
    /// The tour's sandbox didn't run a terminal or touch the agent: what it
    /// would have run (SPEC.md §10 *Onboarding tour*), e.g. `ssh web-01`.
    TourWouldRun {
        command: String,
    },
    /// Tunnel states from the supervisor.
    Tunnels(Arc<Snapshot>),
    /// A start is waiting for an unknown host key to be accepted.
    HostKeyPrompt(Arc<HostKeyPrompt>),
    /// A changed host key was found and needs a decision.
    HostKeyChanged(Arc<ChangedKeyPrompt>),
    /// The prompt with this id is settled.
    HostKeyClosed(u64),
    /// Validation of edit `id`: the problems it would introduce (a save with
    /// problems is not written), or why it could not be checked.
    EditChecked {
        id: u64,
        result: Result<Vec<Problem>, String>,
    },
    /// Edit `id` was written; a new [`Event::Config`] follows.
    EditSaved {
        id: u64,
    },
    /// Edit `id` was not saved: its file changed on disk since editing
    /// started. `disk` is what the file holds now.
    EditConflict {
        id: u64,
        disk: Arc<Vec<u8>>,
    },
    /// Result of [`Command::GenerateKey`] `id`.
    KeyGenerated {
        id: u64,
        result: Result<GeneratedKey, String>,
    },
    /// The key pairs in `~/.ssh` ([`Command::FindKeys`]).
    Keys(Arc<Vec<FoundKey>>),
    /// The file chosen for [`Command::PickKeyFile`] `id`; `None` when the
    /// dialog was cancelled.
    KeyFilePicked {
        id: u64,
        path: Option<PathBuf>,
    },
    /// Something the user asked for succeeded (e.g. "Settings saved").
    Notice(String),
    /// Health results so far and the checks under way.
    Health(Arc<HealthSnapshot>),
    /// The jump hosts on the way to `alias`, first hop first (empty: direct).
    Route {
        alias: String,
        /// [`ConfigSnapshot::generation`] the result belongs to.
        generation: u64,
        result: Result<Vec<RouteHop>, String>,
    },
    Error(String),
}

#[derive(Debug, Clone)]
pub enum ConfigState {
    Loaded {
        set: ConfigSet,
        hosts: HostList,
    },
    /// No config file: no hosts.
    Missing,
    /// The main file could not be read.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    /// Increases with every reload.
    pub generation: u64,
    pub path: PathBuf,
    pub state: ConfigState,
    /// Per loaded file ([`sshconf::FileId`]): why sshelf will not write it.
    pub read_only: Vec<Option<String>>,
}

impl ConfigSnapshot {
    pub fn hosts(&self) -> Option<&HostList> {
        match &self.state {
            ConfigState::Loaded { hosts, .. } => Some(hosts),
            _ => None,
        }
    }

    pub fn set(&self) -> Option<&ConfigSet> {
        match &self.state {
            ConfigState::Loaded { set, .. } => Some(set),
            _ => None,
        }
    }

    /// Why file `id` cannot be edited, if it cannot.
    pub fn read_only(&self, id: sshconf::FileId) -> Option<&str> {
        self.read_only.get(id).and_then(|r| r.as_deref())
    }

    /// Files new hosts can go to: writable loaded files, or the main config
    /// when it does not exist yet.
    pub fn writable_files(&self) -> Vec<PathBuf> {
        match &self.state {
            ConfigState::Loaded { set, .. } => set
                .files
                .iter()
                .enumerate()
                .filter(|(id, _)| self.read_only(*id).is_none())
                .map(|(_, f)| f.path.clone())
                .collect(),
            ConfigState::Missing => vec![self.path.clone()],
            ConfigState::Error(_) => Vec::new(),
        }
    }
}

/// sshelf's own data, as last loaded or saved.
#[derive(Debug, Clone)]
pub struct AppData {
    pub app_dir: PathBuf,
    pub hosts: HostsFile,
    pub hosts_status: LoadStatus,
    pub orphans: Vec<Orphan>,
    /// Aliases an orphan may be relinked to.
    pub relink_targets: Vec<String>,
    pub tunnels: TunnelsFile,
    pub tunnels_status: LoadStatus,
    pub tunnel_orphans: Vec<TunnelOrphan>,
    pub group_orphans: Vec<GroupOrphan>,
    pub settings: Settings,
    pub settings_status: LoadStatus,
    pub history: History,
    pub history_status: LoadStatus,
}

impl AppData {
    /// Nothing loaded yet: every file missing, defaults everywhere.
    pub fn new(app_dir: PathBuf) -> Self {
        Self {
            app_dir,
            hosts: HostsFile::default(),
            hosts_status: LoadStatus::Missing,
            orphans: Vec::new(),
            relink_targets: Vec::new(),
            tunnels: TunnelsFile::default(),
            tunnels_status: LoadStatus::Missing,
            tunnel_orphans: Vec::new(),
            group_orphans: Vec::new(),
            settings: Settings::default(),
            settings_status: LoadStatus::Missing,
            history: History::default(),
            history_status: LoadStatus::Missing,
        }
    }

    pub fn orphan_count(&self) -> usize {
        self.orphans.len() + self.tunnel_orphans.len() + self.group_orphans.len()
    }
}

#[derive(Debug, Clone)]
pub struct SshInfo {
    pub tools: Result<SshTools, String>,
    pub version: Option<Result<String, String>>,
    pub agent: Option<AgentStatus>,
    /// Windows only.
    pub service: Option<Result<AgentService, String>>,
    pub terminal: Option<Terminal>,
    /// Whether the desktop shows tray icons ([`tray_host::available`]).
    pub tray: Result<bool, String>,
}

/// Messages to the core thread: UI commands, relayed supervisor events and
/// results of background host key work.
enum Message {
    Command(Command),
    Supervisor(SupervisorEvent),
    KeyCheck {
        start: StartRequest,
        result: Result<Option<UnknownKey>, String>,
    },
    KeyReview {
        review: KeyFor,
        result: Result<Option<ChangedKey>, String>,
    },
    /// An edit was validated in the background.
    HealthChecked {
        alias: String,
        report: HealthReport,
    },
    EditValidated {
        edit: Edit,
        then: AfterCheck,
        result: Result<Vec<Problem>, String>,
    },
}

/// What happens once an edit is valid.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AfterCheck {
    Report,
    Save {
        overwrite: bool,
    },
    /// Save, then move the app data of `from` to `to`.
    Rename {
        from: String,
        to: String,
    },
}

/// Handle to the core thread; dropping it shuts the core down and stops
/// every tunnel.
pub struct Core {
    tx: Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

/// The onboarding tour's sandbox Core (SPEC.md §10 *Onboarding tour*, §8
/// *Tour sandbox*). Its `Paths` are the sandbox's temporary home; besides:
/// - its ssh and every companion is `ssh` (sshelf as fake-ssh), never one
///   located on this computer or set in Settings;
/// - Connect, terminals and the agent don't run: [`Event::TourWouldRun`]
///   says what would have;
/// - *Install on server* runs through fake-ssh, then restarts the host's
///   failed tunnels;
/// - host keys are not checked (the sample's servers are fake, and the tour
///   has no host key step), and groups never start with it.
#[derive(Debug, Clone)]
pub struct SandboxOptions {
    pub ssh: Tool,
}

impl Core {
    pub fn start(paths: Paths, repaint: Repaint) -> (Self, Receiver<Event>) {
        Self::start_with(paths, None, repaint)
    }

    /// A Core; `sandbox` makes it the tour's.
    pub fn start_with(
        paths: Paths,
        sandbox: Option<SandboxOptions>,
        repaint: Repaint,
    ) -> (Self, Receiver<Event>) {
        let (tx, messages) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let self_tx = tx.clone();
        let thread = thread::Builder::new()
            .name("sshelf-core".into())
            .spawn(move || {
                let emitter = Emitter {
                    tx: events_tx,
                    repaint,
                };
                let mut worker = Worker::new(paths, emitter, self_tx);
                worker.sandbox = sandbox;
                worker.run(messages);
            })
            .expect("spawning the core thread");
        (
            Self {
                tx,
                thread: Some(thread),
            },
            events,
        )
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Message::Command(command));
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Command(Command::Shutdown));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct Emitter {
    tx: Sender<Event>,
    repaint: Repaint,
}

impl Emitter {
    fn emit(&self, event: Event) {
        if self.tx.send(event).is_ok() {
            (self.repaint)();
        }
    }
}

struct Worker {
    paths: Paths,
    emitter: Emitter,
    /// For the watcher (reloads) and the supervisor (events).
    self_tx: Sender<Message>,
    store: Store,
    data: AppData,
    config: Arc<ConfigSnapshot>,
    tools: Result<SshTools, String>,
    /// kubectl, when found (M13).
    kubectl: Option<Tool>,
    terminal: Option<Terminal>,
    watcher: Option<ConfigWatcher>,
    supervisor: Option<Supervisor>,
    tunnel_states: Arc<Snapshot>,
    writer: KnownHostsWriter,
    config_writer: ConfigWriter,
    next_prompt: u64,
    unknown_keys: BTreeMap<u64, (StartRequest, UnknownKey)>,
    changed_keys: BTreeMap<u64, (KeyFor, ChangedKey)>,
    health: HealthSnapshot,
    /// Aliases waiting for a free check slot, in order.
    health_queue: VecDeque<String>,
    health_running: usize,
    /// The tour's sandbox Core.
    sandbox: Option<SandboxOptions>,
}

impl Worker {
    fn new(paths: Paths, emitter: Emitter, self_tx: Sender<Message>) -> Self {
        let mut store = Store::new(paths.app_dir.clone());
        let hosts = store.load::<HostsFile>();
        let tunnels = store.load::<TunnelsFile>();
        let settings = store.load::<Settings>();
        let history = store.load::<History>();
        let data = AppData {
            app_dir: paths.app_dir.clone(),
            hosts: hosts.data,
            hosts_status: hosts.status,
            orphans: Vec::new(),
            relink_targets: Vec::new(),
            tunnels: tunnels.data,
            tunnels_status: tunnels.status,
            tunnel_orphans: Vec::new(),
            group_orphans: Vec::new(),
            settings: settings.data,
            settings_status: settings.status,
            history: history.data,
            history_status: history.status,
        };
        let config = Arc::new(ConfigSnapshot {
            generation: 0,
            path: paths.ssh_config.clone(),
            state: ConfigState::Missing,
            read_only: Vec::new(),
        });
        let config_writer = ConfigWriter::new(paths.app_dir.join("backups"));
        Self {
            paths,
            emitter,
            self_tx,
            store,
            data,
            config,
            tools: Err("not detected yet".into()),
            kubectl: None,
            terminal: None,
            watcher: None,
            supervisor: None,
            tunnel_states: Arc::new(Snapshot::default()),
            writer: KnownHostsWriter::default(),
            config_writer,
            next_prompt: 0,
            unknown_keys: BTreeMap::new(),
            changed_keys: BTreeMap::new(),
            health: HealthSnapshot::default(),
            health_queue: VecDeque::new(),
            health_running: 0,
            sandbox: None,
        }
    }

    /// The sandbox didn't run `command`: tell the UI what it would have.
    fn would_run(&self, command: String) {
        self.emitter.emit(Event::TourWouldRun { command });
    }

    fn run(mut self, messages: Receiver<Message>) {
        let tx = self.self_tx.clone();
        match ConfigWatcher::new(move || {
            let _ = tx.send(Message::Command(Command::Reload));
        }) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(e) => self.error(format!("config changes will not be detected: {e}")),
        }
        self.refresh_ssh();
        self.refresh_kubectl();
        let tx = self.self_tx.clone();
        let ssh = self.tools.as_ref().ok().map(|t| t.ssh.clone());
        let on_event = move |event| {
            let _ = tx.send(Message::Supervisor(event));
        };
        // The tour's sandbox keeps no record: its tunnels are never taken
        // over (SPEC.md §7 *Surviving a crash*).
        self.supervisor = Some(match self.sandbox {
            Some(_) => Supervisor::start(ssh, on_event),
            None => Supervisor::start_recorded(ssh, self.paths.app_dir.clone(), on_event),
        });
        // Located above, before the supervisor existed.
        self.supervise(Request::SetKubectl(self.kubectl.clone()));
        self.reload();
        // Tunnels still running from before a crash, before anything starts.
        if let Some(supervisor) = &self.supervisor {
            supervisor.send(Request::Adopt {
                tunnels: self.data.tunnels.tunnels.clone(),
            });
        }
        // Groups marked "Start on app launch" (SPEC.md §7), through the
        // normal start: host keys are checked and may ask first.
        // Never in the tour's sandbox.
        let launch: Vec<String> = self
            .data
            .tunnels
            .groups
            .iter()
            .filter(|g| g.start_on_launch && self.sandbox.is_none())
            .map(|g| g.id.clone())
            .collect();
        for id in launch {
            self.start(StartRequest::Group(id));
        }

        while let Ok(message) = messages.recv() {
            let command = match message {
                Message::Supervisor(SupervisorEvent::Snapshot(snapshot)) => {
                    self.tunnel_states = Arc::clone(&snapshot);
                    self.emitter.emit(Event::Tunnels(snapshot));
                    continue;
                }
                Message::Supervisor(SupervisorEvent::Error(e)) => {
                    self.error(e);
                    continue;
                }
                Message::Supervisor(SupervisorEvent::Adopted(count)) => {
                    let text = match count {
                        1 => "Took over a tunnel that kept running".to_string(),
                        n => format!("Took over {n} tunnels that kept running"),
                    };
                    self.emitter.emit(Event::Notice(text));
                    continue;
                }
                Message::KeyCheck { start, result } => {
                    self.key_checked(start, result);
                    continue;
                }
                Message::KeyReview { review, result } => {
                    self.key_reviewed(review, result);
                    continue;
                }
                Message::HealthChecked { alias, report } => {
                    self.health_running -= 1;
                    self.health.checking.remove(&alias);
                    self.health.reports.insert(alias, report);
                    self.run_health_checks();
                    continue;
                }
                Message::EditValidated { edit, then, result } => {
                    self.edit_validated(edit, then, result);
                    continue;
                }
                Message::Command(command) => command,
            };
            match command {
                Command::Reload => self.reload(),
                Command::Resolve(alias) => self.resolve(alias),
                Command::SetMetadata { alias, meta } => self.set_metadata(&alias, meta),
                Command::RelinkOrphan { key, to } => self.change_hosts(|hosts, list| {
                    orphans::relink(hosts, list, &key, &to).map_err(|e| e.to_string())
                }),
                Command::DeleteOrphan { key } => self.change_hosts(|hosts, _| {
                    if orphans::delete(hosts, &key) {
                        Ok(())
                    } else {
                        Err(format!("no metadata entry `{key}`"))
                    }
                }),
                Command::SaveSettings(settings) => self.save_settings(settings),
                Command::SetStreamer { on, hide_aliases } => self.set_streamer(on, hide_aliases),
                Command::RefreshSsh => self.refresh_ssh(),
                Command::RefreshKubectl => self.refresh_kubectl(),
                Command::Connect(alias) => self.connect(&alias),
                Command::SaveTunnel(tunnel) => self.save_tunnel(tunnel),
                Command::DeleteTunnel(id) => self.delete_tunnel(&id),
                Command::StartTunnel(id) => self.start(StartRequest::Tunnel(id)),
                Command::StopTunnel(id) => self.supervise(Request::Stop {
                    tunnel_id: id,
                    owner: Owner::Manual,
                }),
                Command::SaveGroup(group) => self.save_group(group),
                Command::DeleteGroup(id) => {
                    self.supervise(Request::StopGroup {
                        group_id: id.clone(),
                    });
                    self.change_tunnels(|file, _| {
                        file.remove_group(&id)
                            .then_some(())
                            .ok_or_else(|| format!("no group `{id}`"))
                    });
                }
                Command::StartGroup(id) => self.start(StartRequest::Group(id)),
                Command::AcceptHostKey(id) => self.accept_host_key(id),
                Command::RejectHostKey(id) => self.reject_host_key(id),
                Command::ReviewChangedKey(tunnel_id) => self.review_changed_key(tunnel_id),
                Command::ReviewHostKey(alias) if self.sandbox.is_some() => {
                    self.would_run(format!("ssh-keygen -F {alias}"));
                }
                Command::ReviewHostKey(alias) => self.start(StartRequest::Review(alias)),
                Command::ReplaceHostKey(id) => self.replace_host_key(id),
                Command::CancelChangedKey(id) => {
                    if self.changed_keys.remove(&id).is_some() {
                        self.emitter.emit(Event::HostKeyClosed(id));
                    }
                }
                Command::StopGroup(id) => self.supervise(Request::StopGroup { group_id: id }),
                Command::RelinkTunnel { tunnel_id, to } => self.change_tunnels(|file, hosts| {
                    orphans::relink_tunnel(file, hosts, &tunnel_id, &to).map_err(|e| e.to_string())
                }),
                Command::RelinkGroupMember { group_id, from, to } => {
                    self.change_tunnels(|file, _| {
                        orphans::relink_group_member(file, &group_id, &from, &to)
                            .map_err(|e| e.to_string())
                    })
                }
                Command::RemoveGroupMember {
                    group_id,
                    tunnel_id,
                } => self.change_tunnels(|file, _| {
                    orphans::remove_group_member(file, &group_id, &tunnel_id)
                        .then_some(())
                        .ok_or_else(|| format!("group has no tunnel `{tunnel_id}`"))
                }),
                Command::CheckEdit(edit) => self.check_edit(edit, AfterCheck::Report),
                Command::SaveEdit { edit, overwrite } => {
                    self.check_edit(edit, AfterCheck::Save { overwrite })
                }
                Command::RenameHost { id, alias, to } => self.rename_host(id, alias, to),
                Command::InstallKey { alias, public_line } if self.sandbox.is_some() => {
                    self.install_in_sandbox(alias, &public_line);
                }
                Command::InstallKey { alias, public_line } => {
                    let args = keygen::install_args(&alias, &public_line);
                    let title = format!("Install key on {alias}");
                    match args.and_then(|args| self.run_in_terminal(&title, "ssh", &args)) {
                        Ok(()) => self.emitter.emit(Event::Notice(format!(
                            "Opened a terminal to install the key on {alias}"
                        ))),
                        Err(e) => self.error(format!("cannot install the key: {e}")),
                    }
                }
                Command::AddKeyToAgent { path, encrypted } => self.add_to_agent(&path, encrypted),
                Command::ResolveRoute(alias) => self.resolve_route(alias),
                Command::GenerateHardwareKey(request) if self.sandbox.is_some() => {
                    match keygen::hardware_args(&request) {
                        Ok(args) => self.would_run(format!("ssh-keygen {}", args.join(" "))),
                        Err(e) => self.error(format!("cannot generate the key: {e}")),
                    }
                }
                Command::GenerateHardwareKey(request) => {
                    let started = keygen::hardware_args(&request).and_then(|args| {
                        self.run_in_terminal("Generate a security key", "ssh-keygen", &args)
                    });
                    match started {
                        Ok(()) => self.emitter.emit(Event::Notice(
                            "Opened a terminal for ssh-keygen: touch your security key when asked"
                                .into(),
                        )),
                        Err(e) => self.error(format!("cannot generate the key: {e}")),
                    }
                }
                Command::FindKeys => {
                    let (dir, emitter) = (self.paths.ssh_dir.clone(), self.emitter.clone());
                    thread::spawn(move || {
                        emitter.emit(Event::Keys(Arc::new(keygen::find_keys(&dir))));
                    });
                }
                Command::PickKeyFile { id, parent } => {
                    let (dir, emitter) = (self.paths.ssh_dir.clone(), self.emitter.clone());
                    // The dialog stays open as long as the user likes: off
                    // the core thread. Async, so on macOS it runs on the
                    // main thread's event loop.
                    thread::spawn(move || {
                        let mut dialog = rfd::AsyncFileDialog::new()
                            .set_title("Choose a key file")
                            .set_directory(&dir);
                        if let Some(parent) = &parent {
                            dialog = dialog.set_parent(parent);
                        }
                        let dialog = dialog.pick_file();
                        let path = pollster::block_on(dialog).map(|f| f.path().to_path_buf());
                        emitter.emit(Event::KeyFilePicked { id, path });
                    });
                }
                Command::LoadPublicKey { id, path } => {
                    let result = keygen::load_public(&path);
                    self.emitter.emit(Event::KeyGenerated { id, result });
                }
                Command::CheckHealth(aliases) => {
                    for alias in aliases {
                        if self.health.checking.insert(alias.clone()) {
                            self.health_queue.push_back(alias);
                        }
                    }
                    self.run_health_checks();
                }
                Command::CancelHealth => {
                    for alias in self.health_queue.drain(..) {
                        self.health.checking.remove(&alias);
                    }
                    self.run_health_checks();
                }
                Command::GenerateKey { id, request } => {
                    // RSA can take seconds: off the core thread.
                    let emitter = self.emitter.clone();
                    thread::spawn(move || {
                        let result = keygen::generate(&request);
                        emitter.emit(Event::KeyGenerated { id, result });
                    });
                }
                // Dropping the supervisor stops every tunnel.
                Command::Shutdown => break,
            }
        }
    }

    fn error(&self, message: String) {
        self.emitter.emit(Event::Error(message));
    }

    fn supervise(&self, request: Request) {
        if let Some(supervisor) = &self.supervisor {
            supervisor.send(request);
        }
    }

    fn reload(&mut self) {
        let state = match ConfigSet::load(&self.paths.ssh_config, &self.paths.resolve_options()) {
            Ok(set) => {
                let hosts = HostList::from_set(&set);
                ConfigState::Loaded { set, hosts }
            }
            Err(sshconf::Error::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                ConfigState::Missing
            }
            Err(e) => ConfigState::Error(e.to_string()),
        };
        let dirs = match &state {
            ConfigState::Loaded { set, .. } => set.watch_dirs(),
            _ => vec![WatchDir {
                path: self.paths.ssh_dir.clone(),
                recursive: false,
            }],
        };
        if let Some(watcher) = &mut self.watcher {
            for problem in watcher.set_dirs(&dirs) {
                self.emitter.emit(Event::Error(problem));
            }
        }
        let read_only = match &state {
            ConfigState::Loaded { set, .. } => set
                .files
                .iter()
                .map(|f| config_edit::read_only_reason(&f.path))
                .collect(),
            _ => Vec::new(),
        };
        self.config = Arc::new(ConfigSnapshot {
            generation: self.config.generation + 1,
            path: self.paths.ssh_config.clone(),
            state,
            read_only,
        });
        self.emitter.emit(Event::Config(Arc::clone(&self.config)));
        self.publish_data();
    }

    /// Recomputes orphans and sends the app data.
    fn publish_data(&mut self) {
        let empty = HostList::default();
        let hosts = match &self.config.state {
            ConfigState::Loaded { hosts, .. } => Some(hosts),
            // Without a config every entry is an orphan; that is the truth.
            ConfigState::Missing => Some(&empty),
            // Unreadable config: do not flag everything as missing.
            ConfigState::Error(_) => None,
        };
        (self.data.orphans, self.data.relink_targets) = match hosts {
            Some(hosts) => (
                orphans::find_orphans(&self.data.hosts, hosts),
                orphans::relink_targets(&self.data.hosts, hosts),
            ),
            None => (Vec::new(), Vec::new()),
        };
        self.data.tunnel_orphans = hosts
            .map(|hosts| orphans::find_tunnel_orphans(&self.data.tunnels, hosts))
            .unwrap_or_default();
        self.data.group_orphans = orphans::find_group_orphans(&self.data.tunnels);
        // Streamer mode's filter follows the config and the tunnels; it goes
        // first, so the data it covers never arrives without it.
        self.emitter
            .emit(Event::Redactor(Arc::new(self.redactor())));
        self.emitter.emit(Event::Data(Arc::new(self.data.clone())));
    }

    /// Streamer mode's filter over the loaded config, the tunnels, the home
    /// folder and the local login (SPEC.md §10 *Streamer mode*).
    fn redactor(&self) -> Redactor {
        let local_user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok();
        Redactor::build(redact::Sources {
            set: self.config.set(),
            hosts: self.config.hosts(),
            tunnels: &self.data.tunnels,
            home: &self.paths.home,
            local_user: local_user.as_deref(),
        })
    }

    fn resolve(&self, alias: String) {
        let generation = self.config.generation;
        let tools = self.tools.clone();
        let emitter = self.emitter.clone();
        thread::spawn(move || {
            let result = tools.and_then(|tools| tools.resolve(&alias));
            emitter.emit(Event::Resolved {
                alias,
                generation,
                result,
            });
        });
    }

    fn set_metadata(&mut self, alias: &str, meta: HostMeta) {
        self.change_hosts(|hosts, list| {
            let host = list
                .find(alias)
                .ok_or_else(|| format!("no host `{alias}`"))?;
            hosts.set(host, meta);
            Ok(())
        });
    }

    /// Applies `change` to `hosts.toml` and saves it; on any failure the
    /// in-memory data stays as it was.
    fn change_hosts(
        &mut self,
        change: impl FnOnce(&mut HostsFile, &HostList) -> Result<(), String>,
    ) {
        let empty = HostList::default();
        let list = self.config.hosts().unwrap_or(&empty);
        let mut hosts = self.data.hosts.clone();
        let result = change(&mut hosts, list)
            .and_then(|()| self.store.save(&hosts).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.data.hosts = hosts;
                self.publish_data();
            }
            Err(e) => self.error(format!("{}: {e}", HostsFile::FILE)),
        }
    }

    /// Like [`Self::change_hosts`], for `tunnels.toml`.
    fn change_tunnels(
        &mut self,
        change: impl FnOnce(&mut TunnelsFile, &HostList) -> Result<(), String>,
    ) {
        let empty = HostList::default();
        let list = self.config.hosts().unwrap_or(&empty);
        let mut tunnels = self.data.tunnels.clone();
        let result = change(&mut tunnels, list)
            .and_then(|()| self.store.save(&tunnels).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.data.tunnels = tunnels;
                self.publish_data();
            }
            Err(e) => self.error(format!("{}: {e}", TunnelsFile::FILE)),
        }
    }

    fn is_active(&self, tunnel_id: &str) -> bool {
        !matches!(
            self.tunnel_states.state(tunnel_id),
            TunnelState::Stopped | TunnelState::Failed(_)
        )
    }

    fn save_tunnel(&mut self, tunnel: Tunnel) {
        if self.is_active(&tunnel.id) {
            return self.error(format!("stop \"{}\" before editing it", tunnel.name));
        }
        // The other type's fields (kept by the editor's switch) aren't saved.
        let tunnel = tunnel.cleaned();
        self.change_tunnels(|file, hosts| {
            let mut problems = tunnel.validate();
            if !tunnel.is_kubectl() && !tunnel.host.is_empty() && hosts.find(&tunnel.host).is_none()
            {
                problems.push(format!("no host `{}`", tunnel.host));
            }
            if !problems.is_empty() {
                return Err(format!(
                    "cannot save \"{}\": {}",
                    tunnel.name,
                    problems.join("; ")
                ));
            }
            file.upsert_tunnel(tunnel);
            Ok(())
        });
    }

    fn delete_tunnel(&mut self, id: &str) {
        self.supervise(Request::Remove {
            tunnel_id: id.to_string(),
        });
        self.change_tunnels(|file, _| {
            file.remove_tunnel(id)
                .then_some(())
                .ok_or_else(|| format!("no tunnel `{id}`"))
        });
    }

    fn save_group(&mut self, group: Group) {
        let active = supervisor::GroupState::Stopped != self.tunnel_states.group_state(&group)
            || self.data.tunnels.group(&group.id).is_some_and(|old| {
                self.tunnel_states.group_state(old) != supervisor::GroupState::Stopped
            });
        if active {
            return self.error(format!("stop \"{}\" before editing it", group.name));
        }
        self.change_tunnels(|file, _| {
            if group.name.trim().is_empty() {
                return Err("cannot save a group without a name".to_string());
            }
            if let Some(missing) = group.tunnels.iter().find(|t| file.tunnel(t).is_none()) {
                return Err(format!("no tunnel `{missing}`"));
            }
            file.upsert_group(group);
            Ok(())
        });
    }

    /// The tunnel's definition, if its host exists (kubectl tunnels have
    /// none; the supervisor reports a missing kubectl).
    fn startable(&self, tunnel: &Tunnel) -> Result<(), String> {
        if tunnel.is_kubectl() {
            return Ok(());
        }
        let exists = self
            .config
            .hosts()
            .is_some_and(|hosts| hosts.find(&tunnel.host).is_some());
        if exists {
            Ok(())
        } else {
            Err(format!("\"{}\": no host `{}`", tunnel.name, tunnel.host))
        }
    }

    /// The supervisor request for `start` once its host keys are known, or why
    /// it cannot start.
    fn launch_request(&self, start: &StartRequest) -> Result<Request, String> {
        match start {
            StartRequest::Tunnel(id) => {
                let tunnel = self
                    .data
                    .tunnels
                    .tunnel(id)
                    .cloned()
                    .ok_or_else(|| format!("no tunnel `{id}`"))?;
                self.startable(&tunnel)
                    .map_err(|e| format!("cannot start {e}"))?;
                Ok(Request::Start {
                    tunnel,
                    owner: Owner::Manual,
                })
            }
            StartRequest::Group(id) => {
                let group = self
                    .data
                    .tunnels
                    .group(id)
                    .cloned()
                    .ok_or_else(|| format!("no group `{id}`"))?;
                let tunnels: Vec<Tunnel> = group
                    .tunnels
                    .iter()
                    .filter_map(|t| self.data.tunnels.tunnel(t).cloned())
                    .collect();
                let problems: Vec<String> = tunnels
                    .iter()
                    .filter_map(|t| self.startable(t).err())
                    .collect();
                if !problems.is_empty() {
                    return Err(format!(
                        "cannot start \"{}\": {}",
                        group.name,
                        problems.join("; ")
                    ));
                }
                Ok(Request::StartGroup { group, tunnels })
            }
            StartRequest::Connect(_) | StartRequest::Review(_) => {
                Err("not a tunnel start".to_string())
            }
        }
    }

    /// The hosts `start` connects to (jump hosts are found from them).
    fn destinations(&self, start: &StartRequest) -> Result<Vec<String>, String> {
        let mut destinations = match start {
            StartRequest::Connect(alias) | StartRequest::Review(alias) => vec![alias.clone()],
            // kubectl tunnels have no host keys.
            StartRequest::Tunnel(_) | StartRequest::Group(_) => match self.launch_request(start)? {
                Request::Start { tunnel, .. } => vec![tunnel]
                    .into_iter()
                    .filter(|t| !t.is_kubectl())
                    .map(|t| t.host)
                    .collect(),
                Request::StartGroup { tunnels, .. } => tunnels
                    .into_iter()
                    .filter(|t| !t.is_kubectl())
                    .map(|t| t.host)
                    .collect(),
                _ => Vec::new(),
            },
        };
        destinations.dedup();
        Ok(destinations)
    }

    fn describe(&self, start: &StartRequest) -> String {
        match start {
            StartRequest::Tunnel(id) => self.data.tunnels.tunnel(id).map_or_else(
                || format!("tunnel {id}"),
                |t| format!("tunnel \"{}\"", t.name),
            ),
            StartRequest::Group(id) => self.data.tunnels.group(id).map_or_else(
                || format!("group {id}"),
                |g| format!("group \"{}\"", g.name),
            ),
            StartRequest::Connect(alias) => format!("connecting to {alias}"),
            StartRequest::Review(alias) => format!("host \"{alias}\""),
        }
    }

    fn keygen(&self) -> Result<(SshTools, Keygen), String> {
        let tools = self.tools.clone()?;
        let keygen = Keygen {
            tool: tools.companion("ssh-keygen"),
        };
        Ok((tools, keygen))
    }

    /// Checks the host keys of every host `start` connects through (jump
    /// hosts first) in the background, then starts or asks (SPEC.md §9).
    fn start(&mut self, start: StartRequest) {
        let destinations = match self.destinations(&start) {
            Ok(destinations) => destinations,
            Err(e) => return self.error(e),
        };
        if self.sandbox.is_some() || destinations.is_empty() {
            // Fake servers, or only kubectl tunnels: no host keys to check.
            return self.key_checked(start, Ok(None));
        }
        let (tools, keygen) = match self.keygen() {
            Ok(found) => found,
            Err(e) => return self.key_checked(start, Err(e)),
        };
        let home = self.paths.home.clone();
        let tx = self.self_tx.clone();
        thread::spawn(move || {
            let result = hostkeys::first_unknown(&tools, &keygen, &home, &destinations);
            let _ = tx.send(Message::KeyCheck { start, result });
        });
    }

    fn key_checked(&mut self, start: StartRequest, result: Result<Option<UnknownKey>, String>) {
        match (result, start) {
            // Connect: a check that can't run doesn't stop it; ssh in the
            // terminal says what is wrong.
            (Ok(None) | Err(_), StartRequest::Connect(alias)) => self.open_terminal(&alias),
            (Ok(None), StartRequest::Review(alias)) => self.review_changed(KeyFor::Host(alias)),
            (Ok(None), start) => match self.launch_request(&start) {
                Ok(request) => self.supervise(request),
                Err(e) => self.error(e),
            },
            (Ok(Some(unknown)), start) => {
                self.next_prompt += 1;
                let prompt = HostKeyPrompt {
                    id: self.next_prompt,
                    purpose: self.describe(&start),
                    unknown: unknown.clone(),
                };
                self.unknown_keys.insert(prompt.id, (start, unknown));
                self.emitter.emit(Event::HostKeyPrompt(Arc::new(prompt)));
            }
            (Err(e), StartRequest::Review(alias)) => {
                self.error(format!("cannot check the host keys of {alias}: {e}"));
            }
            (Err(e), start) => self.error(format!("{} not started: {e}", self.describe(&start))),
        }
    }

    fn accept_host_key(&mut self, id: u64) {
        let Some((start, unknown)) = self.unknown_keys.remove(&id) else {
            return;
        };
        self.emitter.emit(Event::HostKeyClosed(id));
        let Some(file) = unknown.target.user_files.first() else {
            return self.error("no UserKnownHostsFile to store the key in".to_string());
        };
        if let Err(e) = self.writer.append(file, &unknown.lines) {
            return self.error(e);
        }
        // Next hop, or the start itself.
        self.start(start);
    }

    fn reject_host_key(&mut self, id: u64) {
        let Some((start, unknown)) = self.unknown_keys.remove(&id) else {
            return;
        };
        self.emitter.emit(Event::HostKeyClosed(id));
        let what = match &start {
            StartRequest::Connect(alias) => format!("not connected to {alias}"),
            StartRequest::Review(alias) => format!("nothing stored for {alias}"),
            _ => format!("{} not started", self.describe(&start)),
        };
        self.error(format!(
            "{what}: the host key of {} was not accepted",
            unknown.target.display()
        ));
    }

    fn review_changed_key(&mut self, tunnel_id: String) {
        if self.data.tunnels.tunnel(&tunnel_id).is_none() {
            return self.error(format!("no tunnel `{tunnel_id}`"));
        }
        self.review_changed(KeyFor::Tunnel(tunnel_id));
    }

    /// Compares the stored keys on the way to `review`'s host with the
    /// servers' in the background (it connects).
    fn review_changed(&mut self, review: KeyFor) {
        let host = match &review {
            KeyFor::Tunnel(id) => match self.data.tunnels.tunnel(id) {
                Some(tunnel) if tunnel.is_kubectl() => {
                    return self.error(format!("\"{}\" has no host keys", tunnel.name));
                }
                Some(tunnel) => tunnel.host.clone(),
                None => return self.error(format!("no tunnel `{id}`")),
            },
            KeyFor::Host(alias) => alias.clone(),
        };
        let (tools, keygen) = match self.keygen() {
            Ok(found) => found,
            Err(e) => return self.error(e),
        };
        let home = self.paths.home.clone();
        let tx = self.self_tx.clone();
        thread::spawn(move || {
            let result = hostkeys::find_changed(&tools, &keygen, &home, &host);
            let _ = tx.send(Message::KeyReview { review, result });
        });
    }

    fn key_reviewed(&mut self, review: KeyFor, result: Result<Option<ChangedKey>, String>) {
        match (result, review) {
            (Ok(Some(changed)), review) => {
                self.next_prompt += 1;
                let subject = match &review {
                    KeyFor::Tunnel(id) => {
                        let name = self
                            .data
                            .tunnels
                            .tunnel(id)
                            .map_or(id.as_str(), |t| &t.name);
                        format!("tunnel \u{201c}{name}\u{201d}")
                    }
                    KeyFor::Host(alias) => format!("host \u{201c}{alias}\u{201d}"),
                };
                let prompt = ChangedKeyPrompt {
                    id: self.next_prompt,
                    subject,
                    changed: changed.clone(),
                };
                self.changed_keys.insert(prompt.id, (review, changed));
                self.emitter.emit(Event::HostKeyChanged(Arc::new(prompt)));
            }
            (Ok(None), KeyFor::Tunnel(_)) => self.error(
                "every stored host key matches its server now; try starting again".to_string(),
            ),
            (Ok(None), KeyFor::Host(alias)) => self.emitter.emit(Event::Notice(format!(
                "The stored host keys of {alias} match its servers"
            ))),
            (Err(e), _) => self.error(e),
        }
    }

    /// `ssh-keygen -R` the old key from every user file holding it, store the
    /// new one, then start the tunnel again (a host review only replaces it).
    /// Never automatic (SPEC.md §9).
    fn replace_host_key(&mut self, id: u64) {
        let Some((review, changed)) = self.changed_keys.remove(&id) else {
            return;
        };
        self.emitter.emit(Event::HostKeyClosed(id));
        if changed.user_files.is_empty() {
            return self.error(format!(
                "the old key of {} is in the system-wide known_hosts; an administrator has to replace it",
                changed.target.display()
            ));
        }
        let (_, keygen) = match self.keygen() {
            Ok(found) => found,
            Err(e) => return self.error(e),
        };
        for file in &changed.user_files {
            let removed = self
                .writer
                .backup_once(file)
                .and_then(|()| keygen.remove(&changed.target.lookup, file));
            if let Err(e) = removed {
                return self.error(e);
            }
        }
        let file = changed
            .target
            .user_files
            .first()
            .unwrap_or(&changed.user_files[0])
            .clone();
        if let Err(e) = self.writer.append(&file, &changed.new_lines) {
            return self.error(e);
        }
        match review {
            KeyFor::Tunnel(tunnel_id) => self.start(StartRequest::Tunnel(tunnel_id)),
            KeyFor::Host(_) => self.emitter.emit(Event::Notice(format!(
                "Replaced the host key of {}",
                changed.target.display()
            ))),
        }
    }

    fn save_settings(&mut self, settings: Settings) {
        match self.store.save(&settings) {
            Ok(()) => {
                self.data.settings = settings;
                self.publish_data();
                self.emitter.emit(Event::Notice(format!(
                    "Settings saved to {}",
                    Settings::FILE
                )));
                self.refresh_ssh();
                self.refresh_kubectl();
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Streamer mode takes effect even when `settings.toml` can't be
    /// written: masking matters more than remembering it, and the error says
    /// it won't survive a restart. The app shows its own toast.
    fn set_streamer(&mut self, on: bool, hide_aliases: bool) {
        let mut settings = self.data.settings.clone();
        settings.streamer_mode = on;
        settings.streamer_hide_aliases = hide_aliases;
        let saved = self.store.save(&settings);
        self.data.settings = settings;
        self.publish_data();
        if let Err(e) = saved {
            self.error(e.to_string());
        }
    }

    /// Only files of the loaded config set (or the main config before it
    /// exists) are ever written, and none that is read-only.
    fn editable(&self, file: &Path) -> Result<(), String> {
        if file == self.paths.ssh_config && matches!(self.config.state, ConfigState::Missing) {
            return Ok(());
        }
        let id = self
            .config
            .set()
            .and_then(|set| set.files.iter().position(|f| f.path == file))
            .ok_or_else(|| format!("{} is not part of the loaded ssh config", file.display()))?;
        match self.config.read_only(id) {
            Some(reason) => Err(format!("{}: {reason}", file.display())),
            None => Ok(()),
        }
    }

    /// Validates `edit` in the background (`ssh -G -F`, SPEC.md §5).
    fn check_edit(&mut self, edit: Edit, then: AfterCheck) {
        let ssh = match (self.editable(&edit.file), &self.tools) {
            (Err(e), _) => return self.edit_checked(edit.id, Err(e)),
            (_, Err(e)) => return self.edit_checked(edit.id, Err(format!("cannot validate: {e}"))),
            (Ok(()), Ok(tools)) => tools.ssh.clone(),
        };
        let tx = self.self_tx.clone();
        thread::spawn(move || {
            let result = config_edit::validate(&ssh, &edit);
            let _ = tx.send(Message::EditValidated { edit, then, result });
        });
    }

    fn edit_checked(&self, id: u64, result: Result<Vec<Problem>, String>) {
        self.emitter.emit(Event::EditChecked { id, result });
    }

    fn edit_validated(
        &mut self,
        edit: Edit,
        then: AfterCheck,
        result: Result<Vec<Problem>, String>,
    ) {
        let (overwrite, rename) = match (then, &result) {
            (AfterCheck::Save { overwrite }, Ok(problems)) if problems.is_empty() => {
                (overwrite, None)
            }
            (AfterCheck::Rename { from, to }, Ok(problems)) if problems.is_empty() => {
                (false, Some((from, to)))
            }
            _ => return self.edit_checked(edit.id, result),
        };
        if let Err(e) = self.editable(&edit.file) {
            return self.edit_checked(edit.id, Err(e));
        }
        if let Some((from, _)) = &rename
            && let Err(e) = self.renamable(from)
        {
            return self.edit_checked(edit.id, Err(e));
        }
        let disk = match fs::read(&edit.file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return self.edit_checked(
                    edit.id,
                    Err(format!("reading {}: {e}", edit.file.display())),
                );
            }
        };
        if disk != edit.base && !overwrite {
            // The editor needs the new version to show and rebase onto.
            self.reload();
            return self.emitter.emit(Event::EditConflict {
                id: edit.id,
                disk: Arc::new(disk),
            });
        }
        let written = edit
            .candidate()
            .and_then(|(file, _)| self.config_writer.write(&edit.file, &file.to_bytes()));
        if let Err(e) = written {
            return self.edit_checked(edit.id, Err(e));
        }
        if let Some((from, to)) = rename
            && let Err(e) = self.rename_data(&from, &to)
        {
            // One operation: without its app data the rename is undone.
            let restored = self.config_writer.write(&edit.file, &edit.base);
            self.reload();
            let outcome = match restored {
                Ok(()) => "the config was restored".to_string(),
                Err(r) => format!("restoring the config failed too: {r}"),
            };
            return self.edit_checked(edit.id, Err(format!("{e}; {outcome}")));
        }
        self.reload();
        self.emitter.emit(Event::EditSaved { id: edit.id });
    }

    /// Why tunnels of `alias` keep it from being renamed now.
    fn renamable(&self, alias: &str) -> Result<(), String> {
        let running: Vec<&str> = self
            .data
            .tunnels
            .tunnels
            .iter()
            .filter(|t| t.host == alias && self.is_active(&t.id))
            .map(|t| t.name.as_str())
            .collect();
        if running.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "stop {} before renaming `{alias}`",
                running
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }

    /// Renames `from` on its host's `Host` line, validates and saves that,
    /// then moves the app data along (SPEC.md §5).
    fn rename_host(&mut self, id: u64, from: String, to: String) {
        let prepared = (|| {
            let set = self.config.set().ok_or("the ssh config is not loaded")?;
            let hosts = self.config.hosts().ok_or("the ssh config is not loaded")?;
            let host = hosts
                .find(&from)
                .ok_or_else(|| format!("no host `{from}`"))?;
            rename::check_new_alias(&to, hosts)?;
            self.renamable(&from)?;
            let file = &set.files[host.at.file];
            let block = file
                .block_at(host.at.line)
                .ok_or("the host's block was not found")?;
            let text = sshconf::edit::rename_alias(&file.text(block.lines.clone()), &from, &to)
                .ok_or_else(|| format!("`{from}` is not on the host's Host line"))?;
            Ok::<_, String>(Edit {
                id,
                file: file.path.clone(),
                base: file.to_bytes(),
                change: Change::Replace {
                    lines: block.lines,
                    text,
                },
                alias: to.clone(),
            })
        })();
        match prepared {
            Ok(edit) => self.check_edit(edit, AfterCheck::Rename { from, to }),
            Err(e) => self.edit_checked(id, Err(e)),
        }
    }

    /// Moves metadata, tunnels and history from `from` to `to`; a file that
    /// was already saved is put back if a later one fails.
    fn rename_data(&mut self, from: &str, to: &str) -> Result<(), String> {
        let (mut hosts, mut tunnels, mut history) = (
            self.data.hosts.clone(),
            self.data.tunnels.clone(),
            self.data.history.clone(),
        );
        let renamed = rename::rename_in(&mut hosts, &mut tunnels, &mut history, from, to);
        // 0: hosts.toml, 1: tunnels.toml, 2: history.toml
        let needed = [renamed.metadata, renamed.tunnels > 0, renamed.history > 0];
        let mut saved = Vec::new();
        for file in (0..3).filter(|&f| needed[f]) {
            let result = match file {
                0 => self.store.save(&hosts),
                1 => self.store.save(&tunnels),
                _ => self.store.save(&history),
            };
            if let Err(e) = result {
                for file in saved.into_iter().rev() {
                    let _ = match file {
                        0 => self.store.save(&self.data.hosts),
                        1 => self.store.save(&self.data.tunnels),
                        _ => self.store.save(&self.data.history),
                    };
                }
                return Err(e.to_string());
            }
            saved.push(file);
        }
        self.data.hosts = hosts;
        self.data.tunnels = tunnels;
        self.data.history = history;
        Ok(())
    }

    /// Locates the tools now; version, agent and service are checked in the
    /// background and reported as one [`Event::Ssh`].
    fn refresh_ssh(&mut self) {
        let sandbox = self.sandbox.clone();
        self.tools = match &sandbox {
            Some(sandbox) => Ok(SshTools::sandbox(sandbox.ssh.clone())),
            None => {
                SshTools::locate(self.data.settings.ssh_path.as_deref()).map_err(|e| e.to_string())
            }
        };
        self.supervise(Request::SetSsh(
            self.tools.as_ref().ok().map(|t| t.ssh.clone()),
        ));
        // The sandbox opens no terminal, and leaves the agent's service be.
        self.terminal = sandbox.is_none().then(Terminal::detect).flatten();
        let tools = self.tools.clone();
        let terminal = self.terminal.clone();
        let emitter = self.emitter.clone();
        thread::spawn(move || {
            let (version, agent) = match &tools {
                Ok(t) => (Some(t.version()), Some(t.agent_status())),
                Err(_) => (None, None),
            };
            emitter.emit(Event::Ssh(Arc::new(SshInfo {
                tools,
                version,
                agent,
                service: sandbox.is_none().then(agent_service::query).flatten(),
                terminal,
                tray: tray_host::available(),
            })));
        });
    }

    /// Locates kubectl now (SPEC.md §8 *kubectl*); its version and the
    /// kubeconfig are read in the background and reported as one
    /// [`Event::Kubectl`]. The tour's sandbox never runs kubectl.
    fn refresh_kubectl(&mut self) {
        let kubectl = if self.sandbox.is_some() {
            Err(kubectl::NOT_IN_TOUR.to_string())
        } else {
            kubectl::locate(self.data.settings.kubectl_path.as_deref(), &self.paths.kube)
                .map_err(|e| e.to_string())
        };
        self.kubectl = kubectl.as_ref().ok().map(|k| k.tool.clone());
        self.supervise(Request::SetKubectl(self.kubectl.clone()));
        let env = self.paths.kube.clone();
        let home = self.paths.home.clone();
        let emitter = self.emitter.clone();
        thread::spawn(move || {
            let info = KubectlInfo::check(kubectl, &env, &home);
            emitter.emit(Event::Kubectl(Arc::new(info)));
        });
    }

    /// The jump hosts on the way to `alias`, from the same walk the host
    /// key checks use (nested `ProxyJump`s, each hop once, depth-limited).
    fn resolve_route(&self, alias: String) {
        let generation = self.config.generation;
        let tools = self.tools.clone();
        let home = self.paths.home.clone();
        let emitter = self.emitter.clone();
        thread::spawn(move || {
            let result = tools.and_then(|tools| {
                let chain = hostkeys::chain(&tools, &home, std::slice::from_ref(&alias))?;
                // The last entry is the destination itself.
                Ok(chain
                    .into_iter()
                    .filter(|(target, _)| target.destination != alias)
                    .map(|(target, _)| RouteHop {
                        destination: target.destination,
                        hostname: target.hostname,
                        port: target.port,
                    })
                    .collect())
            });
            emitter.emit(Event::Route {
                alias,
                generation,
                result,
            });
        });
    }

    /// Starts queued health checks while slots are free, then publishes.
    fn run_health_checks(&mut self) {
        while self.health_running < health::MAX_PARALLEL
            && let Some(alias) = self.health_queue.pop_front()
        {
            let report = match &self.tools {
                Ok(tools) => {
                    let (ssh, tx) = (tools.ssh.clone(), self.self_tx.clone());
                    self.health_running += 1;
                    thread::spawn(move || {
                        let report = health::check(&ssh, &alias);
                        let _ = tx.send(Message::HealthChecked { alias, report });
                    });
                    continue;
                }
                Err(e) => HealthReport {
                    health: Health::Unreachable(format!("no ssh: {e}")),
                    checked_at: SystemTime::now(),
                },
            };
            self.health.checking.remove(&alias);
            self.health.reports.insert(alias, report);
        }
        self.emitter
            .emit(Event::Health(Arc::new(self.health.clone())));
    }

    fn add_to_agent(&mut self, path: &Path, encrypted: bool) {
        // A path from `ssh -G` may start with `~`, and no shell expands it.
        let path = &hostkeys::expand_path(&path.to_string_lossy(), &self.paths.home);
        let args = keygen::ssh_add_args(path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.sandbox.is_some() {
            return self.would_run(format!("ssh-add {}", args.join(" ")));
        }
        if encrypted {
            match self.run_in_terminal("ssh-add", "ssh-add", &args) {
                Ok(()) => self.emitter.emit(Event::Notice(format!(
                    "Opened a terminal for ssh-add: type the passphrase of {name} there"
                ))),
                Err(e) => self.error(format!("cannot run ssh-add: {e}")),
            }
            return;
        }
        let ssh_add = match &self.tools {
            Ok(tools) => tools.companion("ssh-add"),
            Err(e) => return self.error(format!("cannot run ssh-add: {e}")),
        };
        let emitter = self.emitter.clone();
        let tx = self.self_tx.clone();
        thread::spawn(move || {
            let mut cmd = ssh_add.command();
            cmd.args(&args);
            match process::run(cmd, ssh::QUICK_TIMEOUT) {
                Ok(out) if out.code == Some(0) => {
                    emitter.emit(Event::Notice(format!("Added {name} to the ssh agent")));
                    // The agent's key count changed.
                    let _ = tx.send(Message::Command(Command::RefreshSsh));
                }
                Ok(out) => {
                    let why = out.stderr.lines().map(str::trim).find(|l| !l.is_empty());
                    emitter.emit(Event::Error(format!(
                        "ssh-add failed: {}",
                        why.unwrap_or("no reason given")
                    )));
                }
                Err(e) => emitter.emit(Event::Error(format!("ssh-add failed: {e}"))),
            }
        });
    }

    /// *Install on server* in the sandbox: the install command runs through
    /// fake-ssh (no terminal), which adds the key to the sample server; then
    /// the host's failed tunnels start again (SPEC.md §10 *Onboarding tour*).
    fn install_in_sandbox(&mut self, alias: String, public_line: &str) {
        let prepared = keygen::install_args(&alias, public_line)
            .and_then(|args| Ok((args, self.tools.clone()?)));
        let (args, tools) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => return self.error(format!("cannot install the key: {e}")),
        };
        let failed: Vec<String> = self
            .data
            .tunnels
            .tunnels
            .iter()
            .filter(|t| {
                t.host == alias && matches!(self.tunnel_states.state(&t.id), TunnelState::Failed(_))
            })
            .map(|t| t.id.clone())
            .collect();
        let (emitter, tx) = (self.emitter.clone(), self.self_tx.clone());
        thread::spawn(move || {
            let mut cmd = tools.ssh.command();
            cmd.args(&args);
            match process::run(cmd, ssh::QUICK_TIMEOUT) {
                Ok(out) if out.code == Some(0) => {
                    emitter.emit(Event::Notice(format!("fake-ssh: key added to {alias}")));
                    for id in failed {
                        let _ = tx.send(Message::Command(Command::StartTunnel(id)));
                    }
                }
                Ok(out) => emitter.emit(Event::Error(format!(
                    "cannot install the key: {}",
                    out.stderr.trim()
                ))),
                Err(e) => emitter.emit(Event::Error(format!("cannot install the key: {e}"))),
            }
        });
    }

    /// Runs `tool` (`ssh` or a companion such as `ssh-add`) with `args` in
    /// the user's terminal (SPEC.md §10, §11).
    fn run_in_terminal(&self, title: &str, tool: &str, args: &[String]) -> Result<(), String> {
        let tools = self.tools.as_ref().map_err(String::clone)?;
        let program = if tool == "ssh" {
            tools.ssh.program.clone()
        } else {
            tools.companion(tool).program
        };
        let plan = terminal::plan_command(
            &self.data.settings.terminal_command,
            &program,
            args,
            title,
            self.terminal.as_ref(),
        )
        .map_err(|e| e.to_string())?;
        terminal::spawn(&plan)
    }

    /// Connect: in the sandbox only what would run; otherwise the host keys
    /// are checked first (an unknown one asks, SPEC.md §9), then the
    /// terminal opens ([`Self::open_terminal`]).
    fn connect(&mut self, alias: &str) {
        if self.sandbox.is_some() {
            self.would_run(format!("ssh {alias}"));
            self.record_session(alias);
        } else {
            self.start(StartRequest::Connect(alias.to_string()));
        }
    }

    /// Opens the terminal running ssh for `alias`, and records the session.
    fn open_terminal(&mut self, alias: &str) {
        let tools = match &self.tools {
            Ok(tools) => tools,
            Err(e) => return self.error(format!("cannot connect: {e}")),
        };
        let plan = match terminal::plan(
            &self.data.settings.terminal_command,
            &tools.ssh.program,
            alias,
            self.terminal.as_ref(),
        ) {
            Ok(plan) => plan,
            Err(e) => return self.error(format!("cannot connect: {e}")),
        };
        if let Err(e) = terminal::spawn(&plan) {
            return self.error(format!("cannot connect: {e}"));
        }
        self.emitter.emit(Event::Launched {
            alias: alias.to_string(),
        });
        self.record_session(alias);
    }

    /// "Last launched from sshelf" and the tray's recent hosts.
    fn record_session(&mut self, alias: &str) {
        let mut history = self.data.history.clone();
        history.record(alias, SystemTime::now());
        match self.store.save(&history) {
            Ok(()) => {
                self.data.history = history;
                self.publish_data();
            }
            Err(e) => self.error(format!("session started, but {e}")),
        }
    }
}
