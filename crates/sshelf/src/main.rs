// Hide the console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod close_guard;
mod groups_view;
mod host_detail;
mod host_editor;
mod hostkey_modal;
mod hosts_view;
mod icon;
mod orphans_window;
mod settings_view;
mod sidebar;
mod theme;
mod tray;
mod tunnels_view;
mod util;

use std::sync::Arc;

use app::SshelfApp;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("sshelf")
            .with_icon(Arc::new(icon::egui_icon())),
        ..Default::default()
    };

    eframe::run_native(
        "sshelf",
        native_options,
        Box::new(|cc| Ok(Box::new(SshelfApp::new(cc)))),
    )
}
