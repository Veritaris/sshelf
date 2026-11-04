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
use sshelf_core::redact::{DOTS, Segment};

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
    token(ui, DOTS, value, style.on_card, true)
}

/// Free text from data (logs, errors, notes, toasts, tooltips): what the
/// filter matches shows as a typed label (`ip`, `host`, `port`…) on the
/// pill colour, so the sentence still reads. Pressing a label shows that
/// value while held. `wrap`: wraps at the available width (else one line,
/// cut with "…").
pub fn text(ui: &mut Ui, text: &str, style: &Style, wrap: bool) -> Response {
    let privacy = privacy::get(ui.ctx());
    let plain = |job: egui::text::LayoutJob| egui::Label::new(job).selectable(false);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    if !wrap {
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
    }
    let format = egui::TextFormat::simple(style.font.clone(), style.color);
    if !privacy.on {
        job.append(text, 0.0, format);
        return ui.add(plain(job));
    }
    let p = &Palette::ONE_DARK;
    let id = ui.next_auto_id().with("masked_text");
    let held: Option<usize> = ui.ctx().data(|d| d.get_temp(id));
    let label_font = FontId::monospace(11.0);
    // Where each token's text lies in the galley (chars), by segment.
    let mut tokens: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
    let mut chars = 0;
    let segments = privacy.redactor.segments(text, false);
    for (index, segment) in segments.iter().enumerate() {
        let (shown, format) = match segment {
            Segment::Plain(s) => ((*s).to_string(), format.clone()),
            Segment::Token(_, value) if held == Some(index) => (
                (*value).to_string(),
                egui::TextFormat {
                    background: p.yellow_tint,
                    ..egui::TextFormat::simple(style.font.clone(), p.yellow)
                },
            ),
            Segment::Token(kind, _) => (
                format!("\u{2009}{}\u{2009}", kind.label()),
                egui::TextFormat {
                    background: if style.on_card {
                        p.separator
                    } else {
                        p.surface
                    },
                    ..egui::TextFormat::simple(label_font.clone(), p.dim)
                },
            ),
            // Hidden aliases come with *Hide host aliases and tags*.
            Segment::Alias(alias, _) => ((*alias).to_string(), format.clone()),
        };
        let count = shown.chars().count();
        if matches!(segment, Segment::Token(..)) {
            tokens.push((index, chars..chars + count));
        }
        chars += count;
        job.append(&shown, 0.0, format);
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click_and_drag());
    let name = galley.text().to_string();
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    // The token under the pointer, if any.
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let under = pointer.filter(|pos| rect.contains(*pos)).and_then(|pos| {
        let at = galley.cursor_from_pos(pos - rect.min).index.0;
        tokens
            .iter()
            .find(|(_, range)| range.contains(&at))
            .map(|(index, _)| *index)
    });
    ui.painter().galley(rect.min, galley, style.color);
    // Shown while pressed on it; leaving the token hides it.
    let now = if response.is_pointer_button_down_on() {
        under
    } else {
        None
    };
    if now != held {
        ui.ctx().data_mut(|d| {
            if let Some(index) = now {
                d.insert_temp(id, index);
            } else {
                d.remove::<usize>(id);
            }
        });
        ui.ctx().request_repaint();
    }
    match (now, under) {
        (Some(_), _) => response.on_hover_cursor(CursorIcon::Grabbing),
        (None, Some(_)) => popup::tip(response.on_hover_cursor(CursorIcon::Grab), |ui| {
            ui.label("Hold to reveal");
        }),
        (None, None) => response,
    }
}

/// The dots alone, which can't be peeked (the tray menu).
pub fn dots(ui: &mut Ui, on_card: bool) -> Response {
    token(ui, DOTS, DOTS, on_card, false)
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

/// A pill showing `shown`, and `real` while held (if `peekable`).
fn token(ui: &mut Ui, shown: &str, real: &str, on_card: bool, peekable: bool) -> Response {
    let p = &Palette::ONE_DARK;
    let id = ui.next_auto_id().with("masked");
    let was = peekable && held(ui, id);
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
    let sense = if peekable {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let name = shown.to_string();
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    let painter = ui.painter();
    painter.rect_filled(rect, CORNER, fill);
    painter.galley(
        pos2(rect.left() + PAD, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if !peekable {
        return response;
    }
    if remember(ui, id, &response, was) {
        response.on_hover_cursor(CursorIcon::Grabbing)
    } else {
        let response = response.on_hover_cursor(CursorIcon::Grab);
        popup::tip(response, |ui| {
            ui.label("Hold to reveal");
        })
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::privacy::Privacy;
    use crate::test_util::{self, drawn_text, press, release};

    const LINE: &str = "connect to 10.77.3.41 port 52817 failed";

    /// Free text: each match is its typed label; pressing a label shows that
    /// value until the release, and only that one.
    #[test]
    fn free_text_shows_labels_and_peeks_one_value() {
        let mut harness = test_util::harness()
            .with_size([480.0, 40.0])
            .build_ui(|ui| {
                let on = Privacy {
                    on: true,
                    ..Privacy::default()
                };
                privacy::set(ui.ctx(), on);
                let style = Style {
                    font: FontId::monospace(12.0),
                    color: Palette::ONE_DARK.text,
                    on_card: false,
                };
                text(ui, LINE, &style, false);
            });
        harness.run_steps(2);
        let drawn = drawn_text(&harness);
        assert!(drawn.contains("connect to \u{2009}ip\u{2009} port \u{2009}port\u{2009} failed"));
        assert!(
            !drawn.contains("10.77") && !drawn.contains("52817"),
            "{drawn}"
        );

        // The `ip` label sits right after "connect to ".
        let rect = harness.get_by_label_contains("connect to").rect();
        let at = egui::pos2(rect.left() + 12.0 * 0.6 * 12.5, rect.center().y);
        press(&mut harness, at);
        harness.run_steps(3);
        let drawn = drawn_text(&harness);
        assert!(
            drawn.contains("10.77.3.41") && !drawn.contains("52817"),
            "{drawn}"
        );
        release(&mut harness, at);
        harness.run_steps(3);
        assert!(!drawn_text(&harness).contains("10.77"));
    }
}
