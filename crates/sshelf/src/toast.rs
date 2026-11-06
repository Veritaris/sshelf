//! Toasts: short success messages ("Settings saved") in the bottom-right
//! corner, above the status bar (SPEC.md §10 *Interaction and motion*). Each
//! fades in with a 6 px rise, stays 3 s, then fades out; hovering one pauses
//! it. Errors stay in the status bar until dismissed. A toast's clock only
//! runs while it is drawn, so one raised while the window is hidden is still
//! seen. Waiting is one scheduled repaint, not a polling timer. Every toast
//! has a close button at its right end (§10 *Layout*, *Feedback*): closing
//! removes it at once and the others close the gap.
//!
//! Sticky toasts (§10 *Update check*: a newer version, the first-run note)
//! never fade: they go when closed or when one of their buttons is pressed,
//! which [`Toasts::show`] reports. They sit at the bottom, 400 px wide; the
//! others stack above them.

use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Area, Color32, CursorIcon, FontId, Frame, Id, Margin, Order, Response, Role, Sense,
    Stroke, Ui, WidgetInfo, vec2,
};

use crate::theme::{Palette, radius, timing};
use crate::widgets::button::Button;
use crate::widgets::{masked, text};
use crate::{icons, motion, privacy, status_bar};

/// Older toasts give way beyond this many.
const MAX: usize = 3;
/// Distance from the window's right edge and from the status bar.
const MARGIN: f32 = 20.0;
const WIDTH: f32 = 340.0;
/// Space between stacked toasts.
const GAP: f32 = 8.0;
/// How far a toast rises while it fades in.
const RISE: f32 = 6.0;
/// The longest step a toast's clock takes in one frame, so frames missed
/// while the window was hidden don't make it vanish at once.
const MAX_STEP: f64 = 0.1;
/// The close button's side and icon size.
const CLOSE: f32 = 24.0;
const CLOSE_ICON: f32 = 14.0;
/// The close button's accessible name (never the toast's text, which can be
/// an alias streamer mode hides).
pub const CLOSE_NAME: &str = "Close notification";
const STICKY_WIDTH: f32 = 400.0;

struct Toast {
    id: u64,
    text: String,
    /// A value after the text, in mono (an alias, a path).
    value: Option<String>,
    /// The icon before it and its colour (a green check by default).
    icon: (char, Color32),
    /// A *Copy* link that copies the value.
    copy: bool,
    /// Seconds it has been shown (not counting hovered time).
    age: f64,
    /// `egui` time of the last frame that drew it.
    last: Option<f64>,
}

/// Which sticky toast it is; there is at most one of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickyKind {
    /// "sshelf 0.4.0 is available".
    Update,
    /// "sshelf now checks for new versions" (existing users, once).
    CheckNote,
}

/// A sticky toast's button: secondary, or ghost.
pub struct StickyAction {
    pub label: &'static str,
    pub icon: Option<char>,
    pub ghost: bool,
}

/// A toast that stays: the icon (18 px), a title and a dim line (parts in
/// mono where flagged), and its buttons.
pub struct Sticky {
    pub kind: StickyKind,
    pub icon: (char, Color32),
    pub title: Vec<(String, bool)>,
    pub detail: Vec<(String, bool)>,
    pub actions: Vec<StickyAction>,
}

/// What happened to a sticky toast this frame; it is gone after either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickyClick {
    /// Its button at this index.
    Action(StickyKind, usize),
    Closed(StickyKind),
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
    /// Oldest first; drawn at the bottom of the stack.
    sticky: Vec<(Sticky, f64, Option<f64>)>,
    next_id: u64,
}

/// Where a toast is in its life: opacity and how far below its place it is.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    opacity: f32,
    offset: f32,
}

/// The look of a toast `age` seconds old with fades of `fade` seconds;
/// `None` once it is gone.
fn look(age: f64, fade: f32) -> Option<Look> {
    let (fade, hold) = (f64::from(fade), f64::from(timing::TOAST_HOLD));
    let shown = |opacity: f32| Look {
        opacity,
        offset: RISE * (1.0 - opacity),
    };
    if age < fade {
        Some(shown(motion::ease_out((age / fade) as f32)))
    } else if age < fade + hold {
        Some(shown(1.0))
    } else if age < fade + hold + fade {
        let t = ((age - fade - hold) / fade) as f32;
        Some(Look {
            opacity: 1.0 - motion::ease_out(t),
            offset: 0.0,
        })
    } else {
        None
    }
}

