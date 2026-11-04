//! Drives the onboarding tour (SPEC.md §10 *Onboarding tour*) against the
//! app: swaps the user's Core and views for the sandbox's, applies each
//! step's screen, works out whether its action is done or busy, and draws
//! the overlay. The user's Core keeps running untouched meanwhile and takes
//! over again when the tour ends.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use egui::{Context, Rect};
use sshelf_core::core::{AppData, Command, ConfigSnapshot, Core, Event, SandboxOptions, SshInfo};
use sshelf_core::settings::{CloseBehavior, Settings, TourState};
use sshelf_core::supervisor::{GroupState, Snapshot, TunnelState};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Tunnel};

use super::SshelfApp;
use crate::groups_view::GroupsView;
use crate::hostkey_modal::HostKeyModals;
use crate::hosts_view::HostsView;
use crate::orphans_window::OrphansWindow;
use crate::sidebar::View;
use crate::tour::sandbox::{FakeSsh, SAMPLE_PORTS, Sandbox};
use crate::tour::steps::{
    self, Action, Chapter, STEPS, Screen, TOUR_BACKUP, TOUR_BACKUP_HOST, TOUR_DB, TOUR_HOST,
    TOUR_STAGING_GROUP, TOUR_WEB_ADMIN,
};
use crate::tour::{self, LATE_FRAMES, Tour, overlay, targets};
use crate::tunnels_view::TunnelsView;

/// The passphrase the key step fills in (the key lives in the sandbox).
const TOUR_PASSPHRASE: &str = "tour-sample-passphrase";
/// What the status bar shows on the status bar step: backup's error from
/// before.
const BACKUP_ERROR: &str = "backup: backup-nas: Permission denied (publickey).";

/// Everything that belongs to one Core: kept aside for the user while the
/// tour runs.
#[derive(Default)]
pub(super) struct Session {
    core: Option<(Core, Receiver<Event>)>,
    config: Option<Arc<ConfigSnapshot>>,
    data: Option<Arc<AppData>>,
    ssh: Option<Arc<SshInfo>>,
    tunnel_states: Option<Arc<Snapshot>>,
    config_loaded: Option<SystemTime>,
    error: Option<String>,
    view: Option<View>,
    hosts: HostsView,
    tunnels: TunnelsView,
    groups: GroupsView,
    hostkeys: HostKeyModals,
    orphans: OrphansWindow,
}

pub(super) struct TourRun {
    pub tour: Tour,
    sandbox: Sandbox,
    /// The user's session, back when the tour ends.
    user: Session,
    /// Where input may reach the UI (last frame): holes and the popover.
    pub allowed: Vec<Rect>,
    /// The last spotlight seen (kept while a target is late).
    spot: Option<Rect>,
    warned: bool,
}

impl SshelfApp {
    fn swap_session(&mut self, other: &mut Session) {
        std::mem::swap(&mut self.core, &mut other.core);
        std::mem::swap(&mut self.config, &mut other.config);
        std::mem::swap(&mut self.data, &mut other.data);
        std::mem::swap(&mut self.ssh, &mut other.ssh);
        std::mem::swap(&mut self.tunnel_states, &mut other.tunnel_states);
        std::mem::swap(&mut self.config_loaded, &mut other.config_loaded);
        std::mem::swap(&mut self.error, &mut other.error);
        std::mem::swap(&mut self.hosts, &mut other.hosts);
        std::mem::swap(&mut self.tunnels, &mut other.tunnels);
        std::mem::swap(&mut self.groups, &mut other.groups);
        std::mem::swap(&mut self.hostkeys, &mut other.hostkeys);
        std::mem::swap(&mut self.orphans, &mut other.orphans);
        let view = other.view.replace(self.view);
        self.view = view.unwrap_or(View::Hosts);
    }

    pub(super) fn tour_running(&self) -> bool {
        self.tour.is_some()
    }

