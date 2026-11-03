//! Toasts: short success messages ("Settings saved") in the bottom-right
//! corner that fade out on their own. Errors stay in the status bar until
//! dismissed. A toast's time starts when it is first drawn, so one raised
//! while the window is hidden is still seen. Expiry is one scheduled repaint,
//! not a polling timer.

use std::time::Duration;

use egui::RichText;

use crate::icons;
use crate::theme::Palette;

/// How long a toast stays fully visible, then how long it fades.
const SHOW_SECS: f64 = 3.0;
const FADE_SECS: f64 = 0.5;
/// Older toasts give way beyond this many.
const MAX: usize = 3;

struct Toast {
    text: String,
    /// `egui` time when first drawn.
    since: Option<f64>,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, text: impl Into<String>) {
        self.items.push(Toast {
            text: text.into(),
            since: None,
        });
        if self.items.len() > MAX {
            self.items.remove(0);
        }
    }

    /// Opacity of a toast shown for `age` seconds; `None` once it is gone.
    fn opacity(age: f64) -> Option<f32> {
        if age < SHOW_SECS {
            Some(1.0)
        } else if age < SHOW_SECS + FADE_SECS {
            Some((1.0 - (age - SHOW_SECS) / FADE_SECS) as f32)
        } else {
            None
        }
    }

    /// Call once per frame from `App::ui`.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for toast in &mut self.items {
            toast.since.get_or_insert(now);
        }
        self.items
            .retain(|t| Self::opacity(now - t.since.unwrap_or(now)).is_some());
        if self.items.is_empty() {
            return;
        }
        let p = &Palette::ONE_DARK;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -40.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for toast in &self.items {
                    let age = now - toast.since.unwrap_or(now);
                    ui.scope(|ui| {
                        ui.set_opacity(Self::opacity(age).unwrap_or(0.0));
                        egui::Frame::new()
                            .fill(p.bg_dark)
                            .stroke(egui::Stroke::new(1.0, p.green))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    icons::show(ui, icons::CIRCLE_CHECK, icons::INLINE, p.green);
                                    ui.label(RichText::new(&toast.text).color(p.text));
                                });
                            });
                    });
                    ui.add_space(6.0);
                }
            });
        // Wake up for the next fade step or expiry.
        let next = self
            .items
            .iter()
            .map(|t| {
                let age = now - t.since.unwrap_or(now);
                if age < SHOW_SECS {
                    SHOW_SECS - age
                } else {
                    1.0 / 30.0
                }
            })
            .fold(f64::INFINITY, f64::min);
        ctx.request_repaint_after(Duration::from_secs_f64(next.max(0.0)));
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
    fn opacity_holds_then_fades_then_ends() {
        assert_eq!(Toasts::opacity(0.0), Some(1.0));
        assert_eq!(Toasts::opacity(SHOW_SECS - 0.01), Some(1.0));
        let half = Toasts::opacity(SHOW_SECS + FADE_SECS / 2.0).unwrap();
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert_eq!(Toasts::opacity(SHOW_SECS + FADE_SECS), None);
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
    fn a_toast_is_shown_and_then_goes_away() {
        let mut toasts = Toasts::default();
        toasts.push("Settings saved");
        let mut harness = test_util::harness()
            .build_ui_state(|ui, toasts: &mut Toasts| toasts.show(ui.ctx()), toasts);
        harness.step();
        harness.get_by_label("Settings saved");
        harness.state_mut().items[0].since = Some(-100.0);
        harness.step();
        assert!(harness.query_by_label("Settings saved").is_none());
        assert!(harness.state().items.is_empty());
    }
}
