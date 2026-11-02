//! The core service (SPEC.md §2): owns every file and process. The UI sends
//! [`Command`]s and renders [`Event`]s; after each event the repaint callback
//! runs, so the UI never polls. The ssh config is only ever read.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use sshconf::{ConfigSet, HostList, WatchDir};

use crate::agent_service::{self, AgentService};
use crate::history::History;
use crate::metadata::{HostMeta, HostsFile};
use crate::orphans::{self, Orphan};
use crate::paths::Paths;
use crate::settings::Settings;
use crate::ssh::{AgentStatus, Resolved, SshTools};
use crate::storage::{LoadStatus, Store, Versioned};
use crate::terminal::{self, Terminal};
use crate::watch::ConfigWatcher;

/// Called after every event; the UI passes `ctx.request_repaint()`.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

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
    /// Re-detect the ssh binary, version, agent and service.
    RefreshSsh,
    /// Open a terminal session for an alias.
    Connect(String),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    Config(Arc<ConfigSnapshot>),
    Data(Arc<AppData>),
    Resolved {
        alias: String,
        /// [`ConfigSnapshot::generation`] the result belongs to.
        generation: u64,
        result: Result<Resolved, String>,
    },
    Ssh(Arc<SshInfo>),
    Launched {
        alias: String,
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
}

impl ConfigSnapshot {
    pub fn hosts(&self) -> Option<&HostList> {
        match &self.state {
            ConfigState::Loaded { hosts, .. } => Some(hosts),
            _ => None,
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
    pub settings: Settings,
    pub settings_status: LoadStatus,
    pub history: History,
    pub history_status: LoadStatus,
}

#[derive(Debug, Clone)]
pub struct SshInfo {
    pub tools: Result<SshTools, String>,
    pub version: Option<Result<String, String>>,
    pub agent: Option<AgentStatus>,
    /// Windows only.
    pub service: Option<Result<AgentService, String>>,
    pub terminal: Option<Terminal>,
}

/// Handle to the core thread; dropping it shuts the core down.
pub struct Core {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Core {
    pub fn start(paths: Paths, repaint: Repaint) -> (Self, Receiver<Event>) {
        let (tx, commands) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let self_tx = tx.clone();
        let thread = thread::Builder::new()
            .name("sshelf-core".into())
            .spawn(move || {
                let emitter = Emitter {
                    tx: events_tx,
                    repaint,
                };
                Worker::new(paths, emitter, self_tx).run(commands);
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
        let _ = self.tx.send(command);
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
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
    /// For the watcher, which sends `Reload`.
    self_tx: Sender<Command>,
    store: Store,
    data: AppData,
    config: Arc<ConfigSnapshot>,
    tools: Result<SshTools, String>,
    terminal: Option<Terminal>,
    watcher: Option<ConfigWatcher>,
}

impl Worker {
    fn new(paths: Paths, emitter: Emitter, self_tx: Sender<Command>) -> Self {
        let mut store = Store::new(paths.app_dir.clone());
        let hosts = store.load::<HostsFile>();
        let settings = store.load::<Settings>();
        let history = store.load::<History>();
        let data = AppData {
            app_dir: paths.app_dir.clone(),
            hosts: hosts.data,
            hosts_status: hosts.status,
            orphans: Vec::new(),
            relink_targets: Vec::new(),
            settings: settings.data,
            settings_status: settings.status,
            history: history.data,
            history_status: history.status,
        };
        let config = Arc::new(ConfigSnapshot {
            generation: 0,
            path: paths.ssh_config.clone(),
            state: ConfigState::Missing,
        });
        Self {
            paths,
            emitter,
            self_tx,
            store,
            data,
            config,
            tools: Err("not detected yet".into()),
            terminal: None,
            watcher: None,
        }
    }

    fn run(mut self, commands: Receiver<Command>) {
        let tx = self.self_tx.clone();
        match ConfigWatcher::new(move || {
            let _ = tx.send(Command::Reload);
        }) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(e) => self.error(format!("config changes will not be detected: {e}")),
        }
        self.refresh_ssh();
        self.reload();

        while let Ok(command) = commands.recv() {
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
                Command::RefreshSsh => self.refresh_ssh(),
                Command::Connect(alias) => self.connect(&alias),
                Command::Shutdown => break,
            }
        }
    }

    fn error(&self, message: String) {
        self.emitter.emit(Event::Error(message));
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
        self.config = Arc::new(ConfigSnapshot {
            generation: self.config.generation + 1,
            path: self.paths.ssh_config.clone(),
            state,
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
        self.emitter.emit(Event::Data(Arc::new(self.data.clone())));
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

    fn save_settings(&mut self, settings: Settings) {
        match self.store.save(&settings) {
            Ok(()) => {
                self.data.settings = settings;
                self.publish_data();
                self.refresh_ssh();
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Locates the tools now; version, agent and service are checked in the
    /// background and reported as one [`Event::Ssh`].
    fn refresh_ssh(&mut self) {
        self.tools =
            SshTools::locate(self.data.settings.ssh_path.as_deref()).map_err(|e| e.to_string());
        self.terminal = Terminal::detect();
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
                service: agent_service::query(),
                terminal,
            })));
        });
    }

    fn connect(&mut self, alias: &str) {
        let tools = match &self.tools {
            Ok(tools) => tools,
            Err(e) => return self.error(format!("cannot connect: {e}")),
        };
        let plan = match terminal::plan(
            &self.data.settings.terminal_command,
            &tools.ssh,
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