    /// Starts the tour at `step` (0, or a chapter's first step): builds the
    /// sandbox, starts its Core and puts the user's session aside.
    pub(super) fn start_tour(&mut self, ctx: &Context, step: usize) {
        if self.tour.is_some() {
            return;
        }
        let fake = match self.startup.fake_ssh.clone() {
            Some(fake) => Ok(fake),
            None => FakeSsh::this_program(),
        };
        let reduce_motion = self.data.as_ref().is_some_and(|d| d.settings.reduce_motion);
        let built = fake.and_then(|fake| Sandbox::build(&self.startup.temp, &fake, reduce_motion));
        let sandbox = match built {
            Ok(sandbox) => sandbox,
            Err(e) => {
                self.error = Some(format!("The tour could not start: {e}"));
                return;
            }
        };
        let repaint_ctx = ctx.clone();
        let core = Core::start_with(
            sandbox.paths.clone(),
            Some(SandboxOptions {
                ssh: sandbox.ssh.clone(),
            }),
            Arc::new(move || repaint_ctx.request_repaint()),
        );
        let mut session = Session {
            core: Some(core),
            view: Some(View::Hosts),
            ..Session::default()
        };
        self.swap_session(&mut session);
        self.welcome = false;
        self.close.tour = true;
        self.close.dismiss();
        self.tour = Some(TourRun {
            tour: Tour::new(step),
            sandbox,
            user: session,
            allowed: Vec::new(),
            spot: None,
            warned: false,
        });
        self.replay(step);
        ctx.request_repaint();
    }

    /// Ends the tour any way it ends: the sandbox Core stops (its tunnels
    /// with it) and its folder is deleted, `tour = "done"` is saved, and the
    /// user's hosts open. `welcome`: back to the welcome screen instead (Back
    /// on the first step), nothing saved.
    pub(super) fn end_tour(&mut self, welcome: bool) {
        let Some(mut run) = self.tour.take() else {
            return;
        };
        self.swap_session(&mut run.user);
        // `run.user` now holds the sandbox's session: dropping it stops the
        // sandbox Core, then the sandbox folder goes.
        drop(run);
        self.close.tour = false;
        self.close.dismiss();
        self.view = View::Hosts;
        self.welcome = welcome;
        if !welcome {
            self.save_tour_state(TourState::Done);
        }
    }

    /// Saves `tour` in the user's settings.
    pub(super) fn save_tour_state(&self, state: TourState) {
        let settings = self
            .data
            .as_ref()
            .map(|d| d.settings.clone())
            .unwrap_or_default();
        if settings.tour == Some(state) {
            return;
        }
        self.send(vec![Command::SaveSettings(Settings {
            tour: Some(state),
            ..settings
        })]);
    }

    /// The earlier chapters' actions a chapter needs, done without UI
    /// (SPEC.md: *Start a chapter…*).
    fn replay(&mut self, step: usize) {
        let mut commands = Vec::new();
        if step >= steps::first_step(Chapter::Fixing) {
            // Made in Tunnels; the group editor adds it later.
            commands.push(Command::SaveTunnel(self.web_admin()));
        }
        if step >= steps::first_step(Chapter::Window) {
            // Closing asks only while tunnels run.
            commands.push(Command::StartTunnel(TOUR_DB.into()));
        }
        self.send(commands);
    }

    /// The tunnel the tour makes: web admin, 18443 → localhost:443 on
    /// staging-api (its port moved when busy here).
    fn web_admin(&self) -> Tunnel {
        let port = self
            .tour
            .as_ref()
            .map_or(SAMPLE_PORTS[4], |run| run.sandbox.port(SAMPLE_PORTS[4]));
        Tunnel {
            id: TOUR_WEB_ADMIN.into(),
            name: "web admin".into(),
            host: "staging-api".into(),
            forwards: vec![Forward {
                kind: ForwardKind::L,
                bind: DEFAULT_BIND.into(),
                listen: port,
                target: Some("localhost:443".into()),
            }],
            auto_reconnect: false,
        }
    }

