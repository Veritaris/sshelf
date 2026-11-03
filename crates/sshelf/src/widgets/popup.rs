//! Tooltips with the timing of SPEC.md §10 *Interaction and motion*: after
//! 400 ms of hover they fade in (120 ms) with a 3 px slide and fade out the
//! same way; the next tooltip within 300 ms of one closing shows at once.

use egui::{Align2, Area, FontId, Frame, Id, Margin, Order, Response, Stroke, Ui, vec2};

use crate::motion;
use crate::theme::{Palette, radius, timing};

/// Distance between the widget and its tooltip.
const GAP: f32 = 8.0;
/// How far the tooltip travels while it fades.
const SLIDE: f32 = 3.0;

/// When the last tooltip was visible (for the "next one shows at once" rule).
fn last_visible_id() -> Id {
    Id::unique("sshelf_tooltip_last_visible")
}

/// A tooltip to the right of `response`'s widget (the rail's), built by
/// `add_contents` while it shows.
pub fn tip(response: Response, add_contents: impl FnOnce(&mut Ui)) -> Response {
    let ctx = response.ctx.clone();
    let id = response.id.with("tooltip");
    let now = ctx.input(|i| i.time);
    // A press hides the tooltip until the pointer leaves the widget (the
    // "next tooltip shows at once" rule is for other widgets).
    let suppressed_id = id.with("suppressed");
    let pressed = ctx.input(|i| i.pointer.any_down());
    if response.hovered() && pressed {
        ctx.data_mut(|d| d.insert_temp(suppressed_id, true));
    } else if !response.hovered() {
        ctx.data_mut(|d| d.remove::<bool>(suppressed_id));
    }
    let suppressed = ctx.data(|d| d.get_temp::<bool>(suppressed_id).unwrap_or(false));
    let hovered = response.hovered() && !pressed && !suppressed;
    let since_id = id.with("hover_since");
    let since = if hovered {
        Some(ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(since_id, || now)))
    } else {
        ctx.data_mut(|d| d.remove::<f64>(since_id));
        None
    };
    let last_visible = ctx
        .data(|d| d.get_temp::<f64>(last_visible_id()))
        .unwrap_or(f64::NEG_INFINITY);
    let ready = since.is_some_and(|since| {
        now - since >= f64::from(timing::TOOLTIP_DELAY)
            || now - last_visible <= f64::from(timing::TOOLTIP_GRACE)
    });
    if let Some(since) = since
        && !ready
    {
        let wait = f64::from(timing::TOOLTIP_DELAY) - (now - since);
        ctx.request_repaint_after_secs(wait as f32);
    }
    // "out" while appearing, "in" while disappearing.
    let easing = if ready {
        motion::ease_out
    } else {
        motion::ease_in
    };
    let shown = motion::fade(&ctx, id, ready, timing::TOOLTIP_FADE, easing);
    if shown > 0.0 {
        ctx.data_mut(|d| d.insert_temp(last_visible_id(), now));
        let p = &Palette::ONE_DARK;
        let pos = response.rect.right_center() + vec2(GAP - SLIDE * (1.0 - shown), 0.0);
        Area::new(id)
            .order(Order::Tooltip)
            .fixed_pos(pos)
            .pivot(Align2::LEFT_CENTER)
            .interactable(false)
            // Our fade alone (egui's own would ignore Reduce motion).
            .fade_in(false)
            .show(&ctx, |ui| {
                ui.multiply_opacity(shown);
                Frame::new()
                    .fill(p.tooltip)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::CONTROL)
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.style_mut().override_font_id = Some(FontId::proportional(12.0));
                        ui.visuals_mut().override_text_color = Some(p.text_strong);
                        add_contents(ui);
                    });
            });
    }
    response
}
