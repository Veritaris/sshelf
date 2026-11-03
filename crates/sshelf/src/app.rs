//! Top-level eframe app: applies core events and lays out the sidebar, the
//! current view, the status bar (errors) and toasts (successes). All side
//! effects go through the core.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use egui::RichText;
use sshelf_core::core::{AppData, Command, ConfigSnapshot, Core, Event, SshInfo};
use sshelf_core::paths::Paths;
use sshelf_core::supervisor::Snapshot;

use crate::close_guard::{self, CloseGuard};
use crate::fonts;
use crate::groups_view::GroupsView;
use crate::hostkey_modal::HostKeyModals;
use crate::hosts_view::HostsView;
use crate::icons;
use crate::orphans_window::{self, OrphansWindow};
use crate::settings_view::SettingsView;
use crate::sidebar::{self, View};
use crate::theme::{self, Palette};
use crate::toast::Toasts;
use crate::tray::{self, Tray, TrayAction};
use crate::tunnels_view::TunnelsView;

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
}

impl SshelfApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
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
                }
                Event::Data(data) => self.data = Some(data),
                Event::Resolved {
                    alias,
                    generation,
                    result,
                } => self.hosts.detail.on_resolved(alias, generation, result),
                Event::Ssh(info) => self.ssh = Some(info),
                Event::Launched { alias } => {
                    self.toasts.push(format!("Opened a terminal for {alias}"));
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

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let p = &Palette::ONE_DARK;
        let Some(text) = self.error.clone() else {
            return;
        };
        let mut dismiss = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).color(p.red));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    dismiss = icons::button(ui, icons::X, "Dismiss").clicked();
                });
            });
        });
        if dismiss {
            self.error = None;
        }
    }
}

impl eframe::App for SshelfApp {
    /// Runs every frame, and also while the window is hidden: core events and
    /// the tray are handled here, the drawing in [`Self::ui`].
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_events(ctx);
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

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = &Palette::ONE_DARK;
        if let Some(error) = &self.startup_error {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.colored_label(p.red, error);
            });
            return;
        }

        let orphans = self
            .data
            .as_deref()
            .map(orphans_window::descriptions)
            .unwrap_or_default();
        let orphans_clicked = egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| sidebar::show(ui, &mut self.view, &orphans))
            .inner;
        if orphans_clicked {
            self.orphans.open = !self.orphans.open;
        }
        self.status_bar(ui);
        self.toasts.show(ui.ctx());

        let mut commands = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Hosts => {
                commands = self.hosts.show(
                    ui,
                    self.config.as_deref(),
                    self.data.as_deref(),
                    self.ssh.as_deref(),
                );
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
        if let Some(host) = self.hosts.detail.take_new_tunnel()
            && let Some(data) = &self.data
        {
            self.view = View::Tunnels;
            self.tunnels.new_tunnel(&host, &data.tunnels);
        }
        self.send(commands);
    }
}