    /// An event from the sandbox Core the tour learns from.
    pub(super) fn tour_event(&mut self, event: &Event) {
        let Some(run) = &mut self.tour else {
            return;
        };
        match event {
            Event::TourWouldRun { command } if command.starts_with("ssh ") => {
                run.tour.seen.connected = true;
            }
            Event::EditSaved { .. } => run.tour.seen.saved_edit = true,
            // The sandbox's *Install on server* went through fake-ssh.
            Event::Notice(text) if text.starts_with("fake-ssh: key added") => {
                run.tour.seen.install_requested = true;
            }
            _ => {}
        }
    }

    /// What the close dialog reported in the tour.
    pub(super) fn tour_close_choice(&mut self) {
        let Some(choice) = self.close.take_tour_choice() else {
            return;
        };
        let text = match choice {
            CloseBehavior::MinimizeToTray => {
                if let Some(run) = &mut self.tour {
                    run.tour.seen.minimized = true;
                }
                "Tour: would hide the window; the tunnels keep running in the tray"
            }
            _ => "Tour: would stop the tunnels and quit",
        };
        self.toasts.push(text);
    }

    /// Each frame before the views: enter a new step (its screen), and move
    /// on when its action got done.
    pub(super) fn tour_step(&mut self) {
        let Some(run) = &self.tour else {
            return;
        };
        if run.tour.enter
            && self.apply_screen()
            && let Some(run) = &mut self.tour
        {
            run.tour.enter = false;
        }
        let Some(run) = &self.tour else {
            return;
        };
        let step = run.tour.current();
        let Some((action, _)) = step.action else {
            return;
        };
        let (done, _) = self.progress(action);
        if let Some(run) = &mut self.tour
            && run.tour.advances(done)
            && !run.tour.is_last()
        {
            let next = run.tour.step + 1;
            run.tour.go(next);
        }
    }

    /// Whether `action` is done, and whether it is running. (*Install on
    /// server* counts as running from the key's arrival on the fake server
    /// until backup runs again.)
    fn progress(&self, action: Action) -> (bool, bool) {
        let seen = self.tour.as_ref().map(|r| r.tour.seen).unwrap_or_default();
        let states = self.tunnel_states.as_deref();
        let tunnel = |id: &str| states.map_or(TunnelState::Stopped, |s| s.state(id));
        let data = self.data.as_deref();
        match action {
            Action::Connect => (seen.connected, false),
            Action::CheckAll => {
                let health = self.hosts.health();
                let checking = health.is_some_and(|h| !h.checking.is_empty());
                let all = health.is_some_and(|h| {
                    self.config
                        .as_deref()
                        .and_then(|c| c.hosts())
                        .is_some_and(|hosts| {
                            hosts
                                .hosts
                                .iter()
                                .all(|e| h.reports.contains_key(e.primary()))
                        })
                });
                (all && !checking, checking)
            }
            Action::EffectiveConfig => (self.hosts.detail.on_effective_config(), false),
            Action::Edit => (self.hosts.editor.is_open(), false),
            Action::RawText => (self.hosts.editor.is_raw(), false),
            Action::SaveEdit => (seen.saved_edit, false),
            Action::OpenTunnels => (self.view == View::Tunnels, false),
            Action::StartDbTunnel => {
                let state = tunnel(TOUR_DB);
                (
                    state == TunnelState::Running,
                    state == TunnelState::Starting,
                )
            }
            Action::NewTunnel => (self.tunnels.is_editing(), false),
            Action::SaveTunnel => (
                data.is_some_and(|d| d.tunnels.tunnel(TOUR_WEB_ADMIN).is_some()),
                false,
            ),
            Action::StartBackup => {
                let state = tunnel(TOUR_BACKUP);
                (
                    matches!(state, TunnelState::Failed(_)),
                    state == TunnelState::Starting,
                )
            }
            Action::OpenKeygen => (self.hosts.editor.key_window().is_some(), false),
            Action::GenerateKey => (self.hosts.editor.key_window() == Some(true), false),
            Action::InstallKey => {
                let running = tunnel(TOUR_BACKUP) == TunnelState::Running;
                (
                    seen.install_requested && running,
                    seen.install_requested && !running,
                )
            }
            Action::OpenGroups => (self.view == View::Groups, false),
            Action::StartGroup => {
                let state = data
                    .and_then(|d| d.tunnels.group(TOUR_STAGING_GROUP))
                    .zip(states)
                    .map_or(GroupState::Stopped, |(g, s)| s.group_state(g));
                (
                    matches!(state, GroupState::Running | GroupState::Degraded { .. }),
                    state == GroupState::Starting,
                )
            }
            Action::EditGroup => (self.groups.is_editing(), false),
            Action::SaveGroup => (
                data.and_then(|d| d.tunnels.group(TOUR_STAGING_GROUP))
                    .is_some_and(|g| {
                        g.start_on_launch && g.tunnels.iter().any(|t| t == TOUR_WEB_ADMIN)
                    }),
                false,
            ),
            Action::Close => (self.close.is_asking(), false),
            Action::MinimizeToTray => (seen.minimized, false),
        }
    }

