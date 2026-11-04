//! Lucide icons (SPEC.md §10 *Visual design*): codepoints in the bundled
//! `lucide.ttf` (from `font/codepoints.json` of `lucide-static`, see
//! `assets/fonts/README.md`) and helpers to show them. Icons are painted, so
//! they never become part of a widget's accessible name.

use egui::{Align2, Atom, Color32, FontFamily, FontId, Response, RichText, Role, Sense, Ui};

/// The font family holding the Lucide glyphs (installed by [`crate::fonts`]).
pub const FAMILY: &str = "lucide";

/// Icon size in small buttons and next to text.
pub const INLINE: f32 = 14.0;

pub const ACTIVITY: char = '\u{e038}';
pub const ARROW_RIGHT: char = '\u{e049}';
pub const ARROW_RIGHT_LEFT: char = '\u{e417}';
pub const ASTERISK: char = '\u{e1ef}';
pub const CHECK: char = '\u{e06c}';
pub const CHEVRON_DOWN: char = '\u{e06d}';
pub const CHEVRON_RIGHT: char = '\u{e06f}';
pub const CHEVRON_UP: char = '\u{e070}';
pub const CIRCLE_CHECK: char = '\u{e226}';
pub const CIRCLE_X: char = '\u{e084}';
pub const COPY: char = '\u{e09e}';
pub const FOLDER: char = '\u{e0d7}';
pub const INFO: char = '\u{e0f9}';
pub const KEY_ROUND: char = '\u{e4a3}';
pub const LAYERS: char = '\u{e529}';
pub const LOCK: char = '\u{e10b}';
pub const MINUS: char = '\u{e11c}';
pub const PENCIL: char = '\u{e1f9}';
pub const PLAY: char = '\u{e13c}';
pub const PLUS: char = '\u{e13d}';
pub const REFRESH_CW: char = '\u{e145}';
pub const ROTATE_CW: char = '\u{e149}';
pub const SEARCH: char = '\u{e151}';
pub const SERVER: char = '\u{e153}';
pub const SETTINGS: char = '\u{e154}';
pub const SHIELD_ALERT: char = '\u{e1fe}';
pub const SQUARE: char = '\u{e167}';
pub const SQUARE_TERMINAL: char = '\u{e20a}';
pub const STAR: char = '\u{e176}';
pub const TRIANGLE_ALERT: char = '\u{e193}';
pub const X: char = '\u{e1b2}';

/// Every icon above, for the glyph check in [`crate::fonts`].
#[cfg(test)]
pub const ALL: &[char] = &[
    ACTIVITY,
    ARROW_RIGHT,
    ARROW_RIGHT_LEFT,
    ASTERISK,
    CHECK,
    CHEVRON_DOWN,
    CHEVRON_RIGHT,
    CHEVRON_UP,
    CIRCLE_CHECK,
    CIRCLE_X,
    COPY,
    FOLDER,
    INFO,
    KEY_ROUND,
    LAYERS,
    LOCK,
    MINUS,
    PENCIL,
    PLAY,
    PLUS,
    REFRESH_CW,
    ROTATE_CW,
    SEARCH,
    SERVER,
    SETTINGS,
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
