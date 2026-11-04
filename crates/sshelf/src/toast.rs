//! Toasts: short success messages ("Settings saved") in the bottom-right
//! corner, above the status bar (SPEC.md §10 *Interaction and motion*). Each
//! fades in with a 6 px rise, stays 3 s, then fades out; hovering one pauses
//! it. Errors stay in the status bar until dismissed. A toast's clock only
//! runs while it is drawn, so one raised while the window is hidden is still
//! seen. Waiting is one scheduled repaint, not a polling timer.

use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Area, Color32, FontId, Frame, Id, Margin, Order, Stroke, vec2};

use crate::theme::{Palette, radius, timing};
use crate::{icons, motion, status_bar};

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

struct Toast {
    id: u64,
    text: String,
    /// A value after the text, in mono (an alias, a path).
    value: Option<String>,
    /// The icon before it and its colour (a green check by default).
    icon: (char, Color32),
    /// Seconds it has been shown (not counting hovered time).
    age: f64,
    /// `egui` time of the last frame that drew it.
    last: Option<f64>,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
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

    fn add(&mut self, text: String, value: Option<String>, icon: (char, Color32)) {
        self.items.push(Toast {
            id: self.next_id,
            text,
            value,
            icon,
            age: 0.0,
            last: None,
        });
        self.next_id += 1;
        if self.items.len() > MAX {
            self.items.remove(0);
        }
    }

    /// Call once per frame from `App::ui`.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let fade = motion::duration(ctx, timing::TOAST_FADE);
        let hold_end = f64::from(fade) + f64::from(timing::TOAST_HOLD);
        let p = &Palette::ONE_DARK;
        // Newest at the bottom, stacked upwards.
        let mut bottom = status_bar::HEIGHT + MARGIN;
        let mut next_wake = f64::INFINITY;
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
                                ui.label(message(&toast.text, toast.value.as_deref()));
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
        self.items.retain(|t| look(t.age, fade).is_some());
        if next_wake.is_finite() {
            ctx.request_repaint_after(Duration::from_secs_f64(next_wake));
        }
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
        let mut harness = test_util::harness()
            .build_ui_state(|ui, toasts: &mut Toasts| toasts.show(ui.ctx()), toasts);
        harness.step();
        harness.get_by_label("Opened a terminal for vps-north");
    }

    #[test]
    fn a_toast_is_shown_and_then_goes_away() {
        let mut toasts = Toasts::default();
        toasts.push("Settings saved");
        let mut harness = test_util::harness()
            .build_ui_state(|ui, toasts: &mut Toasts| toasts.show(ui.ctx()), toasts);
        harness.step();
        harness.get_by_label("Settings saved");
        harness.state_mut().items[0].age = 100.0;
        harness.step();
        assert!(harness.query_by_label("Settings saved").is_none());
        assert!(harness.state().items.is_empty());
    }
}
