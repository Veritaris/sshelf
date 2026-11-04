//! Top-level eframe app: applies core events and lays out the sidebar, the
//! current view, the status bar (errors) and toasts (successes). All side
//! effects go through the core.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use sshelf_core::core::{AppData, Command, ConfigSnapshot, Core, Event, ParentWindow, SshInfo};
use sshelf_core::paths::Paths;
use sshelf_core::settings;
use sshelf_core::supervisor::Snapshot;

use crate::close_guard::{self, CloseGuard};
use crate::fonts;
use crate::groups_view::GroupsView;
use crate::hostkey_modal::HostKeyModals;
use crate::hosts_view::{HostsView, Navigate};
use crate::icon::{self, Status};
use crate::orphans_window::{self, OrphansWindow};
use crate::privacy::{self, Privacy};
use crate::settings_view::SettingsView;
use crate::sidebar::{self, View};
use crate::theme::{self, Palette};
use crate::toast::Toasts;
use crate::tour::sandbox::{self, FakeSsh};
use crate::tour::{steps, welcome};
use crate::tray::{self, Tray, TrayAction, TrayModel};
use crate::tray_menu::TrayMenu;
use crate::tunnels_view::{KeyFix, TunnelsView};
use crate::{motion, status_bar, window};

#[cfg(test)]
mod leak;
mod tour_run;

/// How the app starts: the real one, or a test's.
pub struct Startup {
    /// Create the tray icon.
    pub tray: bool,
    /// How the tour runs fake-ssh; `None`: this program (`--fake-ssh`).
    pub fake_ssh: Option<FakeSsh>,
    /// Where the tour's sandbox goes (`<temp>/sshelf-tour-<pid>`).
    pub temp: std::path::PathBuf,
    /// The user's Core runs on these paths; `None`: detected.
    pub user_paths: Option<Paths>,
    /// "Show the window" requests from later starts (single instance);
    /// `None` in tests, which never take the instance lock.
    pub show: Option<Arc<ShowRequests>>,
}

impl Startup {
    pub fn real(show: Arc<ShowRequests>) -> Self {
        Self {
            tray: true,
            fake_ssh: None,
            temp: std::env::temp_dir(),
            user_paths: None,
            show: Some(show),
        }
    }
}

/// Requests to show the window, from later starts of sshelf
/// (`sshelf_core::instance`): set on the instance's listener thread, carried
/// out in `logic`, which also runs while the window is hidden in the tray.
#[derive(Default)]
pub struct ShowRequests {
    pending: AtomicBool,
    /// Set once the app exists; a request before that waits in `pending`.
    ctx: OnceLock<egui::Context>,
}

impl ShowRequests {
    /// Called from the listener thread.
    pub fn request(&self) {
        self.pending.store(true, Ordering::SeqCst);
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::SeqCst)
    }
}

pub struct SshelfApp {
    core: Option<(Core, Receiver<Event>)>,
    startup_error: Option<String>,
    view: View,
    config: Option<Arc<ConfigSnapshot>>,
    data: Option<Arc<AppData>>,
    ssh: Option<Arc<SshInfo>>,
    tunnel_states: Option<Arc<Snapshot>>,
    hosts: HostsView,
    tunnels: TunnelsView,
    groups: GroupsView,
    settings: SettingsView,
    orphans: OrphansWindow,
    hostkeys: HostKeyModals,
    close: CloseGuard,
    /// `None` when the platform could not create one.
    tray: Option<Tray>,
    /// What both tray menus show, from the last `logic`.
    tray_model: TrayModel,
    /// The sshelf tray menu (right click).
    tray_menu: TrayMenu,
    /// The status dot the window's icon (taskbar button) shows now.
    window_icon: Status,
    /// The last error, in the status bar until dismissed.
    error: Option<String>,
    /// Successes, briefly.
    toasts: Toasts,
    /// When the last config snapshot arrived (the status bar shows it).
    config_loaded: Option<SystemTime>,
    /// The window system dialogs belong to (modal to it).
    parent: Option<ParentWindow>,
    startup: Startup,
    /// The onboarding tour while it runs.
    tour: Option<tour_run::TourRun>,
    /// The welcome screen shows even though `tour` is set (Back from the
    /// tour's first step).
    welcome: bool,
}

/// The root window's raw handles, for dialogs that must be modal to it.
fn parent_window(cc: &eframe::CreationContext<'_>) -> Option<ParentWindow> {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
    Some(ParentWindow {
        window: cc.window_handle().ok()?.as_raw(),
        display: cc.display_handle().ok()?.as_raw(),
    })
}