/// The toast's text, with its value (if any) in mono after it.
fn message(text: &str, value: Option<&str>) -> LayoutJob {
    let p = &Palette::ONE_DARK;
    let mut job = LayoutJob::default();
    let format = |font: FontId| TextFormat::simple(font, p.text);
    job.append(text, 0.0, format(FontId::proportional(13.0)));
    if let Some(value) = value {
        job.append(" ", 0.0, format(FontId::proportional(13.0)));
        job.append(value, 0.0, format(FontId::monospace(13.0)));
    }
    job
}

/// Lucide `x` in a 24 px ghost button: secondary grey, the hover background
/// and strong text fading in over 80 ms.
fn close_button(ui: &mut Ui) -> Response {
    let p = &Palette::ONE_DARK;
    let (rect, response) = ui.allocate_exact_size(vec2(CLOSE, CLOSE), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, CLOSE_NAME));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        radius::CONTROL,
        Color32::TRANSPARENT.lerp_to_gamma(p.surface, hover),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icons::X,
        icons::font(CLOSE_ICON),
        p.dim.lerp_to_gamma(p.text_strong, hover),
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A sticky toast's frame and content: the icon at the top left, the title
/// and its line, the buttons under them, the close button at the top right.
fn show_sticky(ui: &mut Ui, toast: &Sticky) -> Option<StickyClick> {
    let p = &Palette::ONE_DARK;
    let mut click = None;
    Frame::new()
        .fill(p.bg_dark)
        .stroke(Stroke::new(1.0, p.selection))
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            let inner = STICKY_WIDTH - 24.0;
            ui.set_width(inner);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let (icon, color) = toast.icon;
                ui.vertical(|ui| {
                    ui.add_space(1.0);
                    icons::show(ui, icon, 18.0, color);
                });
                let body = inner - 18.0 - CLOSE - 20.0;
                ui.vertical(|ui| {
                    ui.set_width(body);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let parts = |parts: &[(String, bool)], size, color| {
                        let parts: Vec<(&str, bool)> =
                            parts.iter().map(|(t, m)| (t.as_str(), *m)).collect();
                        text::parts(&parts, size, color)
                    };
                    ui.add(egui::Label::new(parts(&toast.title, 13.0, p.text_strong)).wrap());
                    ui.add(egui::Label::new(parts(&toast.detail, 12.0, p.dim)).wrap());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (index, action) in toast.actions.iter().enumerate() {
                            let mut button = Button::new(action.label).small();
                            if let Some(icon) = action.icon {
                                button = button.icon(icon);
                            }
                            if action.ghost {
                                button = button.ghost();
                            }
                            if ui.add(button).clicked() {
                                click = Some(StickyClick::Action(toast.kind, index));
                            }
                        }
                    });
                });
                if close_button(ui).clicked() {
                    click = Some(StickyClick::Closed(toast.kind));
                }
            });
        });
    click
}

impl Toasts {
    pub fn push(&mut self, text: impl Into<String>) {
        self.add(
            text.into(),
            None,
            (icons::CIRCLE_CHECK, Palette::ONE_DARK.green),
        );
    }

    /// `text` followed by `value` in mono, e.g. "Opened a terminal for"
    /// and an alias.
    pub fn push_value(&mut self, text: impl Into<String>, value: impl Into<String>) {
        let check = (icons::CIRCLE_CHECK, Palette::ONE_DARK.green);
        self.add(text.into(), Some(value.into()), check);
    }

    /// `text` after `icon` in `color` (streamer mode's on and off).
    pub fn push_icon(&mut self, text: impl Into<String>, icon: char, color: Color32) {
        self.add(text.into(), None, (icon, color));
    }

    /// Like [`Toasts::push_value`], with *Copy* for the value (a kubectl
    /// tunnel's "web api is running on 127.0.0.1:18080").
    pub fn push_copy(&mut self, text: impl Into<String>, value: impl Into<String>) {
        let check = (icons::CIRCLE_CHECK, Palette::ONE_DARK.green);
        self.add(text.into(), Some(value.into()), check);
        if let Some(toast) = self.items.last_mut() {
            toast.copy = true;
        }
    }

