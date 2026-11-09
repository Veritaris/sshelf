//! Lucide icons (SPEC.md §10 *Visual design*): codepoints in the bundled
//! `lucide.ttf` (from `font/codepoints.json` of `lucide-static`, see
//! `assets/fonts/README.md`) and helpers to show them. Icons are painted, so
//! they never become part of a widget's accessible name.
//!
//! The bundled font holds only the icons named here: after adding one, run
//! `assets/fonts/subset_lucide.py` and add it to [`ALL`] (tests check both).

use egui::{Align2, Color32, FontFamily, FontId, Response, RichText, Sense, Ui};

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
pub const CIRCLE_ARROW_UP: char = '\u{e07b}';
pub const CIRCLE_CHECK: char = '\u{e226}';
pub const CIRCLE_X: char = '\u{e084}';
pub const COMPASS: char = '\u{e09b}';
pub const COPY: char = '\u{e09e}';
pub const CORNER_DOWN_RIGHT: char = '\u{e0a2}';
pub const EXTERNAL_LINK: char = '\u{e0b9}';
pub const EYE: char = '\u{e0ba}';
pub const EYE_OFF: char = '\u{e0bb}';
pub const FILE_TEXT: char = '\u{e0cc}';
pub const FOLDER: char = '\u{e0d7}';
pub const GIT_COMMIT_HORIZONTAL: char = '\u{e0e3}';
pub const GRIP_VERTICAL: char = '\u{e0eb}';
pub const INFO: char = '\u{e0f9}';
pub const KEY_ROUND: char = '\u{e4a3}';
pub const LAYERS: char = '\u{e529}';
pub const LOCK: char = '\u{e10b}';
pub const MINUS: char = '\u{e11c}';
pub const MOUSE_POINTER_CLICK: char = '\u{e120}';
pub const PENCIL: char = '\u{e1f9}';
pub const PLAY: char = '\u{e13c}';
pub const PLUS: char = '\u{e13d}';
pub const POWER: char = '\u{e140}';
pub const REFRESH_CW: char = '\u{e145}';
pub const ROTATE_CW: char = '\u{e149}';
pub const SEARCH: char = '\u{e151}';
pub const SERVER: char = '\u{e153}';
pub const SETTINGS: char = '\u{e154}';
pub const SHIELD_ALERT: char = '\u{e1fe}';
pub const SHIELD_CHECK: char = '\u{e1ff}';
pub const SHIP_WHEEL: char = '\u{e502}';
pub const SKIP_FORWARD: char = '\u{e160}';
pub const SQUARE: char = '\u{e167}';
pub const SQUARE_TERMINAL: char = '\u{e20a}';
pub const STAR: char = '\u{e176}';
pub const TERMINAL: char = '\u{e181}';
pub const TRIANGLE_ALERT: char = '\u{e193}';
pub const UNDO_2: char = '\u{e2a1}';
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
    CIRCLE_ARROW_UP,
    CIRCLE_CHECK,
    CIRCLE_X,
    COMPASS,
    COPY,
    CORNER_DOWN_RIGHT,
    EXTERNAL_LINK,
    EYE,
    EYE_OFF,
    FILE_TEXT,
    FOLDER,
    GIT_COMMIT_HORIZONTAL,
    GRIP_VERTICAL,
    INFO,
    KEY_ROUND,
    LAYERS,
    LOCK,
    MINUS,
    MOUSE_POINTER_CLICK,
    PENCIL,
    PLAY,
    PLUS,
    POWER,
    REFRESH_CW,
    ROTATE_CW,
    SEARCH,
    SERVER,
    SETTINGS,
    SHIELD_ALERT,
    SHIELD_CHECK,
    SHIP_WHEEL,
    SKIP_FORWARD,
    SQUARE,
    SQUARE_TERMINAL,
    STAR,
    TERMINAL,
    TRIANGLE_ALERT,
    UNDO_2,
    X,
];

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY.into()))
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

fn paint(ui: &Ui, center: egui::Pos2, icon: char, size: f32, color: Color32) {
    centered(ui.painter(), center, icon, size, color);
}

/// Paints `icon` with its drawn shape centred on `center`. Centring the
/// text box instead leaves glyphs off by the font's ascent and side bearing.
pub fn centered(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: char,
    size: f32,
    color: Color32,
) {
    let galley = painter.layout_no_wrap(icon.to_string(), font(size), color);
    let ink = galley.rows.first().and_then(|row| {
        let glyph = row.glyphs.first()?;
        let min = row.pos + glyph.pos.to_vec2() + glyph.uv_rect.offset;
        Some(egui::Rect::from_min_size(min, glyph.uv_rect.size))
    });
    match ink {
        Some(ink) if ink.area() > 0.0 => {
            painter.galley(center - ink.center().to_vec2(), galley, color);
        }
        _ => {
            painter.text(center, Align2::CENTER_CENTER, icon, font(size), color);
        }
    }
}
