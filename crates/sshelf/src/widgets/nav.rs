//! Highlights that move (SPEC.md §10 *Interaction and motion*): the
//! selection background slides and resizes from the old item to the new one;
//! the hover background slides between rows and fades in place when the
//! pointer leaves.

use egui::layers::ShapeIdx;
use egui::{Color32, Id, Rect, Shape, Ui};

use crate::motion;
use crate::theme::{radius, timing};

/// Paints the selection background of item `selected` (at `target`). When
/// `selected` changes it slides there from the previous item.
pub fn highlight(ui: &Ui, id: Id, selected: usize, target: Rect, fill: Color32, radius: u8) {
    let rect = motion::tween_rect(ui.ctx(), id, selected, target, timing::SLIDE);
    ui.painter().rect_filled(rect, radius, fill);
}

/// The hover background of a list or menu, painted into `slot` (reserved
/// before the rows, so it sits under them). `hovered` is the row under the
/// pointer and its rect. It slides between rows (50 ms), fades out in place
/// when the pointer leaves (120 ms) and, coming back, appears on the hovered
/// row without sliding.
pub fn hover_highlight(
    ui: &Ui,
    id: Id,
    slot: ShapeIdx,
    hovered: Option<(usize, Rect)>,
    color: Color32,
) {
    let ctx = ui.ctx();
    let visible = motion::fade(
        ctx,
        id.with("visible"),
        hovered.is_some(),
        timing::MENU,
        motion::ease_out,
    );
    let last_id = id.with("last");
    let rect = match hovered {
        Some((index, target)) => {
            if visible < 0.01 {
                motion::forget_tween(ctx, id);
            }
            let rect = motion::tween_rect(ctx, id, index, target, timing::MENU_SLIDE);
            ctx.data_mut(|d| d.insert_temp(last_id, rect));
            Some(rect)
        }
        None if visible > 0.0 => ctx.data(|d| d.get_temp::<Rect>(last_id)),
        None => None,
    };
    if let Some(rect) = rect {
        ui.painter().set(
            slot,
            Shape::rect_filled(
                rect,
                radius::CONTROL,
                color.gamma_multiply(visible.max(0.01)),
            ),
        );
    }
}
