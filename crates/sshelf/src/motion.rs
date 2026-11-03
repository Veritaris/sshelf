//! Easing curves and animation helpers (SPEC.md §10 *Interaction and
//! motion*). Durations live in [`crate::theme::timing`]; *Reduce motion*
//! turns every one of them into 0.

use egui::{Context, Id, Rect};

/// A CSS-style `cubic-bezier(x1, y1, x2, y2)` easing curve.
#[derive(Debug, Clone, Copy)]
pub struct CubicBezier {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    /// Progress at time `t` (both 0..=1).
    pub fn eval(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let s = self.solve_x(t);
        bezier(self.y1, self.y2, s)
    }

    /// The curve parameter whose x is `x`: Newton steps, then bisection if
    /// they don't converge (flat spots in the curve).
    fn solve_x(&self, x: f32) -> f32 {
        let mut s = x;
        for _ in 0..8 {
            let error = bezier(self.x1, self.x2, s) - x;
            if error.abs() < 1e-5 {
                return s;
            }
            let slope = bezier_slope(self.x1, self.x2, s);
            if slope.abs() < 1e-6 {
                break;
            }
            s = (s - error / slope).clamp(0.0, 1.0);
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        s = x;
        for _ in 0..30 {
            let value = bezier(self.x1, self.x2, s);
            if (value - x).abs() < 1e-5 {
                break;
            }
            if value < x {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.0;
        }
        s
    }
}

/// One coordinate of a bezier from (0,0) to (1,1) with control points `a`, `b`.
fn bezier(a: f32, b: f32, s: f32) -> f32 {
    let u = 1.0 - s;
    3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
}

fn bezier_slope(a: f32, b: f32, s: f32) -> f32 {
    let u = 1.0 - s;
    3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
}

/// "out": `cubic-bezier(.2, .8, .2, 1)`.
pub const OUT: CubicBezier = CubicBezier::new(0.2, 0.8, 0.2, 1.0);
/// "in-out": `cubic-bezier(.65, 0, .35, 1)`.
pub const IN_OUT: CubicBezier = CubicBezier::new(0.65, 0.0, 0.35, 1.0);

/// "in": `cubic-bezier(.4, 0, 1, 1)`.
pub const IN: CubicBezier = CubicBezier::new(0.4, 0.0, 1.0, 1.0);

/// [`IN`] as the plain function egui's animations take.
pub fn ease_in(t: f32) -> f32 {
    IN.eval(t)
}

/// [`OUT`] as the plain function egui's animations take.
pub fn ease_out(t: f32) -> f32 {
    OUT.eval(t)
}

/// [`IN_OUT`] as the plain function egui's animations take.
pub fn ease_in_out(t: f32) -> f32 {
    IN_OUT.eval(t)
}

/// Turns all animation off (or on again); the app sets it from the settings
/// every frame.
pub fn set_reduce_motion(ctx: &Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(reduce_motion_id(), on));
}

/// `seconds`, or 0 with *Reduce motion*: for animations timed by hand.
pub fn duration(ctx: &Context, seconds: f32) -> f32 {
    if reduce_motion(ctx) { 0.0 } else { seconds }
}

fn reduce_motion(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(reduce_motion_id()).unwrap_or(false))
}

fn reduce_motion_id() -> Id {
    Id::unique("sshelf_reduce_motion")
}

/// 0 → 1 while `on`, back to 0 after, over `seconds` along `easing`. On the
/// way back egui returns `1 - easing(progress)`, so pass the curve meant for
/// the current direction.
pub fn fade(ctx: &Context, id: Id, on: bool, seconds: f32, easing: fn(f32) -> f32) -> f32 {
    let seconds = if reduce_motion(ctx) { 0.0 } else { seconds };
    ctx.animate_bool_with_time_and_easing(id, on, seconds, easing)
}

#[derive(Debug, Clone, Copy)]
struct RectTween {
    key: usize,
    from: Rect,
    to: Rect,
    start: f64,
}

