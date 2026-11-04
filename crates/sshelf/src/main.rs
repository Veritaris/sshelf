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
mod status_bar;
#[cfg(test)]
mod test_util;
mod theme;
mod toast;
mod tour;
mod tray;
mod tray_menu;
mod tunnels_view;
mod util;
mod widgets;
mod window;

use std::process::ExitCode;
use std::sync::Arc;

use app::SshelfApp;

fn main() -> ExitCode {
    // The onboarding tour's sandbox runs sshelf itself as its ssh (SPEC.md
    // §8): as the first argument, before any window, tray or lock exists.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some(fake_ssh::EMBEDDED_FLAG) {
        return fake_ssh::run(args.collect());
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sshelf: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> eframe::Result<()> {
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
