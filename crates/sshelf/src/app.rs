//! Top-level eframe app: applies core events and lays out the sidebar, the
//! current view, the status bar (errors) and toasts (successes). All side
//! effects go through the core.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use sshelf_core::core::{AppData, Command, ConfigSnapshot, Core, Event, SshInfo};
use sshelf_core::paths::Paths;
use sshelf_core::supervisor::Snapshot;

use crate::close_guard::{self, CloseGuard};
use crate::fonts;
use crate::groups_view::GroupsView;
use crate::hostkey_modal::HostKeyModals;
use crate::hosts_view::{HostsView, Navigate};
use crate::icon::Status;
use crate::orphans_window::{self, OrphansWindow};
use crate::settings_view::SettingsView;
use crate::sidebar::{self, View};
use crate::theme::{self, Palette};
use crate::toast::Toasts;
use crate::tray::{self, Tray, TrayAction};
use crate::tunnels_view::TunnelsView;
use crate::{motion, status_bar, window};

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
    /// The last error, in the status bar until dismissed.
    error: Option<String>,
    /// Successes, briefly.
    toasts: Toasts,
    /// When the last config snapshot arrived (the status bar shows it).
    config_loaded: Option<SystemTime>,
}

impl SshelfApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        #[cfg(target_os = "windows")]
        window::native::prepare(cc);
        let ctx = cc.egui_ctx.clone();
        // The event loop runs by now, as macOS requires for tray icons.
        let (tray, tray_error) = match Tray::create(&cc.egui_ctx) {
            Ok(tray) => (Some(tray), None),
            Err(e) => (None, Some(format!("No tray icon: {e}"))),
        };
        let (core, startup_error) = match Paths::detect() {
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
            error: tray_error,
            toasts: Toasts::default(),
            config_loaded: None,
        }
    }

    /// Carries out tray clicks and keeps the tray menu current.
    fn run_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = &mut self.tray else {
            return;
        };
        for action in tray.actions() {
            match action.command() {
                Some(command) => self.send(vec![command]),
                None if action == TrayAction::Show => close_guard::show(ctx),
                // The same close request as the window's (SPEC.md §10).
                None => self.close.quit_from_tray(ctx),
            }
        }
        let Some(tray) = &mut self.tray else {
            return;
        };
        tray.update(tray::model(
            self.data.as_deref(),
            self.config.as_deref().and_then(|c| c.hosts()),
            self.tunnel_states.as_deref(),
        ));
    }

    fn apply_events(&mut self, ctx: &egui::Context) {
        let Some((_, events)) = &self.core else {
            return;
        };
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Config(snapshot) => {
                    self.hosts.on_config(&snapshot);
                    self.config = Some(snapshot);
                    self.config_loaded = Some(SystemTime::now());
                }
                Event::Data(data) => self.data = Some(data),
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
                core.send(command);
            }
        }
    }

    /// The window's drag strip (SPEC.md §10), above everything but the sidebar.
    fn drag_strip(&self, ui: &mut egui::Ui) {
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
            .show(ui, |ui| window::strip(ui, "sshelf"))
            .inner;
        if let Some(action) = action {
            window::apply(ui.ctx(), action);
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
            .exact_size(status_bar::HEIGHT)
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
        motion::set_reduce_motion(
            ctx,
            self.data.as_ref().is_some_and(|d| d.settings.reduce_motion),
        );
        self.run_tray(ctx);
        // A tray exists only if one was created and the desktop shows it.
        let tray = self.tray.is_some() && self.ssh.as_ref().is_some_and(|s| s.tray == Ok(true));
        self.close.on_close_request(
            ctx,
            close_guard::running_count(self.tunnel_states.as_deref()),
            self.data.as_deref().map(|d| &d.settings),
            tray,
        );
    }

    /// Transparent, so the window's rounded corners show what is behind it.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = &Palette::ONE_DARK;
        window::background(ui.ctx());
        if let Some(error) = &self.startup_error {
            self.drag_strip(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                ui.colored_label(p.red, error);
            });
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
                    );
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
        commands.extend(self.close.modal(
            ui.ctx(),
            close_guard::running_count(self.tunnel_states.as_deref()),
            self.data.as_deref().map(|d| &d.settings),
        ));
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
        window::resize_handles(ui);
    }
}
