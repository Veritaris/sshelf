//! Shared `egui_kittest` setup: every UI test runs with the app's real fonts
//! and theme, so a glyph missing from the bundled fonts fails the test.

use egui_kittest::{Harness, HarnessBuilder};

/// `Harness::builder()` with the fonts and theme installed.
pub fn harness<State>() -> HarnessBuilder<State> {
    Harness::builder().with_context_setup(|ctx| {
        crate::fonts::install(ctx);
        crate::theme::apply(ctx);
    })
}
