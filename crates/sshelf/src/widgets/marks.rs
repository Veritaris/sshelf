//! Status marks (SPEC.md §10 *Visual design*): a filled dot is a settled
//! state (running green, stopped grey, failed red). They are painted shapes,
//! never glyphs.

use egui::{Color32, Painter, Pos2, Response, Sense, Ui, Vec2};

/// Diameter of a status dot.
pub const DOT: f32 = 8.0;

/// A status dot in the layout; hover it for a tooltip.
pub fn dot(ui: &mut Ui, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(DOT), Sense::hover());
    paint_dot(ui.painter(), rect.center(), DOT, color);
    response
}

/// A dot of `diameter` at `center`, e.g. on top of another shape.
pub fn paint_dot(painter: &Painter, center: Pos2, diameter: f32, color: Color32) {
    painter.circle_filled(center, diameter / 2.0, color);
}
