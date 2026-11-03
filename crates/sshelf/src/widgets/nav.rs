//! Highlights that move (SPEC.md §10 *Interaction and motion*): the
//! selection background slides and resizes from the old item to the new one;
//! the hover background slides between rows and fades in place when the
//! pointer leaves.

use egui::layers::ShapeIdx;
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Id, Rect, Role, Sense, Shape, Ui, WidgetInfo, pos2,
    vec2,
};

use crate::motion;
use crate::theme::{Palette, radius, timing};

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

const TAB_HEIGHT: f32 = 32.0;
const TAB_PADDING: f32 = 12.0;

/// A row of tabs over a 1 px line; the selected one is Magenta with a 2 px
/// underline that slides to the next selection (SPEC.md §10). Returns the
/// tab clicked.
pub fn tabs(ui: &mut Ui, id: Id, labels: &[&str], selected: usize) -> Option<usize> {
    let p = &Palette::ONE_DARK;
    let mut clicked = None;
    let mut underline = None;
    let top = ui.cursor().top();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (index, label) in labels.iter().enumerate() {
            let galley =
                ui.painter()
                    .layout_no_wrap(label.to_string(), FontId::proportional(13.0), p.text);
            let size = vec2(galley.size().x + TAB_PADDING * 2.0, TAB_HEIGHT);
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            let is_selected = index == selected;
            response.widget_info(|| {
                WidgetInfo::selected(Role::Tab, response.enabled(), is_selected, *label)
            });
            let hover = motion::fade(
                ui.ctx(),
                id.with(("hover", index)),
                response.hovered() && !is_selected,
                timing::HOVER,
                motion::ease_out,
            );
            if hover > 0.0 {
                let corners = CornerRadius {
                    nw: radius::CONTROL,
                    ne: radius::CONTROL,
                    sw: radius::ROW,
                    se: radius::ROW,
                };
                ui.painter()
                    .rect_filled(rect, corners, p.surface.gamma_multiply(hover));
            }
            let active = motion::fade(
                ui.ctx(),
                id.with(("active", index)),
                is_selected,
                timing::SLIDE,
                motion::ease_in_out,
            );
            ui.painter().galley_with_override_text_color(
                pos2(
                    rect.left() + TAB_PADDING,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                p.text.lerp_to_gamma(p.accent, active),
            );
            if is_selected {
                underline = Some(Rect::from_min_max(
                    pos2(rect.left(), rect.bottom() - 2.0),
                    rect.max,
                ));
            }
            if response.clicked() && !is_selected {
                clicked = Some(index);
            }
            response.on_hover_cursor(CursorIcon::PointingHand);
        }
    });
    let line = Rect::from_min_max(
        pos2(ui.min_rect().left(), top + TAB_HEIGHT - 1.0),
        pos2(ui.max_rect().right(), top + TAB_HEIGHT),
    );
    ui.painter().rect_filled(line, 0, p.selection);
    if let Some(target) = underline {
        let rect = motion::tween_rect(
            ui.ctx(),
            id.with("underline"),
            selected,
            target,
            timing::SLIDE,
        );
        ui.painter().rect_filled(rect, 0, p.accent);
    }
    clicked
}
