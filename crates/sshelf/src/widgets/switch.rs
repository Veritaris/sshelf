//! The on / off switch (SPEC.md §10: everywhere outside multi-select
//! dropdowns on/off is a switch): 36 × 20, the knob slides and the track
//! fades to Magenta in 200 ms.

use egui::{CursorIcon, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};

use crate::motion;
use crate::theme::Palette;

const WIDTH: f32 = 36.0;
const HEIGHT: f32 = 20.0;
const KNOB: f32 = 16.0;
/// Knob slide and track fade.
const TIME: f32 = 0.200;

/// A switch for `on`, named `name` (its accessible name); flips on click.
pub fn switch(ui: &mut Ui, on: &mut bool, name: &str) -> Response {
    track(ui, on, name, Palette::ONE_DARK.selection)
}

/// A switch on a selected (`#3E4451`) row: the off track is the outline
/// colour, so it stays visible.
pub fn switch_raised(ui: &mut Ui, on: &mut bool, name: &str) -> Response {
    track(ui, on, name, Palette::ONE_DARK.border)
}

fn track(ui: &mut Ui, on: &mut bool, name: &str, off: egui::Color32) -> Response {
    let p = &Palette::ONE_DARK;
    let (rect, mut response) = ui.allocate_exact_size(vec2(WIDTH, HEIGHT), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let value = *on;
    response.widget_info(|| WidgetInfo::selected(Role::Switch, response.enabled(), value, name));
    let t = motion::fade(
        ui.ctx(),
        response.id.with("switch"),
        value,
        TIME,
        motion::ease_in_out,
    );
    let enabled = ui.is_enabled();
    let alpha = if enabled { 1.0 } else { 0.45 };
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        HEIGHT / 2.0,
        off.lerp_to_gamma(p.accent, t).gamma_multiply(alpha),
    );
    let x = rect.left() + 2.0 + (WIDTH - KNOB - 4.0) * t;
    let knob = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(KNOB, KNOB));
    painter.circle_filled(
        knob.center(),
        KNOB / 2.0,
        p.text_strong
            .lerp_to_gamma(p.bg_dark, t)
            .gamma_multiply(alpha),
    );
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}
