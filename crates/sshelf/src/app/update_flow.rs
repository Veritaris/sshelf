//! The update check in the app (SPEC.md §12 M14 *Tests*, UI), with the
//! user's real Core asking a local stand-in for GitHub: the toast and its
//! buttons, the rail dot, and the first-run note for an existing user
//! (and none for a new one, whose welcome screen says it). No test asks
//! GitHub.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sshelf_core::core::UpdateSource;
use sshelf_core::paths::Paths;

use super::{SshelfApp, Startup};
use crate::test_util;
use crate::toast::CLOSE_NAME;
use crate::tour::welcome;

/// Answers every request with release `version` as GitHub's JSON; the
/// count is the requests made.
fn serve(version: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
    let body = format!(
        "{{\"tag_name\": \"{version}\", \
         \"html_url\": \"https://github.com/Veritaris/sshelf/releases/tag/{version}\", \
         \"published_at\": \"2026-11-02T10:00:00Z\"}}"
    );
    let count = Arc::new(AtomicUsize::new(0));
    let requests = Arc::clone(&count);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            requests.fetch_add(1, Ordering::SeqCst);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, count)
}

/// An app on a user folder in `root` (no ssh) whose build is 0.3.2 and
/// asks `url` at once; `settings`: extra `settings.toml` lines.
fn app(root: &Path, settings: &str, url: &str) -> Harness<'static, SshelfApp> {
    let paths = Paths::new(&root.join("home"), root.join("app"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::create_dir_all(&paths.app_dir).unwrap();
    fs::write(&paths.ssh_config, "Host mine\n").unwrap();
    fs::write(
        paths.app_dir.join("settings.toml"),
        format!(
            "version = 1\nssh_path = '{}'\nreduce_motion = true\n{settings}",
            root.join("no-ssh-here").display()
        ),
    )
    .unwrap();
    let temp = root.join("temp");
    fs::create_dir_all(&temp).unwrap();
    let startup = Startup {
        tray: false,
        fake_ssh: None,
        temp,
        user_paths: Some(paths),
        show: None,
        updates: Some(UpdateSource {
            version: "0.3.2".into(),
            url: url.into(),
            first: Duration::ZERO,
        }),
    };
    test_util::harness()
        .with_size([1280.0, 800.0])
        .build_eframe(|cc| SshelfApp::with(cc, startup))
}

fn settle(harness: &mut Harness<'_, SshelfApp>, what: &str, done: impl Fn(&SshelfApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn until_drawn(harness: &mut Harness<'_, SshelfApp>, text: &str) {
    settle(harness, text, |_| true);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !test_util::drawn_text(harness).contains(text) {
        assert!(Instant::now() < deadline, "timed out waiting for {text:?}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn frames(harness: &mut Harness<'_, SshelfApp>) {
    for _ in 0..4 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn settings_file(root: &Path) -> String {
    fs::read_to_string(root.join("app").join("settings.toml")).unwrap()
}

const NOTED: &str = "tour = \"done\"\nupdate_check_noted = true\n";

#[test]
fn a_newer_release_shows_the_toast_and_dot_until_skipped() {
    let (url, requests) = serve("9.9.9");
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), NOTED, &url);
    until_drawn(&mut harness, "sshelf 9.9.9 is available");
    harness.get_by_label_contains("You have 0.3.2");
    assert_eq!(
        harness
            .state()
            .update_to_announce()
            .map(|r| r.version.as_str()),
        Some("9.9.9")
    );
    // *What's new* opens the release page and closes the toast.
    harness.get_by_label("What's new").click();
    harness.step();
    let opened = harness.output().platform_output.commands.iter().any(
        |c| matches!(c, egui::OutputCommand::OpenUrl(open) if open.url.ends_with("/tag/9.9.9")),
    );
    assert!(opened, "{:?}", harness.output().platform_output.commands);
    frames(&mut harness);
    assert!(
        harness
            .query_by_label("sshelf 9.9.9 is available")
            .is_none()
    );
    // The dot stays; *Skip* from Settings takes it.
    assert!(harness.state().update_to_announce().is_some());
    harness.state_mut().view = super::View::Settings;
    frames(&mut harness);
    harness.get_by_label("Skip this version").click();
    settle(&mut harness, "skipped", |app| {
        app.updates
            .as_ref()
            .is_some_and(|s| s.skipped.as_deref() == Some("9.9.9"))
    });
    assert!(harness.state().update_to_announce().is_none());
    let file = fs::read_to_string(root.path().join("app").join("updates.toml")).unwrap();
    assert!(file.contains("skipped = \"9.9.9\""), "{file}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[test]
fn a_closed_update_toast_keeps_the_dot_and_skip_silences_it() {
    let (url, _) = serve("9.9.9");
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), NOTED, &url);
    until_drawn(&mut harness, "sshelf 9.9.9 is available");
    harness.get_by_label(CLOSE_NAME).click();
    frames(&mut harness);
    assert!(
        harness
            .query_by_label("sshelf 9.9.9 is available")
            .is_none()
    );
    assert!(harness.state().update_to_announce().is_some());
    // Not again this run, though the next status still names it.
    harness.state_mut().announced = None;
    frames(&mut harness);
    harness.get_by_label("sshelf 9.9.9 is available");
    harness.get_by_label("Skip this version").click();
    settle(&mut harness, "skipped", |app| {
        app.update_to_announce().is_none()
    });
    frames(&mut harness);
    assert!(
        harness
            .query_by_label("sshelf 9.9.9 is available")
            .is_none()
    );
}

#[test]
fn an_existing_user_is_told_before_the_first_check() {
    let (url, requests) = serve("9.9.9");
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), "tour = \"done\"\n", &url);
    until_drawn(&mut harness, "sshelf now checks for new versions");
    harness.get_by_label_contains("Nothing about you is sent.");
    for _ in 0..20 {
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "checked before the note"
    );
    harness.get_by_label("Keep it on").click();
    settle(&mut harness, "the check", |_| {
        requests.load(Ordering::SeqCst) == 1
    });
    until_drawn(&mut harness, "sshelf 9.9.9 is available");
    let settings = settings_file(root.path());
    assert!(settings.contains("update_check_noted = true"), "{settings}");
    assert!(settings.contains("update_check = true"), "{settings}");
}

