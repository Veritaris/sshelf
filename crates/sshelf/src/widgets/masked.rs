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

use crate::privacy::{self, AliasLook};
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
    let segments = privacy.redactor.segments(text, privacy.hide_aliases);
    for (index, segment) in segments.iter().enumerate() {
        let (shown, format) = match segment {
            Segment::Plain(s) => ((*s).to_string(), format.clone()),
            Segment::Token(_, value) | Segment::Alias(value, _) if held == Some(index) => (
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
            // *Hide host aliases and tags*: "host N", underlined in Blue.
            Segment::Alias(_, n) => (
                format!("host {n}"),
                egui::TextFormat {
                    underline: egui::Stroke::new(1.0, p.blue),
                    ..format.clone()
                },
            ),
        };
        let count = shown.chars().count();
        if !matches!(segment, Segment::Plain(_)) {
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

/// A host alias: as it is; "host N" with a dotted Blue underline while
/// aliases are hidden (hold to see it); the dots whenever streamer mode is
/// on if it is an IP address or equals its HostName.
pub fn alias(ui: &mut Ui, alias: &str, style: &Style) -> Response {
    match privacy::get(ui.ctx()).alias_look(alias) {
        AliasLook::Shown => ui.add(
            egui::Label::new(
                egui::RichText::new(alias)
                    .font(style.font.clone())
                    .color(style.color),
            )
            .selectable(false),
        ),
        AliasLook::Dots => token(ui, DOTS, alias, style.on_card, true),
        AliasLook::Numbered(n) => numbered(ui, &format!("host {n}"), alias, style),
    }
}

/// "host N" in the text's own style with a dotted Blue underline; held, the
/// alias on the yellow tint.
fn numbered(ui: &mut Ui, shown: &str, real: &str, style: &Style) -> Response {
    let p = &Palette::ONE_DARK;
    let id = ui.next_auto_id().with("masked_alias");
    let was = held(ui, id);
    let (text, color) = if was {
        (real, p.yellow)
    } else {
        (shown, style.color)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), style.font.clone(), color);
    let pad = if was { 4.0 } else { 0.0 };
    let size = vec2(galley.size().x + pad * 2.0, galley.size().y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let name = shown.to_string();
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    let painter = ui.painter();
    if was {
        painter.rect_filled(rect, CORNER, p.yellow_tint);
    } else {
        // Dotted: 1 px dots, 3 px apart, just under the text.
        let y = rect.bottom() - 0.5;
        let mut x = rect.left();
        while x < rect.right() {
            painter.rect_filled(
                egui::Rect::from_min_size(pos2(x, y - 0.5), vec2(1.0, 1.0)),
                0.0,
                p.blue,
            );
            x += 3.0;
        }
    }
    painter.galley(pos2(rect.left() + pad, rect.top()), galley, color);
    if remember(ui, id, &response, was) {
        response.on_hover_cursor(CursorIcon::Grabbing)
    } else {
        let response = response.on_hover_cursor(CursorIcon::Grab);
        popup::tip(response, |ui| {
            ui.label("Hold to reveal");
        })
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

    /// A redactor over a config of `web` and an IP alias.
    fn redactor() -> std::sync::Arc<sshelf_core::redact::Redactor> {
        use sshconf::{ConfigSet, HostList, ResolveOptions};
        use sshelf_core::redact::{Redactor, Sources};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config");
        std::fs::write(
            &path,
            "Host web\n    HostName w.example.net\nHost 10.1.2.3\n",
        )
        .unwrap();
        let set = ConfigSet::load(&path, &ResolveOptions::new(tmp.path())).unwrap();
        let hosts = HostList::from_set(&set);
        std::sync::Arc::new(Redactor::build(Sources {
            set: Some(&set),
            hosts: Some(&hosts),
            tunnels: &Default::default(),
            home: tmp.path(),
            local_user: None,
        }))
    }

    /// Hidden aliases: "host N" until held; an IP alias is dots even when
    /// aliases are shown.
    #[test]
    fn aliases_show_as_host_n_and_peek_while_held() {
        let redactor = redactor();
        let mut harness = test_util::harness()
            .with_size([300.0, 60.0])
            .build_ui_state(
                move |ui, hide: &mut bool| {
                    let on = Privacy {
                        on: true,
                        redactor: redactor.clone(),
                        hide_aliases: *hide,
                    };
                    privacy::set(ui.ctx(), on);
                    let style = Style {
                        font: FontId::proportional(13.0),
                        color: Palette::ONE_DARK.text,
                        on_card: false,
                    };
                    alias(ui, "web", &style);
                    alias(ui, "10.1.2.3", &style);
                },
                false,
            );
        harness.run_steps(2);
        let drawn = drawn_text(&harness);
        assert!(
            drawn.contains("web") && !drawn.contains("10.1.2.3"),
            "{drawn}"
        );

        *harness.state_mut() = true;
        harness.run_steps(2);
        let drawn = drawn_text(&harness);
        assert!(
            drawn.contains("host 1") && !drawn.contains("web"),
            "{drawn}"
        );
        let at = harness.get_by_label("host 1").rect().center();
        press(&mut harness, at);
        harness.run_steps(3);
        assert!(
            drawn_text(&harness).contains("web"),
            "held: the alias shows"
        );
        release(&mut harness, at);
        harness.run_steps(3);
        assert!(!drawn_text(&harness).contains("web"));
    }

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