    /// Puts the screen the current step needs in place (also after Back);
    /// `false` while that can't happen yet (the sample is loading).
    fn apply_screen(&mut self) -> bool {
        let Some(run) = &self.tour else {
            return true;
        };
        let index = run.tour.step;
        let screen = run.tour.current().screen;
        let Some(config) = self.config.clone() else {
            return false;
        };
        if config.hosts().is_none() {
            return false;
        }
        let close_dialogs = |app: &mut Self| {
            app.hosts.editor.close_keys();
            app.close.dismiss();
        };
        match screen {
            Screen::Hosts | Screen::HostsKeep => {
                self.view = View::Hosts;
                self.hosts.select(TOUR_HOST);
                if self.hosts.editor.is_open() {
                    self.hosts.editor.close();
                }
                if screen == Screen::Hosts {
                    self.hosts.detail.show_overview();
                }
                close_dialogs(self);
            }
            Screen::HostEditor | Screen::HostEditorKeep => {
                self.view = View::Hosts;
                self.hosts.select(TOUR_HOST);
                if !self.hosts.editor.is_open() {
                    self.hosts.editor.open(&config, TOUR_HOST);
                }
                if screen == Screen::HostEditor {
                    self.hosts.editor.show_form();
                }
                // "We changed User to admin for you."
                self.hosts.editor.set_value("User", "admin");
                close_dialogs(self);
            }
            Screen::Tunnels => {
                self.view = View::Tunnels;
                self.tunnels.close_editor();
                self.tunnels.select(TOUR_DB);
                close_dialogs(self);
            }
            Screen::TunnelEditor => {
                self.view = View::Tunnels;
                if !self.tunnels.is_editing()
                    && let Some(data) = &self.data
                {
                    self.tunnels.new_tunnel("", &data.tunnels);
                }
                let web_admin = self.web_admin();
                if let Some(draft) = self.tunnels.draft_mut()
                    && draft.name.is_empty()
                {
                    *draft = web_admin;
                }
                close_dialogs(self);
            }
            Screen::Backup => {
                self.view = View::Tunnels;
                self.tunnels.close_editor();
                self.tunnels.select(TOUR_BACKUP);
                close_dialogs(self);
            }
            Screen::Keygen | Screen::KeyResult => {
                self.view = View::Tunnels;
                self.tunnels.close_editor();
                self.tunnels.select(TOUR_BACKUP);
                self.close.dismiss();
                if self.hosts.editor.key_window().is_none() {
                    self.hosts.editor.open_keys(&config, TOUR_BACKUP_HOST);
                }
                self.hosts.editor.prefill_key_passphrase(TOUR_PASSPHRASE);
            }
            Screen::Groups => {
                self.view = View::Groups;
                self.groups.close_editor();
                self.groups.select(TOUR_STAGING_GROUP);
                close_dialogs(self);
                // "Edit a group": the editor only opens on a stopped group.
                if STEPS[index].action.map(|(a, _)| a) == Some(Action::EditGroup) {
                    let (running, _) = self.progress(Action::StartGroup);
                    if running {
                        self.send(vec![Command::StopGroup(TOUR_STAGING_GROUP.into())]);
                    }
                }
            }
            Screen::GroupEditor => {
                self.view = View::Groups;
                close_dialogs(self);
                if !self.groups.is_editing()
                    && let Some(group) = self
                        .data
                        .as_ref()
                        .and_then(|d| d.tunnels.group(TOUR_STAGING_GROUP))
                        .cloned()
                {
                    self.groups.edit(group);
                }
                let has_web_admin = self
                    .data
                    .as_ref()
                    .is_some_and(|d| d.tunnels.tunnel(TOUR_WEB_ADMIN).is_some());
                if let Some(draft) = self.groups.draft_mut() {
                    if has_web_admin && !draft.tunnels.iter().any(|t| t == TOUR_WEB_ADMIN) {
                        draft.tunnels.push(TOUR_WEB_ADMIN.into());
                    }
                    draft.start_on_launch = true;
                }
            }
            Screen::Any => {
                close_dialogs(self);
                // The status bar step shows backup's error from before.
                if STEPS[index].targets == [targets::Target::StatusBar] && self.error.is_none() {
                    self.error = Some(BACKUP_ERROR.into());
                }
            }
            Screen::CloseDialog => {
                self.hosts.editor.close_keys();
                if !self.close.is_asking() {
                    self.close.ask();
                }
            }
        }
        true
    }