#[test]
fn turning_it_off_from_the_note_never_checks() {
    let (url, requests) = serve("9.9.9");
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), "tour = \"done\"\n", &url);
    until_drawn(&mut harness, "sshelf now checks for new versions");
    harness.get_by_label("Turn off").click();
    settle(&mut harness, "turned off", |app| {
        app.updates.as_ref().is_some_and(|s| !s.enabled)
    });
    frames(&mut harness);
    assert!(
        harness
            .query_by_label("sshelf now checks for new versions")
            .is_none()
    );
    let settings = settings_file(root.path());
    assert!(settings.contains("update_check = false"), "{settings}");
    assert!(settings.contains("update_check_noted = true"), "{settings}");
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[test]
fn a_new_user_reads_it_on_the_welcome_screen_instead() {
    let (url, requests) = serve("0.3.2");
    let root = tempfile::tempdir().unwrap();
    let mut harness = app(root.path(), "", &url);
    until_drawn(&mut harness, welcome::UPDATE_NOTE);
    settle(&mut harness, "the check", |_| {
        requests.load(Ordering::SeqCst) == 1
    });
    frames(&mut harness);
    assert!(
        !test_util::drawn_text(&harness).contains("sshelf now checks for new versions"),
        "no toast for a new user"
    );
    harness.get_by_label("Open my hosts").click();
    settle(&mut harness, "tour = offered", |app| {
        app.data.as_ref().is_some_and(|d| d.settings.tour.is_some())
    });
    frames(&mut harness);
    let settings = settings_file(root.path());
    assert!(settings.contains("update_check_noted = true"), "{settings}");
    assert!(!test_util::drawn_text(&harness).contains("sshelf now checks for new versions"));
    // Same version: nothing to announce.
    assert!(harness.state().update_to_announce().is_none());
}
