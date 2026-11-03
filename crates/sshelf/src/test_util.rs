//! Shared `egui_kittest` setup: every UI test runs with the app's real fonts
//! and theme, so a glyph missing from the bundled fonts fails the test, and
//! with *Reduce motion*, so animations end within the frame they start.

use egui_kittest::{Harness, HarnessBuilder};

/// `Harness::builder()` with the fonts, theme and reduced motion installed.
pub fn harness<State>() -> HarnessBuilder<State> {
    Harness::builder().with_context_setup(|ctx| {
        crate::fonts::install(ctx);
        crate::theme::apply(ctx);
        crate::motion::set_reduce_motion(ctx, true);
    })
}