    /// Draws the overlay over everything and acts on the popover; call last
    /// in `ui`.
    pub(super) fn tour_overlay(&mut self, ctx: &Context) {
        let Some(run) = &self.tour else {
            return;
        };
        let step = run.tour.current();
        let index = run.tour.step;
        // The spotlight: the targets' union; a late one is waited for.
        let found: Vec<Rect> = step
            .targets
            .iter()
            .filter_map(|t| targets::rect(ctx, *t))
            .collect();
        // Targets that come and go (the failure banner before the failure)
        // join the spotlight once there; it is late only with none on screen.
        let missing = found.is_empty() && !step.targets.is_empty();
        let mut spot = found.into_iter().reduce(|a, b| a.union(b));
        let extra: Vec<Rect> = step
            .extra
            .iter()
            .filter_map(|t| targets::rect(ctx, *t))
            .collect();
        let (pending, busy) = match step.action {
            Some((action, _)) => {
                let (done, busy) = self.progress(action);
                (!done, busy)
            }
            None => (false, false),
        };
        let glint = step
            .action
            .filter(|_| pending && !busy)
            .and_then(|(action, _)| targets::rect(ctx, tour::click_target(action)));
        let Some(run) = &mut self.tour else {
            return;
        };
        if missing {
            run.tour.missing += 1;
            if run.tour.missing <= LATE_FRAMES {
                spot = run.spot;
                ctx.request_repaint();
            } else if !run.warned {
                run.warned = true;
                eprintln!(
                    "sshelf tour: step {} (\u{201c}{}\u{201d}) has no target on screen",
                    index + 1,
                    step.title
                );
            }
        } else {
            run.tour.missing = 0;
            run.warned = false;
        }
        run.spot = spot;
        let tray = self.tray_model.clone();
        let view = overlay::View {
            step,
            index,
            total: STEPS.len(),
            spot,
            extra,
            glint,
            pending,
            busy,
            tray: &tray,
        };
        let (clicked, allowed) = overlay::show(ctx, &view);
        if let Some(run) = &mut self.tour {
            run.allowed = allowed;
        }
        match clicked {
            Some(overlay::Clicked::Skip) => self.end_tour(false),
            Some(overlay::Clicked::Back) => {
                if index == 0 {
                    self.end_tour(true);
                } else if let Some(run) = &mut self.tour {
                    run.tour.go(index - 1);
                }
            }
            Some(overlay::Clicked::Primary) => {
                let last = self.tour.as_ref().is_some_and(|r| r.tour.is_last());
                if last {
                    self.end_tour(false);
                } else if pending {
                    // *Show me*: the step's own button, clicked.
                    if let Some((action, _)) = step.action {
                        targets::click(ctx, tour::click_target(action));
                    }
                } else if let Some(run) = &mut self.tour {
                    run.tour.go(index + 1);
                }
            }
            None => {}
        }
        // `Esc` ends the tour (once nothing else took it).
        if self.tour.is_some()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.end_tour(false);
        }
    }

    /// A close request while the tour runs: on its close step the dialog
    /// asks (as the tour); anywhere else the tour ends first, then the
    /// normal close applies to the user's Core. Returns whether it was
    /// handled.
    pub(super) fn tour_close_request(&mut self, ctx: &Context) -> bool {
        if self.tour.is_none() || !ctx.input(|i| i.viewport().close_requested()) {
            return false;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.tour_close(ctx);
        true
    }

    /// Closing during the tour, from the window's *Close* or the OS.
    pub(super) fn tour_close(&mut self, ctx: &Context) {
        let on_close_step = self
            .tour
            .as_ref()
            .is_some_and(|r| r.tour.current().action.map(|(a, _)| a) == Some(Action::Close));
        if on_close_step {
            self.close.ask();
        } else {
            self.end_tour(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Drops pointer input outside `allowed` (and the drag strip, which has
/// *Exit tour*): the overlay lets clicks and hovers through its holes only.
pub(super) fn filter_input(raw: &mut egui::RawInput, allowed: &[Rect], strip: f32) {
    let inside = |pos: egui::Pos2| pos.y <= strip || allowed.iter().any(|r| r.contains(pos));
    raw.events.retain_mut(|event| match event {
        egui::Event::PointerMoved(pos) => {
            if !inside(*pos) {
                *event = egui::Event::PointerGone;
            }
            true
        }
        egui::Event::PointerButton { pos, .. } => inside(*pos),
        egui::Event::MouseWheel { .. } => true,
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshelf_core::paths::Paths;
    use sshelf_core::ssh::ToolSource;

    use super::*;
    use crate::app::Startup;
    use crate::test_util;

    /// One tour at a time: two sandboxes would race for the same sample
    /// ports (the app only ever runs one).
    static ONE_TOUR: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn one_tour() -> std::sync::MutexGuard<'static, ()> {
        ONE_TOUR.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The workspace's fake-ssh binary, beside this test binary's folder
    /// (`target/<profile>/deps/..`): `cargo test --workspace` builds it.
    fn fake_ssh_binary() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(Path::parent).unwrap();
        let path = dir.join(format!("fake-ssh{}", std::env::consts::EXE_SUFFIX));
        assert!(
            path.is_file(),
            "the tour test runs the fake-ssh binary; build it first \
             (`cargo test --workspace` does): {}",
            path.display()
        );
        path
    }

    /// Every file under `dir` with its bytes.
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        files
    }

    /// Steps the app until `done` (real Cores run in the background).
    fn settle(harness: &mut Harness<'_, SshelfApp>, what: &str, done: impl Fn(&SshelfApp) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(harness.state()) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what} (close dialog open: {})",
                harness.state().close.is_asking()
            );
            harness.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn step_index(app: &SshelfApp) -> Option<usize> {
        app.tour.as_ref().map(|r| r.tour.step)
    }

    /// The popover's primary button, if enabled.
    fn primary(harness: &Harness<'_, SshelfApp>, label: &str) -> bool {
        harness
            .query_by_label(label)
            .is_some_and(|n| !n.accesskit_node().is_disabled())
    }

    /// The whole tour, headless, with *Show me* on every step: each step's
    /// targets are on screen, and nothing outside the sandbox is touched —
    /// the user's folder (config, app data) is left byte for byte, except
    /// `tour = "done"` at the end; the user's Core has no ssh at all, so
    /// only the sandbox's fake-ssh can run; the sandbox is deleted after.
    #[test]
    fn the_whole_tour_plays_in_the_sandbox_and_touches_nothing_else() {
        let _one = one_tour();
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let paths = Paths::new(&user.join("home"), user.join("app"));
        fs::create_dir_all(&paths.ssh_dir).unwrap();
        fs::create_dir_all(&paths.app_dir).unwrap();
        fs::write(
            &paths.ssh_config,
            "Host mine\n    HostName mine.example.com\n",
        )
        .unwrap();
        fs::write(
            paths.ssh_dir.join("known_hosts"),
            "mine.example.com ssh-ed25519 AAAA\n",
        )
        .unwrap();
        let missing_ssh = user.join("no-ssh-here");
        fs::write(
            paths.app_dir.join("settings.toml"),
            format!(
                "version = 1\nssh_path = '{}'\nreduce_motion = true\ntour = \"offered\"\n",
                missing_ssh.display()
            ),
        )
        .unwrap();
        let before = snapshot(&user);
        let temp = root.path().join("temp");
        fs::create_dir_all(&temp).unwrap();

        let startup = Startup {
            tray: false,
            fake_ssh: Some(FakeSsh {
                program: fake_ssh_binary(),
                prefix: vec![fake_ssh::EMBEDDED_FLAG.into()],
            }),
            temp: temp.clone(),
            user_paths: Some(paths.clone()),
        };
        let mut harness = test_util::harness()
            .with_size([1280.0, 800.0])
            .build_eframe(|cc| SshelfApp::with(cc, startup));
        settle(&mut harness, "the user's data", |app| app.data.is_some());

        let ctx = harness.ctx.clone();
        harness.state_mut().start_tour(&ctx, 0);
        play(&mut harness, 0);
        settle(&mut harness, "tour = done", |app| {
            app.data
                .as_ref()
                .is_some_and(|d| d.settings.tour == Some(TourState::Done))
        });
        drop(harness);

        // The sandbox is gone.
        let left: Vec<_> = fs::read_dir(&temp).unwrap().flatten().collect();
        assert!(left.is_empty(), "left behind: {left:?}");
        // The user's files: all as they were, settings.toml aside.
        let mut after = snapshot(&user);
        let settings = paths.app_dir.join("settings.toml");
        let saved = String::from_utf8(after.remove(&settings).unwrap()).unwrap();
        assert!(saved.contains("tour = \"done\""), "{saved}");
        let mut before = before;
        before.remove(&settings);
        assert_eq!(before, after);
    }

    /// An app on a user folder in `root` whose Core has no ssh, and the
    /// fake-ssh binary for the sandbox.
    fn app_in(root: &Path) -> Harness<'static, SshelfApp> {
        let paths = Paths::new(&root.join("home"), root.join("app"));
        fs::create_dir_all(&paths.ssh_dir).unwrap();
        fs::create_dir_all(&paths.app_dir).unwrap();
        fs::write(
            paths.app_dir.join("settings.toml"),
            format!(
                "version = 1\nssh_path = '{}'\nreduce_motion = true\ntour = \"done\"\n",
                root.join("no-ssh-here").display()
            ),
        )
        .unwrap();
        let temp = root.join("temp");
        fs::create_dir_all(&temp).unwrap();
        let startup = Startup {
            tray: false,
            fake_ssh: Some(FakeSsh {
                program: fake_ssh_binary(),
                prefix: vec![fake_ssh::EMBEDDED_FLAG.into()],
            }),
            temp,
            user_paths: Some(paths),
        };
        let mut harness = test_util::harness()
            .with_size([1280.0, 800.0])
            .build_eframe(|cc| SshelfApp::with(cc, startup));
        settle(&mut harness, "the user's data", |app| app.data.is_some());
        harness
    }

    /// Each chapter from Settings › Help plays to the end on its replayed
    /// prerequisites.
    #[test]
    fn every_chapter_plays_from_its_first_step() {
        let _one = one_tour();
        let root = tempfile::tempdir().unwrap();
        let mut harness = app_in(root.path());
        // One through the Help card itself (tall enough to show it without
        // scrolling).
        harness.set_size(egui::vec2(1280.0, 1200.0));
        harness.state_mut().view = View::Settings;
        harness.run_steps(3);
        harness.get_by_label("Start a chapter").click();
        harness.run_steps(3);
        harness.get_by_label("Fixing problems").click();
        harness.run_steps(3);
        let fixing = steps::first_step(Chapter::Fixing);
        assert_eq!(step_index(harness.state()), Some(fixing));
        harness.set_size(egui::vec2(1280.0, 800.0));
        play(&mut harness, fixing);
        let ctx = harness.ctx.clone();
        for chapter in Chapter::PLAYABLE.into_iter().skip(1) {
            if chapter == Chapter::Fixing {
                continue;
            }
            let first = steps::first_step(chapter);
            harness.state_mut().start_tour(&ctx, first);
            play(&mut harness, first);
        }
        drop(harness);
        let left: Vec<_> = fs::read_dir(root.path().join("temp"))
            .unwrap()
            .flatten()
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    /// Plays a started tour from step `from` to its end with *Show me* on
    /// every step, checking each step's targets are on screen.
    fn play(harness: &mut Harness<'_, SshelfApp>, from: usize) {
        let ctx = harness.ctx.clone();
        settle(harness, "the sandbox", |app| {
            app.ssh.as_ref().is_some_and(|s| {
                s.tools
                    .as_ref()
                    .is_ok_and(|t| t.source == ToolSource::Sandbox)
            }) && app.config.as_ref().is_some_and(|c| c.hosts().is_some())
        });

        for (index, step) in STEPS.iter().enumerate().skip(from) {
            assert_eq!(step_index(harness.state()), Some(index), "{}", step.title);
            // The step's screen, then its targets on screen.
            for _ in 0..5 {
                harness.step();
            }
            settle(harness, step.title, |_| {
                step.targets.is_empty()
                    || step
                        .targets
                        .iter()
                        .any(|t| targets::rect(&ctx, *t).is_some())
            });
            let mut shown = false;
            let mut since_shown = 0;
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                harness.step();
                if step_index(harness.state()) != Some(index) {
                    break;
                }
                if Instant::now() >= deadline {
                    let app = harness.state();
                    let run = app.tour.as_ref().unwrap();
                    panic!(
                        "stuck on step {}: {} (enter {}, done_on_enter {:?}, progress {:?}, \
                         shown {shown}, editor {}, error {:?})",
                        index + 1,
                        step.title,
                        run.tour.enter,
                        run.tour.done_on_enter,
                        step.action.map(|(a, _)| app.progress(a)),
                        app.hosts.editor.is_open(),
                        app.error,
                    );
                }
                if primary(harness, "Open my hosts") {
                    harness.get_by_label("Open my hosts").click();
                } else if primary(harness, "Next") {
                    harness.get_by_label("Next").click();
                } else if (!shown || since_shown > 100) && primary(harness, "Show me") {
                    // Again after a while: kittest presses and releases in
                    // one frame, which a popover that just moved can miss.
                    harness.get_by_label("Show me").click();
                    shown = true;
                    since_shown = 0;
                }
                since_shown += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        for _ in 0..5 {
            harness.step();
        }
        assert!(harness.state().tour.is_none(), "the tour ended");
        assert_eq!(harness.state().view, View::Hosts);
    }

    #[test]
    fn input_outside_the_holes_is_dropped() {
        let hole = Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(50.0, 50.0));
        let mut raw = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(egui::pos2(10.0, 300.0)),
                egui::Event::PointerButton {
                    pos: egui::pos2(10.0, 300.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: egui::pos2(120.0, 120.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                // The drag strip stays usable.
                egui::Event::PointerButton {
                    pos: egui::pos2(400.0, 20.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        filter_input(&mut raw, &[hole], 44.0);
        assert_eq!(raw.events.len(), 3);
        assert_eq!(raw.events[0], egui::Event::PointerGone);
    }
}
