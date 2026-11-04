//! Copyable value rows (SPEC.md §10 *Copy on hover*): hovering a row fades
//! in its background and a copy icon 8 px after the value; a long value is
//! cut earlier so the icon always shows, a long key before the value column
//! (the tooltip has both in full); a click anywhere on the row copies the
//! full value. In streamer mode the value is a masked token (hold to peek)
//! and the tooltip never shows it; copying still copies the real value.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use crate::theme::{Palette, timing};
use crate::widgets::{masked, popup, text};
use crate::{icons, motion, privacy};

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
    /// The row sits on a raised card (a masked value's pill is lighter).
    pub on_card: bool,
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
    let masked = privacy::get(ui.ctx()).on;
    let (value_rect, cut) = if masked {
        // Streamer mode: the token (hold to peek); a click on the row still
        // copies the real value.
        let slot = Rect::from_min_size(pos2(value_left, rect.top()), vec2(room, rect.height()));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(slot)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let style = masked::Style {
            font,
            color: row.value_color,
            on_card: row.on_card,
        };
        (masked::value(&mut child, row.value, &style).rect, false)
    } else {
        let (shown, cut) = text::elide(painter, row.value, &font, room);
        let galley = painter.layout_no_wrap(shown, font, row.value_color);
        let value_rect = Rect::from_min_size(
            pos2(value_left, rect.center().y - galley.size().y / 2.0),
            galley.size(),
        );
        painter.galley(value_rect.min, galley, row.value_color);
        (value_rect, cut)
    };
    let painter = ui.painter();
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

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;
    use sshelf_core::redact::DOTS;

    use super::*;
    use crate::privacy::Privacy;
    use crate::test_util::{self, drawn_text, press, release};

    const SECRET: &str = "lk-host-q7.example.net";

    fn harness() -> egui_kittest::Harness<'static, usize> {
        test_util::harness()
            .with_size([480.0, 60.0])
            .build_ui_state(
                |ui, copies: &mut usize| {
                    privacy::set(ui.ctx(), Privacy { on: true });
                    let p = &Palette::ONE_DARK;
                    let copied = row(
                        ui,
                        Row {
                            name: "HostName",
                            key: ("HostName", 120.0, p.dim),
                            key_font: FontId::proportional(13.0),
                            value: SECRET,
                            value_color: p.text,
                            height: 28.0,
                            fill: None,
                            hover_fill: p.separator,
                            hover_outline: false,
                            corners: CornerRadius::same(8),
                            on_card: true,
                        },
                    );
                    *copies += usize::from(copied);
                },
                0,
            )
    }

    /// Masked: only dots are drawn (also no tooltip or accessibility name
    /// has the value); holding the token shows it until the release.
    #[test]
    fn a_masked_value_shows_only_while_held() {
        let mut harness = harness();
        harness.run_steps(2);
        assert!(!drawn_text(&harness).contains(SECRET));
        assert!(harness.query_by_label_contains(SECRET).is_none());
        let token = harness.get_by_label(DOTS).rect().center();

        press(&mut harness, token);
        harness.run_steps(3);
        assert!(
            drawn_text(&harness).contains(SECRET),
            "held: the value shows"
        );

        release(&mut harness, token);
        harness.run_steps(3);
        assert!(
            !drawn_text(&harness).contains(SECRET),
            "released: hidden again"
        );
    }

    /// A click on the row copies the real value, which stays masked.
    #[test]
    fn copy_copies_the_real_value_without_showing_it() {
        let mut harness = harness();
        harness.run_steps(2);
        let key = harness.get_by_label("Copy HostName").rect().left_center() + vec2(20.0, 0.0);
        press(&mut harness, key);
        harness.step();
        release(&mut harness, key);
        harness.step();
        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == SECRET));
        assert!(copied, "the real value is on the clipboard");
        assert_eq!(*harness.state(), 1);
        harness.run_steps(2);
        assert!(!drawn_text(&harness).contains(SECRET));
    }
}
