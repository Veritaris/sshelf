// No console window on Windows, in debug builds too. Started from a terminal
// (`cargo run`), stdout and stderr still reach it through the inherited
// handles; `sshelf --fake-ssh` relies on the same with piped stdio.
#![windows_subsystem = "windows"]

mod app;
mod close_guard;
mod fonts;
mod groups_view;
mod host_detail;
mod host_editor;
mod hostkey_modal;
mod hosts_view;
mod icon;
mod icon_shape;
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

/// The icon eframe starts with: the window icon, or on macOS the Dock icon
/// (eframe makes it the app icon there, which Cmd+Tab shows large).
fn startup_icon() -> egui::IconData {
    #[cfg(target_os = "macos")]
    return window::macos::dock_icon(icon::Status::Idle);
    #[cfg(not(target_os = "macos"))]
    icon::egui_icon(icon::Status::Idle)
}

fn run() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: window::viewport(
            egui::ViewportBuilder::default()
                .with_title("sshelf")
                .with_icon(Arc::new(startup_icon())),
        ),
        ..Default::default()
    };

    eframe::run_native(
        "sshelf",
        native_options,
        Box::new(|cc| Ok(Box::new(SshelfApp::new(cc)))),
    )
}
