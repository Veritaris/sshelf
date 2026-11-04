//! Masked values (SPEC.md §10 *Streamer mode*, boards Streamer-Mode and
//! Streamer-States): a field value shows as "•••••" (always five dots,
//! never the real length) in a pill.
//!
//! Pressing a token shows the real value while the button is held, with the
//! yellow tint; releasing, or the pointer leaving it, hides it again. The
//! cursor is `Grab` (`Grabbing` while held), and after the standard delay a
//! tooltip says "Hold to reveal". Accessibility names are the masked text,
//! never the value. With streamer mode off a value draws as plain text.

use egui::{Color32, CursorIcon, FontId, Id, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use sshelf_core::redact::DOTS;

use crate::privacy;
use crate::theme::Palette;
use crate::widgets::popup;

/// Token pill: height, horizontal padding, corners.
const HEIGHT: f32 = 18.0;
const PAD: f32 = 6.0;
const CORNER: u8 = 8;

/// How a value is drawn when it isn't masked, and where it sits.
#[derive(Clone)]
pub struct Style {
    pub font: FontId,
    pub color: Color32,
    /// On a raised card: the pill uses the separator colour (`#353B45`).
    pub on_card: bool,
}

/// A field value: the value itself, or (streamer mode) the dots.
pub fn value(ui: &mut Ui, value: &str, style: &Style) -> Response {
    if !privacy::get(ui.ctx()).on {
        return ui.add(
            egui::Label::new(
                egui::RichText::new(value)
                    .font(style.font.clone())
                    .color(style.color),
            )
            .selectable(false),
        );
    }
    token(ui, DOTS, value, style.on_card)
}

/// Whether the token with `id` was held last frame: its size follows, so a
/// pill only ever has the width of the dots or, while held, of the value.
fn held(ui: &Ui, id: Id) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false)
}

fn remember(ui: &Ui, id: Id, response: &Response, was: bool) -> bool {
    let now = response.is_pointer_button_down_on() && response.contains_pointer();
    if now != was {
        ui.ctx().data_mut(|d| d.insert_temp(id, now));
        ui.ctx().request_repaint();
    }
    now
}

/// A pill showing `shown`, and `real` while held.
fn token(ui: &mut Ui, shown: &str, real: &str, on_card: bool) -> Response {
    let p = &Palette::ONE_DARK;
    let id = ui.next_auto_id().with("masked");
    let was = held(ui, id);
    let font = FontId::monospace(12.0);
    let (text, color, fill) = if was {
        (real, p.yellow, p.yellow_tint)
    } else {
        (shown, p.dim, if on_card { p.separator } else { p.surface })
    };
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font.clone(), color);
    if !was {
        // The dots get 0.06 em of letter spacing; a peeked value none.
        if let Some(section) = job.sections.first_mut() {
            section.format.extra_letter_spacing = font.size * 0.06;
        }
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let size = vec2(galley.size().x + PAD * 2.0, HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let name = shown.to_string();
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    let now = remember(ui, id, &response, was);
    let painter = ui.painter();
    painter.rect_filled(rect, CORNER, fill);
    painter.galley(
        pos2(rect.left() + PAD, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if now {
        response.on_hover_cursor(CursorIcon::Grabbing)
    } else {
        let response = response.on_hover_cursor(CursorIcon::Grab);
        popup::tip(response, |ui| {
            ui.label("Hold to reveal");
        })
    }
}
