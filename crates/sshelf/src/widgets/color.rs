//! A small colour picker (a custom host colour, SPEC.md §10): a saturation
//! / brightness square, a hue bar and a `#rrggbb` field. egui's own picker
//! has an emoji copy button, which the icon rule doesn't allow.

use egui::ecolor::Hsva;
use egui::epaint::{Mesh, Vertex};
use egui::{
    Color32, CursorIcon, Margin, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui, WidgetInfo,
    pos2, vec2,
};

use crate::theme::{Palette, radius};

const WIDTH: f32 = 200.0;
const SQUARE_HEIGHT: f32 = 120.0;
const BAR_HEIGHT: f32 = 12.0;
/// Mesh cells across and down the square, and along the hue bar.
const CELLS: (usize, usize) = (24, 12);
const HUES: usize = 36;

/// The colour being picked, and the hex field's text.
#[derive(Debug, Clone, PartialEq)]
pub struct Picking {
    hsva: Hsva,
    hex: String,
}

impl Picking {
    pub fn new(color: Color32) -> Self {
        Self {
            hsva: Hsva::from(color),
            hex: hex(color),
        }
    }

    pub fn color(&self) -> Color32 {
        Color32::from(self.hsva).to_opaque()
    }
}

/// `#rrggbb` of `color`.
pub fn hex(color: Color32) -> String {
    let [r, g, b, _] = color.to_array();
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `#rrggbb` (or without the `#`) as a colour.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// The picker; changes `picking` as it is used.
pub fn picker(ui: &mut Ui, picking: &mut Picking) {
    let p = &Palette::ONE_DARK;
    ui.spacing_mut().item_spacing.y = 8.0;

    // Saturation across, brightness down.
    let (rect, response) =
        ui.allocate_exact_size(vec2(WIDTH, SQUARE_HEIGHT), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Slider, true, "Saturation and brightness"));
    if let Some(pos) = pointer(&response) {
        picking.hsva.s = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        picking.hsva.v = 1.0 - ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        picking.hex = hex(picking.color());
    }
    let h = picking.hsva.h;
    paint_gradient(ui, rect, CELLS, |x, y| Hsva::new(h, x, 1.0 - y, 1.0));
    let marker = pos2(
        rect.left() + picking.hsva.s * rect.width(),
        rect.top() + (1.0 - picking.hsva.v) * rect.height(),
    );
    ui.painter()
        .circle_stroke(marker, 5.0, Stroke::new(2.0, Color32::WHITE));
    ui.painter()
        .circle_stroke(marker, 6.5, Stroke::new(1.0, Color32::BLACK));
    response.on_hover_cursor(CursorIcon::Crosshair);

    // Hue.
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, BAR_HEIGHT), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Slider, true, "Hue"));
    if let Some(pos) = pointer(&response) {
        picking.hsva.h = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        // A grey has no hue to show: give it some colour to move.
        if picking.hsva.s < 0.05 {
            picking.hsva.s = 0.6;
        }
        if picking.hsva.v < 0.05 {
            picking.hsva.v = 0.8;
        }
        picking.hex = hex(picking.color());
    }
    paint_gradient(ui, rect, (HUES, 1), |x, _| Hsva::new(x, 1.0, 1.0, 1.0));
    let x = rect.left() + picking.hsva.h * rect.width();
    let knob = Rect::from_center_size(pos2(x, rect.center().y), vec2(4.0, BAR_HEIGHT + 4.0));
    ui.painter().rect(
        knob,
        1,
        Color32::WHITE,
        Stroke::new(1.0, Color32::BLACK),
        egui::StrokeKind::Outside,
    );
    response.on_hover_cursor(CursorIcon::PointingHand);

    // The colour and its hex value.
    ui.horizontal(|ui| {
        let (swatch, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
        ui.painter()
            .rect_filled(swatch, radius::CONTROL, picking.color());
        let edit = ui.add(
            egui::TextEdit::singleline(&mut picking.hex)
                .hint_text("#rrggbb")
                .font(egui::FontId::monospace(13.0))
                .desired_width(WIDTH - 28.0 - 8.0)
                .margin(Margin::symmetric(8, 6))
                .text_color(p.text),
        );
        if edit.changed()
            && let Some(color) = parse_hex(&picking.hex)
        {
            let hue = picking.hsva.h;
            picking.hsva = Hsva::from(color);
            // Keep the hue of a grey, so the square doesn't jump to red.
            if picking.hsva.s == 0.0 {
                picking.hsva.h = hue;
            }
        }
    });
}

/// Where the pointer presses or drags on `response`'s widget.
fn pointer(response: &Response) -> Option<Pos2> {
    (response.dragged() || response.clicked())
        .then(|| response.interact_pointer_pos())
        .flatten()
}

/// `rect` filled with `color(x, y)` (both 0…1), as a mesh of `cells`.
fn paint_gradient(
    ui: &Ui,
    rect: Rect,
    (columns, rows): (usize, usize),
    color: impl Fn(f32, f32) -> Hsva,
) {
    let mut mesh = Mesh::default();
    for row in 0..=rows {
        for column in 0..=columns {
            let (x, y) = (column as f32 / columns as f32, row as f32 / rows as f32);
            mesh.vertices.push(Vertex {
                pos: pos2(
                    rect.left() + x * rect.width(),
                    rect.top() + y * rect.height(),
                ),
                uv: egui::epaint::WHITE_UV,
                color: Color32::from(color(x, y)),
            });
        }
    }
    let at = |column: usize, row: usize| (row * (columns + 1) + column) as u32;
    for row in 0..rows {
        for column in 0..columns {
            let (a, b) = (at(column, row), at(column + 1, row));
            let (c, d) = (at(column, row + 1), at(column + 1, row + 1));
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, d, c);
        }
    }
    ui.painter().add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let color = Color32::from_rgb(0xc6, 0x78, 0xdd);
        assert_eq!(hex(color), "#c678dd");
        assert_eq!(parse_hex("#C678DD"), Some(color));
        assert_eq!(parse_hex(" c678dd "), Some(color));
        assert_eq!(parse_hex("#c678d"), None);
        assert_eq!(parse_hex("#c678dg"), None);
    }

    #[test]
    fn picking_keeps_the_colour() {
        let color = Color32::from_rgb(0x61, 0xaf, 0xef);
        assert_eq!(Picking::new(color).color(), color);
    }
}
