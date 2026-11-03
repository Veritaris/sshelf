//! Copyable value rows (SPEC.md §10 *Copy on hover*): hovering a row fades
//! in its background and a copy icon 8 px after the value; a long value is
//! cut earlier so the icon always shows, a long key before the value column
//! (the tooltip has both in full); a click anywhere on the row copies the
//! full value.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use crate::theme::{Palette, timing};
use crate::widgets::{popup, text};
use crate::{icons, motion};

/// The icon's size and the space before it.
const ICON: f32 = 14.0;
const ICON_GAP: f32 = 8.0;
/// Least space between a key and its value.
const KEY_GAP: f32 = 12.0;

/// How a row looks.
pub struct Row<'a> {
    /// Name of the value: the left column and the toast's "Copied <name>".
    pub name: &'a str,
    /// The left column's text, its width and colour.
    pub key: (&'a str, f32, Color32),
    pub key_font: FontId,
    pub value: &'a str,
    pub value_color: Color32,
    pub height: f32,
    /// Background while not hovered (zebra rows).
    pub fill: Option<Color32>,
    pub hover_fill: Color32,
    /// A 1 px outline while hovered (table rows).
    pub hover_outline: bool,
    pub corners: CornerRadius,
}

/// Shows the row; returns whether it was clicked (its value is copied).
pub fn row(ui: &mut Ui, row: Row) -> bool {
    let p = &Palette::ONE_DARK;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), row.height), Sense::click());
    let name = format!("Copy {}", row.name);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    let painter = ui.painter();
    if let Some(fill) = row.fill {
        painter.rect_filled(rect, row.corners, fill);
    }
    if hover > 0.0 {
        painter.rect_filled(rect, row.corners, row.hover_fill.gamma_multiply(hover));
        if row.hover_outline {
            painter.rect_stroke(
                rect,
                row.corners,
                Stroke::new(1.0, p.border.gamma_multiply(hover)),
                StrokeKind::Inside,
            );
        }
    }
    let (key, key_width, key_color) = row.key;
    let left = rect.left() + 8.0;
    // Keys are cut too, leaving a gap before the value column.
    let (key_shown, key_cut) = text::elide(painter, key, &row.key_font, key_width - KEY_GAP);
    let key_galley = painter.layout_no_wrap(key_shown, row.key_font, key_color);
    painter.galley(
        pos2(left, rect.center().y - key_galley.size().y / 2.0),
        key_galley,
        key_color,
    );
    let value_left = left + key_width;
    let room = (rect.right() - 8.0 - value_left - ICON_GAP - ICON).max(0.0);
    let font = FontId::monospace(13.0);
    let (shown, cut) = text::elide(painter, row.value, &font, room);
    let galley = painter.layout_no_wrap(shown, font, row.value_color);
    let value_rect = Rect::from_min_size(
        pos2(value_left, rect.center().y - galley.size().y / 2.0),
        galley.size(),
    );
    painter.galley(value_rect.min, galley, row.value_color);
    if hover > 0.0 {
        painter.text(
            pos2(value_rect.right() + ICON_GAP + ICON / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            icons::COPY,
            icons::font(ICON),
            p.accent.gamma_multiply(hover),
        );
    }
    let clicked = response.clicked();
    if clicked {
        ui.ctx().copy_text(row.value.to_string());
    }
    let response: Response = response.on_hover_cursor(CursorIcon::Copy);
    if cut || key_cut {
        let (key, value) = (key.to_string(), row.value.to_string());
        popup::tip_below(response, |ui| {
            if key_cut {
                ui.label(egui::RichText::new(key).color(key_color));
            }
            ui.label(egui::RichText::new(value).monospace());
        });
    }
    clicked
}