impl SshelfApp {
    pub fn new(cc: &eframe::CreationContext<'_>, show: Arc<ShowRequests>) -> Self {
        Self::with(cc, Startup::real(show))
    }

    pub fn with(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        #[cfg(target_os = "windows")]
        window::native::prepare(cc);
        let ctx = cc.egui_ctx.clone();
        if let Some(show) = &startup.show {
            // From now on a later start's request wakes the app.
            let _ = show.ctx.set(ctx.clone());
        }
        // A crashed run's tour sandbox goes now.
        sandbox::clean_leftovers(&startup.temp);
        // The event loop runs by now, as macOS requires for tray icons.
        let (tray, tray_error) = if startup.tray {
            match Tray::create(&cc.egui_ctx) {
                Ok(tray) => (Some(tray), None),
                Err(e) => (None, Some(format!("No tray icon: {e}"))),
            }
        } else {
            (None, None)
        };
        let paths = startup.user_paths.clone().or_else(Paths::detect);
        let (core, startup_error) = match paths {
            Some(paths) => (
                Some(Core::start(paths, Arc::new(move || ctx.request_repaint()))),
                None,
            ),
            None => (
                None,
                Some("Cannot determine your home directory.".to_string()),
            ),
        };
        Self {
            core,
            startup_error,
            view: View::Hosts,
            config: None,
            data: None,
            ssh: None,
            tunnel_states: None,
            hosts: HostsView::default(),
            tunnels: TunnelsView::default(),
            groups: GroupsView::default(),
            settings: SettingsView::default(),
            orphans: OrphansWindow::default(),
            hostkeys: HostKeyModals::default(),
            close: CloseGuard::default(),
            tray,
            tray_model: TrayModel::default(),
            tray_menu: TrayMenu::default(),
            window_icon: Status::Idle,
            error: tray_error,
            toasts: Toasts::default(),
            config_loaded: None,
            parent: parent_window(cc),
            startup,
            tour: None,
            welcome: false,
        }
    }

