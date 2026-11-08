// No console window on Windows, in debug builds too. Started from a terminal
// (`cargo run`), stdout and stderr still reach it through the inherited
// handles; `sshelf --fake-ssh` relies on the same with piped stdio.
#![windows_subsystem = "windows"]

mod app;
#[cfg(test)]
mod build_version;
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
mod kube_editor;
mod motion;
mod orphans_window;
mod privacy;
mod settings_view;
mod sidebar;
mod start_prompt;
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
mod version;
mod widgets;
mod window;

use std::process::ExitCode;
use std::sync::Arc;

use app::{ShowRequests, SshelfApp};
use sshelf_core::instance;
use sshelf_core::paths::Paths;

fn main() -> ExitCode {
    // As the first argument, before any window, tray or lock exists: the
    // onboarding tour's sandbox runs sshelf itself as its ssh (SPEC.md §8),
    // and `--version` (§10 *Update check*).
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some(fake_ssh::EMBEDDED_FLAG) => return fake_ssh::run(args.collect()),
        Some("--version") => {
            print_line(&version::line());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sshelf: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `text` on stdout. On Windows a console that started this GUI-subsystem
/// program hands it no stdout (unless redirected): then the line goes to
/// that console.
fn print_line(text: &str) {
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
        };
        // SAFETY: plain calls without pointers.
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if (handle.is_null() || handle == INVALID_HANDLE_VALUE)
            // SAFETY: as above.
            && unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0
            && let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("CONOUT$")
        {
            let _ = writeln!(console, "{text}");
            return;
        }
    }
    println!("{text}");
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
    // One sshelf per user (SPEC.md §10 *Window*): a later start shows the
    // running one's window and ends, before any window or tray of its own.
    let show = Arc::new(ShowRequests::default());
    let _instance = match Paths::detect() {
        Some(paths) => {
            let requests = Arc::clone(&show);
            match instance::acquire(&paths.app_dir, move || requests.request()) {
                Ok(Some(running)) => Some(running),
                Ok(None) => {
                    if let Err(e) = instance::show_running(&paths.app_dir) {
                        already_running(&e.to_string());
                    }
                    return Ok(());
                }
                // No lock (e.g. an unwritable app folder): start anyway.
                Err(e) => {
                    eprintln!("sshelf: cannot check for a running sshelf: {e}");
                    None
                }
            }
        }
        None => None,
    };

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
        Box::new(|cc| Ok(Box::new(SshelfApp::new(cc, show)))),
    )
}

/// sshelf runs already but didn't take the request to show itself: say so
/// (a message box on Windows, where there is no console) and start nothing.
fn already_running(why: &str) {
    let text = format!(
        "sshelf is already running, but it did not respond ({why}).\n\
         Find it in the tray, or end it and start sshelf again."
    );
    eprintln!("sshelf: {text}");
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (wide(&text), wide("sshelf"));
        // SAFETY: NUL-terminated UTF-16 strings; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONWARNING,
            )
        };
    }
}