    /// Shows `toast`, in place of a sticky toast of its kind.
    pub fn push_sticky(&mut self, toast: Sticky) {
        self.remove_sticky(toast.kind);
        self.sticky.push((toast, 0.0, None));
    }

    pub fn remove_sticky(&mut self, kind: StickyKind) {
        self.sticky.retain(|(t, _, _)| t.kind != kind);
    }

    fn add(&mut self, text: String, value: Option<String>, icon: (char, Color32)) {
        self.items.push(Toast {
            id: self.next_id,
            text,
            value,
            icon,
            copy: false,
            age: 0.0,
            last: None,
        });
        self.next_id += 1;
        if self.items.len() > MAX {
            self.items.remove(0);
        }
    }

    /// Call once per frame from `App::ui`; `sticky` false leaves the sticky
    /// toasts waiting (the tour). Returns what was done to sticky toasts.
    pub fn show(&mut self, ctx: &egui::Context, sticky: bool) -> Vec<StickyClick> {
        let now = ctx.input(|i| i.time);
        let fade = motion::duration(ctx, timing::TOAST_FADE);
        let hold_end = f64::from(fade) + f64::from(timing::TOAST_HOLD);
        let p = &Palette::ONE_DARK;
        // Newest at the bottom, stacked upwards; sticky ones below the rest.
        let mut bottom = status_bar::HEIGHT + MARGIN;
        let mut clicks = Vec::new();
        if sticky {
            for (toast, age, last) in self.sticky.iter_mut().rev() {
                // Rises in like the others, then stays.
                let look = look(age.min(f64::from(fade)), fade).unwrap_or(Look {
                    opacity: 1.0,
                    offset: 0.0,
                });
                let area = Area::new(Id::unique(("sticky_toast", toast.kind as u8)))
                    .order(Order::Foreground)
                    .anchor(Align2::RIGHT_BOTTOM, vec2(-MARGIN, -(bottom - look.offset)))
                    .fade_in(false)
                    .show(ctx, |ui| {
                        ui.multiply_opacity(look.opacity);
                        show_sticky(ui, toast)
                    });
                if let Some(click) = area.inner {
                    clicks.push(click);
                }
                bottom += area.response.rect.height() + GAP;
                let step = last.map_or(0.0, |last| (now - last).clamp(0.0, MAX_STEP));
                *last = Some(now);
                if *age < f64::from(fade) {
                    *age += step;
                    ctx.request_repaint();
                }
            }
            self.sticky.retain(|(t, _, _)| {
                !clicks.iter().any(|c| {
                    matches!(c, StickyClick::Action(k, _) | StickyClick::Closed(k) if *k == t.kind)
                })
            });
        }
        let mut next_wake = f64::INFINITY;
        let mut closed = None;
        for toast in self.items.iter_mut().rev() {
            let Some(look) = look(toast.age, fade) else {
                continue;
            };
            let area = Area::new(Id::unique(("toast", toast.id)))
                .order(Order::Foreground)
                .anchor(Align2::RIGHT_BOTTOM, vec2(-MARGIN, -(bottom - look.offset)))
                .fade_in(false)
                .show(ctx, |ui| {
                    ui.multiply_opacity(look.opacity);
                    Frame::new()
                        .fill(p.bg_dark)
                        .stroke(Stroke::new(1.0, p.selection))
                        .corner_radius(radius::CONTROL)
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(WIDTH - 24.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let (icon, color) = toast.icon;
                                icons::show(ui, icon, 16.0, color);
                                if privacy::get(ui.ctx()).on {
                                    // Through the filter (a value is an alias).
                                    let text = match &toast.value {
                                        Some(value) => format!("{} {value}", toast.text),
                                        None => toast.text.clone(),
                                    };
                                    let style = masked::Style {
                                        font: FontId::proportional(13.0),
                                        color: p.text,
                                        on_card: false,
                                    };
                                    masked::text(ui, &text, &style, true);
                                } else {
                                    ui.label(message(&toast.text, toast.value.as_deref()));
                                }
                                if toast.copy
                                    && let Some(value) = &toast.value
                                    && crate::widgets::button::link(ui, "Copy", 13.0).clicked()
                                {
                                    // The real value, also in streamer mode.
                                    ui.ctx().copy_text(value.clone());
                                }
                                // The close button at the right end (a long
                                // message widens the toast instead).
                                let rest = ui.available_width() - CLOSE;
                                if rest > 0.0 {
                                    ui.add_space(rest);
                                }
                                if close_button(ui).clicked() {
                                    closed = Some(toast.id);
                                }
                            });
                        });
                });
            bottom += area.response.rect.height() + GAP;
            // The clock stops while hovered; hovering a fading toast brings
            // it back.
            let step = toast
                .last
                .map_or(0.0, |last| (now - last).clamp(0.0, MAX_STEP));
            toast.last = Some(now);
            if area.response.contains_pointer() {
                toast.age = toast.age.min(hold_end);
            } else {
                toast.age += step;
                next_wake = next_wake.min(if (f64::from(fade)..hold_end).contains(&toast.age) {
                    hold_end - toast.age
                } else {
                    0.0
                });
            }
        }
        self.items
            .retain(|t| look(t.age, fade).is_some() && Some(t.id) != closed);
        if closed.is_some() || !clicks.is_empty() {
            // The others close the gap on the next pass.
            ctx.request_repaint();
        }
        if next_wake.is_finite() {
            ctx.request_repaint_after(Duration::from_secs_f64(next_wake));
        }
        clicks
    }

    #[cfg(test)]
    fn texts(&self) -> Vec<&str> {
        self.items.iter().map(|t| t.text.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_util;

    #[test]
    fn toasts_rise_in_hold_then_fade_out() {
        let fade = timing::TOAST_FADE;
        let start = look(0.0, fade).unwrap();
        assert_eq!(start.opacity, 0.0);
        assert_eq!(start.offset, RISE);
        let rising = look(0.06, fade).unwrap();
        assert!(
            rising.opacity > 0.5 && rising.offset < RISE / 2.0,
            "{rising:?}"
        );
        let held = f64::from(fade) + 1.0;
        assert_eq!(
            look(held, fade),
            Some(Look {
                opacity: 1.0,
                offset: 0.0
            })
        );
        let end = f64::from(fade) * 2.0 + f64::from(timing::TOAST_HOLD);
        let fading = look(end - 0.01, fade).unwrap();
        assert!(fading.opacity < 0.2, "{fading:?}");
        assert_eq!(look(end, fade), None);
        // Reduce motion: no fades, just the hold.
        assert_eq!(look(0.0, 0.0).unwrap().opacity, 1.0);
        assert_eq!(look(f64::from(timing::TOAST_HOLD), 0.0), None);
    }

    #[test]
    fn only_the_newest_few_are_kept() {
        let mut toasts = Toasts::default();
        for i in 0..5 {
            toasts.push(format!("t{i}"));
        }
        assert_eq!(toasts.texts(), ["t2", "t3", "t4"]);
    }

    #[test]
    fn a_value_reads_as_part_of_the_message() {
        let mut toasts = Toasts::default();
        toasts.push_value("Opened a terminal for", "vps-north");
        let mut harness = test_util::harness().build_ui_state(
            |ui, toasts: &mut Toasts| {
                toasts.show(ui.ctx(), true);
            },
            toasts,
        );
        harness.step();
        harness.get_by_label("Opened a terminal for vps-north");
    }

    #[test]
    fn every_toast_closes_at_once_and_the_others_stay() {
        let mut toasts = Toasts::default();
        toasts.push("Settings saved");
        toasts.push_copy("web api is running on", "127.0.0.1:18080");
        toasts.push_value("Opened a terminal for", "vps-north");
        let mut harness = test_util::harness().build_ui_state(
            |ui, toasts: &mut Toasts| {
                toasts.show(ui.ctx(), true);
            },
            toasts,
        );
        harness.step();
        assert_eq!(harness.get_all_by_label(CLOSE_NAME).count(), 3);
        // The newest is drawn first (at the bottom): close the middle one.
        let middle = harness
            .get_by_label("web api is running on 127.0.0.1:18080")
            .rect();
        harness
            .get_all_by_label(CLOSE_NAME)
            .find(|close| {
                close.rect().center().y > middle.top() && close.rect().center().y < middle.bottom()
            })
            .expect("the middle toast's close button")
            .click();
        harness.step();
        assert_eq!(
            harness.state().texts(),
            ["Settings saved", "Opened a terminal for"]
        );
        harness.step();
        assert!(harness.query_by_label_contains("web api").is_none());
        // Still in their hold: closing isn't fading.
        assert!(harness.state().items.iter().all(|t| t.age < 1.0));
    }

    fn update_toast() -> Sticky {
        Sticky {
            kind: StickyKind::Update,
            icon: (icons::CIRCLE_ARROW_UP, Palette::ONE_DARK.blue),
            title: vec![
                ("sshelf ".into(), false),
                ("0.4.0".into(), true),
                (" is available".into(), false),
            ],
            detail: vec![("You have 0.3.2".into(), false)],
            actions: vec![
                StickyAction {
                    label: "What's new",
                    icon: Some(icons::EXTERNAL_LINK),
                    ghost: false,
                },
                StickyAction {
                    label: "Skip this version",
                    icon: None,
                    ghost: true,
                },
            ],
        }
    }

    type State = (Toasts, bool, Vec<StickyClick>);

    fn sticky_harness(toasts: Toasts) -> egui_kittest::Harness<'static, State> {
        test_util::harness().build_ui_state(
            |ui, (toasts, sticky, clicks): &mut State| {
                clicks.extend(toasts.show(ui.ctx(), *sticky));
            },
            (toasts, true, Vec::new()),
        )
    }

    #[test]
    fn a_sticky_toast_stays_below_the_others_until_answered() {
        let mut toasts = Toasts::default();
        toasts.push_sticky(update_toast());
        toasts.push("Settings saved");
        let mut harness = sticky_harness(toasts);
        harness.step();
        let sticky = harness.get_by_label("sshelf 0.4.0 is available").rect();
        let other = harness.get_by_label("Settings saved").rect();
        assert!(other.bottom() < sticky.top(), "{other:?} above {sticky:?}");
        harness.get_by_label("You have 0.3.2");
        // The other one fades; the sticky one doesn't.
        harness.state_mut().0.items[0].age = 100.0;
        harness.state_mut().0.sticky[0].1 = 100.0;
        harness.step();
        assert!(harness.query_by_label("Settings saved").is_none());
        harness.get_by_label("sshelf 0.4.0 is available");
        // Not shown while held back (the tour), still there after.
        harness.state_mut().1 = false;
        harness.step();
        assert!(
            harness
                .query_by_label("sshelf 0.4.0 is available")
                .is_none()
        );
        harness.state_mut().1 = true;
        harness.step();
        harness.get_by_label("Skip this version").click();
        harness.step();
        assert_eq!(
            harness.state().2,
            [StickyClick::Action(StickyKind::Update, 1)]
        );
        harness.step();
        assert!(
            harness
                .query_by_label("sshelf 0.4.0 is available")
                .is_none()
        );
    }

    #[test]
    fn a_sticky_toast_closes_and_says_so() {
        let mut toasts = Toasts::default();
        toasts.push_sticky(update_toast());
        // One of a kind: pushing again replaces it.
        toasts.push_sticky(update_toast());
        assert_eq!(toasts.sticky.len(), 1);
        let mut harness = sticky_harness(toasts);
        harness.step();
        harness.get_by_label(CLOSE_NAME).click();
        harness.step();
        assert_eq!(harness.state().2, [StickyClick::Closed(StickyKind::Update)]);
        assert!(harness.state().0.sticky.is_empty());
    }

    #[test]
    fn a_toast_is_shown_and_then_goes_away() {
        let mut toasts = Toasts::default();
        toasts.push("Settings saved");
        let mut harness = test_util::harness().build_ui_state(
            |ui, toasts: &mut Toasts| {
                toasts.show(ui.ctx(), true);
            },
            toasts,
        );
        harness.step();
        harness.get_by_label("Settings saved");
        harness.state_mut().items[0].age = 100.0;
        harness.step();
        assert!(harness.query_by_label("Settings saved").is_none());
        assert!(harness.state().items.is_empty());
    }
}
