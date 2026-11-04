//! Streamer mode's state for drawing (SPEC.md §10 *Streamer mode*). The app
//! stores it in the context once per frame; views and widgets read it from
//! there, so no view signature changes.

use egui::{Context, Id};

#[derive(Clone, Copy, Default)]
pub struct Privacy {
    /// Streamer mode is on: values are masked.
    pub on: bool,
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