    /// Carries out tray clicks and keeps the tray menus current.
    fn run_tray(&mut self, ctx: &egui::Context) {
        let actions = self
            .tray
            .as_mut()
            .map(|tray| tray.actions())
            .unwrap_or_default();
        for action in actions {
            self.tray_action(ctx, action);
        }
        self.tray_model = tray::model(
            self.data.as_deref(),
            self.config.as_deref().and_then(|c| c.hosts()),
            self.tunnel_states.as_deref(),
            self.hostkeys.waiting(),
        );
        // During the tour the tray mirrors the sandbox, under "Tour".
        self.tray_model.tour = self.tour_running();
        // Streamer mode is the user's, during the tour too.
        self.tray_model.streamer = privacy::get(ctx).on;
        // The taskbar button / Dock icon carries the same status dot as the
        // tray icon. Linux: X11 taskbars read the window icon; Wayland has
        // no window icons (its docks use the desktop entry's).
        if self.tray_model.status != self.window_icon {
            self.window_icon = self.tray_model.status;
            let icon = icon::egui_icon(self.window_icon);
            // Windows: the taskbar shows the big icon, which the command
            // below leaves alone.
            #[cfg(target_os = "windows")]
            window::native::set_taskbar_icon(&icon.rgba, icon.width);
            #[cfg(target_os = "macos")]
            window::macos::set_dock_icon(self.window_icon);
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Icon(Some(Arc::new(icon))),
            );
        }
        // The sshelf tray menu's clicks, carried out here: `ui` may not run.
        let model = self.tray_model.clone();
        for action in self.tray_menu.poll(ctx, &model) {
            self.tray_action(ctx, action);
        }
        // Linux trays (KSNI) report no icon position: always the system menu.
        let native = cfg!(target_os = "linux")
            || self
                .data
                .as_ref()
                .is_some_and(|d| d.settings.tray_menu == settings::TrayMenu::System);
        let style = self
            .data
            .as_ref()
            .map(|d| d.settings.tray_icon)
            .unwrap_or_default();
        if let Some(tray) = &mut self.tray {
            tray.update(self.tray_model.clone(), native, style);
        }
    }

    /// A click in either tray menu.
    fn tray_action(&mut self, ctx: &egui::Context, action: TrayAction) {
        match action {
            TrayAction::Show => close_guard::show(ctx),
            // The same close request as the window's (SPEC.md §10).
            TrayAction::Quit => self.close.quit_from_tray(ctx),
            TrayAction::OpenMenu(icon) => self.tray_menu.open(ctx, icon),
            TrayAction::ShowTunnel(id) => {
                close_guard::show(ctx);
                self.view = View::Tunnels;
                self.tunnels.select(&id);
            }
            TrayAction::SetStreamer(on) => {
                let hide = self
                    .user_settings()
                    .is_some_and(|s| s.streamer_hide_aliases);
                self.set_streamer(on, hide);
            }
            other => self.send(other.command().into_iter().collect()),
        }
    }

    fn apply_events(&mut self, ctx: &egui::Context) {
        let Some((_, events)) = &self.core else {
            return;
        };
        let events: Vec<Event> = events.try_iter().collect();
        for event in events {
            self.tour_event(&event);
            match event {
                Event::Config(snapshot) => {
                    self.hosts.on_config(&snapshot);
                    self.config = Some(snapshot);
                    self.config_loaded = Some(SystemTime::now());
                }
                Event::Data(data) => self.data = Some(data),
                // Streamer mode isn't drawn yet (M11 step 2).
                Event::Redactor(_) => {}
                Event::Resolved {
                    alias,
                    generation,
                    result,
                } => self.hosts.detail.on_resolved(alias, generation, result),
                Event::Ssh(info) => self.ssh = Some(info),
                Event::Launched { alias } => {
                    self.toasts.push_value("Opened a terminal for", alias);
                }
                Event::Notice(text) => self.toasts.push(text),
                Event::TourWouldRun { command } => {
                    self.toasts
                        .push(format!("Tour: would run `{command}` in your terminal"));
                }
                Event::Health(health) => self.hosts.on_health(health),
                Event::Route {
                    alias,
                    generation,
                    result,
                } => self.hosts.detail.on_route(alias, generation, result),
                Event::KeyGenerated { id, result } => {
                    self.hosts.editor.on_key_generated(id, result);
                }
                Event::Keys(keys) => self.hosts.editor.on_keys(keys),
                Event::KeyFilePicked { id, path } => self.hosts.editor.on_key_file(id, path),
                Event::Error(message) => self.error = Some(message),
                Event::Tunnels(states) => self.tunnel_states = Some(states),
                // A start from the tray may need an answer: show the window.
                Event::HostKeyPrompt(prompt) => {
                    self.hostkeys.on_unknown(prompt);
                    close_guard::show(ctx);
                }
                Event::HostKeyChanged(prompt) => {
                    self.hostkeys.on_changed(prompt);
                    close_guard::show(ctx);
                }
                Event::HostKeyClosed(id) => self.hostkeys.on_closed(id),
                Event::EditChecked { id, result } => self.hosts.editor.on_checked(id, result),
                Event::EditSaved { id } => self.hosts.editor.on_saved(id),
                Event::EditConflict { id, disk } => self.hosts.editor.on_conflict(id, &disk),
            }
        }
        if let Some((error, text)) = self.hosts.editor.take_status() {
            if error {
                self.error = Some(text);
            } else {
                self.toasts.push(text);
            }
        }
    }

    fn send(&self, commands: Vec<Command>) {
        if let Some((core, _)) = &self.core {
            for command in commands {
                // Views don't know the window; a file dialog is modal to it.
                let command = match command {
                    Command::PickKeyFile { id, parent: None } => Command::PickKeyFile {
                        id,
                        parent: self.parent,
                    },
                    other => other,
                };
                core.send(command);
            }
        }
    }

    /// The window's drag strip (SPEC.md §10), above everything but the sidebar.
    fn drag_strip(&mut self, ui: &mut egui::Ui) {
        let tour = self.tour_running();
        let settings = self.user_settings();
        let streamer = settings
            .filter(|s| s.streamer_mode)
            .map(|s| window::Streamer {
                hide_aliases: s.streamer_hide_aliases,
            });
        let action = egui::Panel::top("drag_strip")
            .resizable(false)
            .exact_size(window::STRIP_HEIGHT)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 16,
                right: 8,
                top: 0,
                bottom: 0,
            }))
            .show(ui, |ui| window::strip(ui, "sshelf", tour, streamer))
            .inner;
        match action {
            Some(window::StripAction::ExitTour) => self.end_tour(false),
            Some(window::StripAction::HideAliases(hide)) => self.set_streamer(true, hide),
            Some(window::StripAction::StreamerOff) => {
                let hide = streamer.is_some_and(|s| s.hide_aliases);
                self.set_streamer(false, hide);
            }
            Some(window::StripAction::Close) if tour => self.tour_close(ui.ctx()),
            Some(action) => window::apply(ui.ctx(), action),
            None => {}
        }
    }

    /// The status bar, along the whole bottom of the window (under the rail).
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let (running, failed) = status_bar::tunnel_counts(self.tunnel_states.as_deref());
        let status = status_bar::Status {
            running,
            failed,
            ssh: self.ssh.as_deref(),
            config_loaded: self.config_loaded,
        };
        egui::Panel::bottom("status")
            .resizable(false)
            .exact_size(status_bar::height(self.error.is_some()))
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| status_bar::show(ui, &status, &mut self.error));
    }

    /// The icon rail; returns `true` when the orphan indicator was clicked.
    fn rail(&mut self, ui: &mut egui::Ui) -> bool {
        let orphans = self
            .data
            .as_deref()
            .map(orphans_window::descriptions)
            .unwrap_or_default();
        let groups = self
            .data
            .as_deref()
            .map(|d| d.tunnels.groups.as_slice())
            .unwrap_or_default();
        let rail = sidebar::Rail {
            orphans: &orphans,
            running: status_bar::tunnel_counts(self.tunnel_states.as_deref()).0,
            status: Status::of(self.tunnel_states.as_deref(), groups),
            settings_off: self
                .tour_running()
                .then_some("Not available during the tour"),
        };
        egui::Panel::left("rail")
            .resizable(false)
            .exact_size(sidebar::WIDTH)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| sidebar::show(ui, &mut self.view, &rail))
            .inner
    }
}

