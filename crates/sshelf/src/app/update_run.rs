//! The update check in the app (SPEC.md §10 *Update check*): starts it once
//! the window or tray is up, announces a newer version with the sticky
//! toast (once per start) and the rail dot, and shows the first-run note:
//! the welcome screen's line for new users, a one-time toast for existing
//! ones. The tour shows none of it, and nothing here reaches its Core.

use egui::Context;
use sshelf_core::core::Command;
use sshelf_core::updates::Release;

use super::SshelfApp;
use crate::theme::Palette;
use crate::toast::{Sticky, StickyAction, StickyClick, StickyKind};
use crate::{icons, util};

/// The update toast's buttons, in order.
const WHATS_NEW: usize = 0;
/// The first-run note's buttons, in order.
const TURN_OFF: usize = 1;

impl SshelfApp {
    /// From `logic`: starts the check once, then raises (or drops) the
    /// sticky toasts.
    pub(super) fn run_updates(&mut self) {
        if self.tour_running() {
            return;
        }
        if self.core.is_some()
            && let Some(source) = self.startup.updates.take()
        {
            self.send(vec![Command::StartUpdates(source)]);
        }
        let unnoted = self.updates_on
            && self
                .user_settings()
                .is_some_and(|s| s.tour.is_some() && !s.update_check_noted);
        if unnoted && !self.note_shown {
            self.note_shown = true;
            self.toasts.push_sticky(note_toast());
        }
        match self.update_to_announce().cloned() {
            Some(release) if self.announced.as_deref() != Some(release.version.as_str()) => {
                self.announced = Some(release.version.clone());
                let current = self
                    .updates
                    .as_ref()
                    .map(|s| s.current.clone())
                    .unwrap_or_default();
                self.toasts.push_sticky(update_toast(&release, &current));
            }
            // Skipped (in Settings too): the toast goes with the dot.
            None => self.toasts.remove_sticky(StickyKind::Update),
            Some(_) => {}
        }
    }

    /// The newer release the toast and the rail dot announce; none during
    /// the tour.
    pub(super) fn update_to_announce(&self) -> Option<&Release> {
        if self.tour_running() {
            return None;
        }
        self.updates.as_deref()?.announced()
    }

    /// The welcome screen was shown: that is the first-run note (once).
    pub(super) fn note_welcome(&mut self) {
        if self.note_shown || !self.updates_on {
            return;
        }
        self.note_shown = true;
        if self.user_settings().is_some_and(|s| !s.update_check_noted) {
            self.send(vec![Command::NoteUpdateCheck { keep_on: true }]);
        }
    }

    /// Every toast, and what was pressed on the sticky ones (held back
    /// during the tour).
    pub(super) fn show_toasts(&mut self, ctx: &Context) {
        let clicks = self.toasts.show(ctx, !self.tour_running());
        for click in clicks {
            match click {
                StickyClick::Action(StickyKind::Update, WHATS_NEW) => {
                    if let Some(release) = self.update_to_announce() {
                        ctx.open_url(egui::OpenUrl::new_tab(&release.url));
                    }
                }
                StickyClick::Action(StickyKind::Update, _) => {
                    if let Some(release) = self.update_to_announce() {
                        let skip = Command::SkipUpdate(release.version.clone());
                        self.send(vec![skip]);
                    }
                }
                // The dot stays; the toast doesn't come back this run.
                StickyClick::Closed(StickyKind::Update) => {}
                StickyClick::Action(StickyKind::CheckNote, TURN_OFF) => {
                    self.send(vec![Command::NoteUpdateCheck { keep_on: false }]);
                }
                // *Keep it on*, or closed (which keeps it on).
                StickyClick::Action(StickyKind::CheckNote, _)
                | StickyClick::Closed(StickyKind::CheckNote) => {
                    self.send(vec![Command::NoteUpdateCheck { keep_on: true }]);
                }
            }
        }
    }
}

/// "sshelf 0.4.0 is available", "You have 0.3.2 · released 2 Nov".
fn update_toast(release: &Release, current: &str) -> Sticky {
    let mut detail = vec![("You have ".into(), false), (current.to_string(), true)];
    if let Some(published) = release.published {
        detail.push((format!(" \u{b7} released {}", util::day(published)), false));
    }
    Sticky {
        kind: StickyKind::Update,
        icon: (icons::CIRCLE_ARROW_UP, Palette::ONE_DARK.blue),
        title: vec![
            ("sshelf ".into(), false),
            (release.version.clone(), true),
            (" is available".into(), false),
        ],
        detail,
        actions: vec![
            StickyAction {
                label: "What's new",
                icon: Some(icons::EXTERNAL_LINK),
                ghost: false,
            },
            StickyAction {
                label: "Skip this version",
                icon: None,
                ghost: true,
            },
        ],
    }
}

/// The first-run note for users who had sshelf before the check existed.
fn note_toast() -> Sticky {
    Sticky {
        kind: StickyKind::CheckNote,
        icon: (icons::INFO, Palette::ONE_DARK.blue),
        title: vec![("sshelf now checks for new versions".into(), false)],
        detail: vec![(
            "It asks GitHub at start and once a day. Nothing about you is sent.".into(),
            false,
        )],
        actions: vec![
            StickyAction {
                label: "Keep it on",
                icon: None,
                ghost: false,
            },
            StickyAction {
                label: "Turn off",
                icon: None,
                ghost: true,
            },
        ],
    }
}
