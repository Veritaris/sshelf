//! Text that may not fit (SPEC.md §10 *Truncated text*): one line, cut with
//! an ellipsis, with the full text in a tooltip only when it was cut.

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, Response, Sense, Ui};

use crate::widgets::popup;

/// `job` on one line in the width left in `ui`; `full` is the tooltip text
/// when it doesn't fit.
pub fn truncated(ui: &mut Ui, mut job: LayoutJob, full: &str) -> Response {
    job.wrap = TextWrapping::truncate_at_width(ui.available_width().max(0.0));
    let galley = ui.painter().layout_job(job);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let elided = galley.elided;
    ui.painter().galley(rect.min, galley, Color32::PLACEHOLDER);
    if elided {
        popup::tip_below(response, |ui| {
            ui.label(full);
        })
    } else {
        response
    }
}
