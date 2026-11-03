//! Lucide icons (SPEC.md §10 *Visual design*): codepoints in the bundled
//! `lucide.ttf` (from `font/codepoints.json` of `lucide-static`, see
//! `assets/fonts/README.md`) and helpers to show them. Icons are painted, so
//! they never become part of a widget's accessible name.

use egui::{Align2, Atom, Color32, FontFamily, FontId, Response, RichText, Role, Sense, Ui};

/// The font family holding the Lucide glyphs (installed by [`crate::fonts`]).
pub const FAMILY: &str = "lucide";

/// Icon size in small buttons and next to text.
pub const INLINE: f32 = 14.0;

pub const CIRCLE_CHECK: char = '\u{e226}';
pub const CIRCLE_X: char = '\u{e084}';
pub const INFO: char = '\u{e0f9}';
pub const PLAY: char = '\u{e13c}';
pub const REFRESH_CW: char = '\u{e145}';
pub const ROTATE_CW: char = '\u{e149}';
pub const SHIELD_ALERT: char = '\u{e1fe}';
pub const SQUARE: char = '\u{e167}';
pub const SQUARE_TERMINAL: char = '\u{e20a}';
pub const STAR: char = '\u{e176}';
pub const TRIANGLE_ALERT: char = '\u{e193}';
pub const X: char = '\u{e1b2}';

/// Every icon above, for the glyph check in [`crate::fonts`].
#[cfg(test)]
pub const ALL: &[char] = &[
    CIRCLE_CHECK,
    CIRCLE_X,
    INFO,
    PLAY,
    REFRESH_CW,
    ROTATE_CW,
    SHIELD_ALERT,
    SQUARE,
    SQUARE_TERMINAL,
    STAR,
    TRIANGLE_ALERT,
    X,
];

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY.into()))
}

/// An icon for buttons and other atom widgets, in the widget's text colour
/// (so it follows hover and disabled states like the label next to it).
pub fn atom<'a>(icon: char) -> Atom<'a> {
    Atom::paint(egui::Vec2::splat(INLINE), move |ui, args| {
        paint(
            ui,
            args.rect.center(),
            icon,
            INLINE,
            args.fallback_text_color,
        );
    })
}

/// Like [`atom`], always in `color`.
pub fn tinted<'a>(icon: char, color: Color32) -> Atom<'a> {
    Atom::paint(egui::Vec2::splat(INLINE), move |ui, args| {
        paint(ui, args.rect.center(), icon, INLINE, color);
    })
}

/// A decorative icon of `size` in `color`; its response only senses hover.
pub fn show(ui: &mut Ui, icon: char, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), Sense::hover());
    paint(ui, rect.center(), icon, size, color);
    response
}

/// An icon followed by `text`, both in `color`; returns the text's response.
pub fn label(ui: &mut Ui, icon: char, color: Color32, text: impl Into<RichText>) -> Response {
    ui.horizontal_wrapped(|ui| {
        show(ui, icon, INLINE, color);
        ui.label(text.into().color(color))
    })
    .inner
}

/// A small icon-only button. `name` is its accessible name and tooltip.
pub fn button(ui: &mut Ui, icon: char, name: &str) -> Response {
    let response = ui.add(egui::Button::new(atom(icon)).small());
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(Role::Button, enabled, name));
    response.on_hover_text(name)
}

fn paint(ui: &Ui, center: egui::Pos2, icon: char, size: f32, color: Color32) {
    ui.painter()
        .text(center, Align2::CENTER_CENTER, icon, font(size), color);
}
