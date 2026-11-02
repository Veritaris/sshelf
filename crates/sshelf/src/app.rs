//! Top-level eframe app. M0: an empty themed window.

use egui::RichText;

use crate::theme::{self, Palette};

pub struct SshelfApp;

impl SshelfApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        Self
    }
}

impl eframe::App for SshelfApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(
                RichText::new("sshelf")
                    .color(Palette::ONE_DARK.accent)
                    .strong(),
            );
        });
    }
}
