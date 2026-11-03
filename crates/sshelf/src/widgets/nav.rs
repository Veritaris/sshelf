//! Selection that slides (SPEC.md §10 *Interaction and motion*): the active
//! background moves and resizes from the old item to the new one.

use egui::{Color32, Id, Rect, Ui};

use crate::motion;
use crate::theme::timing;

/// Paints the selection background of item `selected` (at `target`). When
/// `selected` changes it slides there from the previous item.
pub fn highlight(ui: &Ui, id: Id, selected: usize, target: Rect, fill: Color32, radius: u8) {
    let rect = motion::tween_rect(ui.ctx(), id, selected, target, timing::SLIDE);
    ui.painter().rect_filled(rect, radius, fill);
}
