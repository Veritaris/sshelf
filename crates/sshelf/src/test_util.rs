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

/// Every text egui drew in the last frame (all layers and viewports of the
/// root's output), one galley per line: what a screen recording would show.
pub fn drawn_text<State>(harness: &Harness<'_, State>) -> String {
    fn walk(shape: &egui::Shape, out: &mut String) {
        match shape {
            egui::Shape::Text(text) => {
                out.push_str(text.galley.text());
                out.push('\n');
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    for clipped in &harness.output().shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

/// Presses the primary button at `pos` (held until [`release`]).
pub fn press<State>(harness: &mut Harness<'_, State>, pos: egui::Pos2) {
    let input = harness.input_mut();
    input.events.push(egui::Event::PointerMoved(pos));
    input.events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
}

/// Releases the primary button pressed with [`press`].
pub fn release<State>(harness: &mut Harness<'_, State>, pos: egui::Pos2) {
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
}
