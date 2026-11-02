//! Top-level eframe app: applies core events and lays out the sidebar, the
//! current view and the status bar. All side effects go through the core.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use egui::RichText;
use sshelf_core::core::{AppData, Command, ConfigSnapshot, Core, Event, SshInfo};
use sshelf_core::paths::Paths;

use crate::hosts_view::HostsView;
use crate::sidebar::{self, View};
use crate::theme::{self, Palette};

enum Status {
    Info(String),
    Error(String),
}

pub struct SshelfApp {
    core: Option<(Core, Receiver<Event>)>,
    startup_error: Option<String>,
    view: View,
    config: Option<Arc<ConfigSnapshot>>,
    data: Option<Arc<AppData>>,
    ssh: Option<Arc<SshInfo>>,
    hosts: HostsView,
    status: Option<Status>,
}

impl SshelfApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
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
            hosts: HostsView::default(),
            status: None,
        }
    }

    fn apply_events(&mut self) {
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
                    self.status = Some(Status::Info(format!("Opened a terminal for {alias}")));
                }
                Event::Error(message) => self.status = Some(Status::Error(message)),
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
        let Some(status) = &self.status else {
            return;
        };
        let (text, color) = match status {
            Status::Info(text) => (text.clone(), p.dim),
            Status::Error(text) => (text.clone(), p.red),
        };
        let mut dismiss = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).color(color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    dismiss = ui.small_button("✖").clicked();
                });
            });
        });
        if dismiss {
            self.status = None;
        }
    }
}

impl eframe::App for SshelfApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = &Palette::ONE_DARK;
        if let Some(error) = &self.startup_error {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.colored_label(p.red, error);
            });
            return;
        }
        self.apply_events();

        let orphans = self
            .data
            .as_ref()
            .map(|d| d.orphans.clone())
            .unwrap_or_default();
        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| sidebar::show(ui, &mut self.view, &orphans));
        self.status_bar(ui);

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
        });
        self.send(commands);
    }
}
