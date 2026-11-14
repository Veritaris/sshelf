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

/// Every accessibility name and value in the last frame, one per line: what
/// a screen reader (or a screen-reading tool) would get.
pub fn accessible_text<State>(harness: &Harness<'_, State>) -> String {
    use egui_kittest::kittest::{By, NodeT, Queryable};
    let mut out = String::new();
    for node in harness.query_all(By::new()) {
        let node = node.accesskit_node();
        for text in [node.label(), node.value()].into_iter().flatten() {
            out.push_str(&text);
            out.push('\n');
        }
    }
    out
}

/// The workspace's fake-ssh binary in `target/<profile>`, a folder above
/// this test binary (`deps/`, or `build/sshelf/<hash>/` in newer cargo's
/// layout): `cargo test --workspace` builds it.
pub fn fake_ssh_binary() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let name = format!("fake-ssh{}", std::env::consts::EXE_SUFFIX);
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "this test runs the fake-ssh binary; build it first \
                 (`cargo test --workspace` does); none above {}",
                exe.display()
            )
        })
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
