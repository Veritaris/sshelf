//! Easing curves and animation helpers (SPEC.md §10 *Interaction and
//! motion*). Durations live in [`crate::theme::timing`]; *Reduce motion*
//! turns every one of them into 0.

use egui::{Context, Id};

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

fn ease_out(t: f32) -> f32 {
    OUT.eval(t)
}

/// Turns all animation off (or on again); the app sets it from the settings
/// every frame.
pub fn set_reduce_motion(ctx: &Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(reduce_motion_id(), on));
}

fn reduce_motion(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(reduce_motion_id()).unwrap_or(false))
}

fn reduce_motion_id() -> Id {
    Id::unique("sshelf_reduce_motion")
}

/// 0 → 1 while `on`, back to 0 after, over `seconds` with the "out" curve.
pub fn fade(ctx: &Context, id: Id, on: bool, seconds: f32) -> f32 {
    let seconds = if reduce_motion(ctx) { 0.0 } else { seconds };
    ctx.animate_bool_with_time_and_easing(id, on, seconds, ease_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_run_from_zero_to_one() {
        for curve in [OUT, CubicBezier::new(0.65, 0.0, 0.35, 1.0)] {
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
        // A symmetric curve passes through the middle.
        let in_out = CubicBezier::new(0.65, 0.0, 0.35, 1.0);
        assert!((in_out.eval(0.5) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn reduce_motion_jumps_to_the_target() {
        let ctx = Context::default();
        let id = Id::unique("fade");
        set_reduce_motion(&ctx, true);
        for (on, expected) in [(false, 0.0), (true, 1.0)] {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                assert_eq!(fade(ui.ctx(), id, on, 0.08), expected);
            });
            // Nothing renders these textures here.
            output.textures_delta.clear();
        }
    }
}
