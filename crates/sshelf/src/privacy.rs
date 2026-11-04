//! Streamer mode's state for drawing (SPEC.md §10 *Streamer mode*). The app
//! stores it in the context once per frame; views and widgets read it from
//! there, so no view signature changes.

use std::borrow::Cow;
use std::sync::Arc;

use egui::{Context, Id};
use sshelf_core::redact::Redactor;

#[derive(Clone, Default)]
pub struct Privacy {
    /// Streamer mode is on: values are masked.
    pub on: bool,
    /// What the free-text filter knows (core rebuilds it on every config
    /// load and tunnel change).
    pub redactor: Arc<Redactor>,
}

impl Privacy {
    /// The toast after copying `name`'s value.
    pub fn copied(&self, name: &str) -> String {
        if self.on {
            format!("Copied {name} · not shown in streamer mode")
        } else {
            format!("Copied {name}")
        }
    }

    /// `text` through the filter for places that only take a string (the
    /// native tray menu, the window title): tokens become "•••••".
    pub fn plain<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.on {
            Cow::Owned(self.redactor.mask_plain(text, false))
        } else {
            Cow::Borrowed(text)
        }
    }
}

fn id() -> Id {
    Id::unique("sshelf_privacy")
}

/// Stores this frame's state; the app calls it at the start of every pass.
pub fn set(ctx: &Context, privacy: Privacy) {
    ctx.data_mut(|d| d.insert_temp(id(), privacy));
}

/// The current state (off until the app has set it, as in widget tests).
pub fn get(ctx: &Context) -> Privacy {
    ctx.data(|d| d.get_temp(id())).unwrap_or_default()
}