/// A rectangle moving to `target` over `seconds` ("in-out") whenever `key`
/// changes, e.g. the sliding highlight following the selected item. While
/// `key` stays the same it follows `target` at once (window resizes don't
/// animate).
pub fn tween_rect(ctx: &Context, id: Id, key: usize, target: Rect, seconds: f32) -> Rect {
    let now = ctx.input(|i| i.time);
    let seconds = if reduce_motion(ctx) { 0.0 } else { seconds };
    let running = |tween: &RectTween| now - tween.start < f64::from(seconds);
    let previous = ctx.data(|d| d.get_temp::<RectTween>(id));
    let tween = match previous {
        Some(old) if old.key != key => RectTween {
            key,
            from: at(&old, now, seconds),
            to: target,
            start: now,
        },
        Some(old) if running(&old) => RectTween { to: target, ..old },
        _ => RectTween {
            key,
            from: target,
            to: target,
            start: f64::NEG_INFINITY,
        },
    };
    ctx.data_mut(|d| d.insert_temp(id, tween));
    if running(&tween) {
        ctx.request_repaint();
        at(&tween, now, seconds)
    } else {
        target
    }
}

fn at(tween: &RectTween, now: f64, seconds: f32) -> Rect {
    let t = if seconds <= 0.0 {
        1.0
    } else {
        ((now - tween.start) as f32 / seconds).clamp(0.0, 1.0)
    };
    let e = IN_OUT.eval(t);
    Rect::from_min_max(
        tween.from.min.lerp(tween.to.min, e),
        tween.from.max.lerp(tween.to.max, e),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_run_from_zero_to_one() {
        for curve in [OUT, IN, IN_OUT] {
            assert_eq!(curve.eval(0.0), 0.0);
            assert!((curve.eval(1.0) - 1.0).abs() < 1e-4);
            let mut last = 0.0;
            for i in 1..=100 {
                let value = curve.eval(i as f32 / 100.0);
                assert!(value >= last - 1e-4, "{curve:?} goes back at {i}");
                last = value;
            }
        }
    }

    #[test]
    fn out_starts_fast_and_linear_stays_linear() {
        // "out" is past the middle well before half the time.
        assert!(OUT.eval(0.25) > 0.6, "{}", OUT.eval(0.25));
        let linear = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
        for t in [0.1, 0.33, 0.5, 0.9] {
            assert!((linear.eval(t) - t).abs() < 1e-3);
        }
        // A symmetric curve passes through the middle; "in" starts slowly.
        assert!((IN_OUT.eval(0.5) - 0.5).abs() < 1e-3);
        assert!(IN.eval(0.25) < 0.15, "{}", IN.eval(0.25));
    }

    #[test]
    fn reduce_motion_jumps_to_the_target() {
        let ctx = Context::default();
        let id = Id::unique("fade");
        set_reduce_motion(&ctx, true);
        for (on, expected) in [(false, 0.0), (true, 1.0)] {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                assert_eq!(fade(ui.ctx(), id, on, 0.08, ease_out), expected);
            });
            // Nothing renders these textures here.
            output.textures_delta.clear();
        }
    }

    /// Runs one frame at `time` and returns `tween_rect`'s answer.
    fn tween_at(ctx: &Context, time: f64, key: usize, target: Rect) -> Rect {
        let mut rect = Rect::NOTHING;
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            rect = tween_rect(ui.ctx(), Id::unique("tween"), key, target, 0.18);
        });
        output.textures_delta.clear();
        rect
    }

    #[test]
    fn tweens_slide_when_the_key_changes_and_follow_otherwise() {
        let ctx = Context::default();
        let a = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(40.0, 40.0));
        let b = Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(40.0, 40.0));
        assert_eq!(tween_at(&ctx, 0.0, 0, a), a, "starts where it is");
        // A new key slides from a to b.
        assert_eq!(tween_at(&ctx, 1.0, 1, b), a);
        let mid = tween_at(&ctx, 1.09, 1, b);
        assert!(mid.min.y > 10.0 && mid.min.y < 90.0, "{mid:?}");
        assert_eq!(tween_at(&ctx, 1.2, 1, b), b);
        // Same key, moved target (a window resize): no animation.
        let c = b.translate(egui::vec2(0.0, 50.0));
        assert_eq!(tween_at(&ctx, 2.0, 1, c), c);
    }
}