impl eframe::App for SshelfApp {
    /// Runs every frame, and also while the window is hidden: core events and
    /// the tray are handled here, the drawing in [`Self::ui`].
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_events(ctx);
        // Streamer mode for this frame's drawing, from the user's settings.
        privacy::set(
            ctx,
            Privacy {
                on: self.user_settings().is_some_and(|s| s.streamer_mode),
            },
        );
        // sshelf was started again: show this window instead (also from the
        // tray, where only `logic` runs).
        if self.startup.show.as_ref().is_some_and(|s| s.take()) {
            close_guard::show(ctx);
        }
        motion::set_reduce_motion(
            ctx,
            self.data.as_ref().is_some_and(|d| d.settings.reduce_motion),
        );
        self.run_tray(ctx);
        self.tour_step();
        if self.tour_close_request(ctx) {
            return;
        }
        // A tray exists only if one was created and the desktop shows it.
        let tray = self.tray.is_some() && self.ssh.as_ref().is_some_and(|s| s.tray == Ok(true));
        self.close.on_close_request(
            ctx,
            close_guard::running_count(self.tunnel_states.as_deref()),
            self.data.as_deref().map(|d| &d.settings),
            tray,
        );
    }

    /// While the tour runs, pointer input reaches the UI only through the
    /// overlay's holes (and the popover, and the drag strip).
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(run) = &self.tour {
            tour_run::filter_input(raw_input, &run.allowed, window::STRIP_HEIGHT);
        }
    }

    /// Transparent, so the window's rounded corners show what is behind it.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = &Palette::ONE_DARK;
        // The sshelf tray menu: drawn here even while the window is hidden
        // (it asked for this pass; see `tray_menu`).
        self.tray_menu.show(ui.ctx());
        window::background(ui.ctx());
        if let Some(error) = self.startup_error.clone() {
            self.drag_strip(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                ui.colored_label(p.red, error);
            });
            window::resize_handles(ui);
            return;
        }

        // First launch: the welcome screen instead of the main UI, while
        // `settings.toml` has no `tour` key (SPEC.md §10).
        let first_launch = self
            .data
            .as_ref()
            .is_some_and(|d| d.settings.tour.is_none());
        if !self.tour_running() && (first_launch || self.welcome) {
            self.drag_strip(ui);
            self.toasts.show(ui.ctx());
            let choice = egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| welcome::show(ui, self.config.as_deref()))
                .inner;
            if let Some(choice) = choice {
                self.welcome = false;
                self.save_tour_state(settings::TourState::Offered);
                match choice {
                    welcome::Choice::Tour => self.start_tour(ui.ctx(), 0),
                    welcome::Choice::MyHosts => self.view = View::Hosts,
                    welcome::Choice::NewHost => {
                        self.view = View::Hosts;
                        if let Some(config) = &self.config {
                            self.hosts.editor.open_new(config);
                        }
                    }
                }
            }
            window::resize_handles(ui);
            return;
        }

        // Bottom first: the status bar runs under the rail.
        self.status_bar(ui);
        if self.rail(ui) {
            self.orphans.open = !self.orphans.open;
        }
        self.drag_strip(ui);
        self.toasts.show(ui.ctx());

        let mut commands = Vec::new();
        let mut tour_chapter = None;
        // Hosts sits flush with the right and bottom edges: the cards'
        // detail pane runs to the window edge and the status bar.
        let frame = egui::Frame::central_panel(ui.style());
        let frame = match self.view {
            View::Hosts => frame.inner_margin(egui::Margin {
                right: 0,
                bottom: 0,
                ..frame.inner_margin
            }),
            _ => frame,
        };
        egui::CentralPanel::default()
            .frame(frame)
            .show(ui, |ui| match self.view {
                View::Hosts => {
                    commands = self.hosts.show(
                        ui,
                        self.config.as_deref(),
                        self.data.as_deref(),
                        self.tunnel_states.as_deref(),
                        self.ssh.as_deref(),
                    );
                    for notice in self.hosts.take_notices() {
                        self.toasts.push(notice);
                    }
                }
                View::Tunnels => {
                    commands = self.tunnels.show(
                        ui,
                        self.config.as_deref(),
                        self.data.as_deref(),
                        self.tunnel_states.as_deref(),
                        self.ssh.as_deref(),
                    );
                }
                View::Groups => {
                    commands =
                        self.groups
                            .show(ui, self.data.as_deref(), self.tunnel_states.as_deref());
                }
                View::Settings => {
                    commands = self.settings.show(
                        ui,
                        self.data.as_deref(),
                        self.ssh.as_deref(),
                        self.config.as_deref().map(|c| c.path.as_path()),
                        close_guard::running_count(self.tunnel_states.as_deref()),
                    );
                    for notice in self.settings.take_notices() {
                        self.toasts.push(notice);
                    }
                    tour_chapter = self.settings.take_tour();
                    if let Some((on, hide)) = self.settings.take_streamer() {
                        self.set_streamer(on, hide);
                    }
                }
            });
        if self.orphans.open
            && let Some(data) = &self.data
        {
            let hosts: Vec<String> = self
                .config
                .as_deref()
                .and_then(|c| c.hosts())
                .map(|h| h.hosts.iter().map(|e| e.primary().to_string()).collect())
                .unwrap_or_default();
            commands.extend(self.orphans.show(ui.ctx(), data, &hosts));
        }
        commands.extend(self.hostkeys.show(ui.ctx()));
        for notice in self.hostkeys.take_notices() {
            self.toasts.push(notice);
        }
        // The key window outlives the hosts view (a tunnel's *Generate key…*).
        if self.view != View::Hosts
            && let Some(config) = self.config.clone()
        {
            commands.extend(
                self.hosts
                    .editor
                    .show_keys(ui.ctx(), &config, self.ssh.as_deref()),
            );
        }
        match self.tunnels.take_key_fix() {
            Some(KeyFix::GenerateKey(host)) => {
                if let Some(config) = &self.config {
                    self.hosts.editor.open_keys(config, &host);
                }
            }
            Some(KeyFix::LoadKey(host)) => {
                // The host's key from `ssh -G` when known, else ssh's default.
                let file = self
                    .hosts
                    .detail
                    .identity_file(&host)
                    .unwrap_or_else(|| "~/.ssh/id_ed25519".to_string());
                commands.push(Command::AddKeyToAgent {
                    path: file.into(),
                    encrypted: true,
                });
            }
            None => {}
        }
        let running =
            close_guard::running_tunnels(self.tunnel_states.as_deref(), self.data.as_deref());
        commands.extend(self.close.modal(
            ui.ctx(),
            &running,
            self.data.as_deref().map(|d| &d.settings),
        ));
        self.tour_close_choice();
        // A tunnel's "via" link shows its host.
        if let Some(alias) = self.tunnels.take_open_host() {
            self.view = View::Hosts;
            self.hosts.select(&alias);
        }
        // A group's tunnel row shows that tunnel.
        if let Some(id) = self.groups.take_open_tunnel() {
            self.view = View::Tunnels;
            self.tunnels.select(&id);
        }
        match self.hosts.take_navigation() {
            Some(Navigate::Settings) => self.view = View::Settings,
            Some(Navigate::Tunnel(id)) => {
                self.view = View::Tunnels;
                self.tunnels.select(&id);
            }
            None => {}
        }
        if let Some(host) = self.hosts.detail.take_new_tunnel()
            && let Some(data) = &self.data
        {
            self.view = View::Tunnels;
            self.tunnels.new_tunnel(&host, &data.tunnels);
        }
        self.send(commands);
        // After this frame's commands went to the user's Core.
        if let Some(chapter) = tour_chapter {
            self.start_tour(ui.ctx(), steps::first_step(chapter));
        }
        window::resize_handles(ui);
        self.tour_overlay(ui.ctx());
    }
}
