// Hide the console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod close_guard;
mod fonts;
mod groups_view;
mod host_detail;
mod host_editor;
mod hostkey_modal;
mod hosts_view;
mod icon;
mod icons;
mod key_dialog;
mod motion;
mod orphans_window;
mod settings_view;
mod sidebar;
#[cfg(test)]
mod test_util;
mod theme;
mod toast;
mod tray;
mod tunnels_view;
mod util;
mod window;

use std::sync::Arc;

use app::SshelfApp;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: window::viewport(
            egui::ViewportBuilder::default()
                .with_title("sshelf")
                .with_icon(Arc::new(icon::egui_icon())),
        ),
        ..Default::default()
    };

    eframe::run_native(
        "sshelf",
        native_options,
        Box::new(|cc| Ok(Box::new(SshelfApp::new(cc)))),
    )
}
