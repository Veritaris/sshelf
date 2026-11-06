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

/// A segmented control (e.g. Form / Raw text): small segments on an inset
/// strip; the selected one's background slides to the next selection.
/// Returns the segment clicked.
pub fn segmented(ui: &mut Ui, id: Id, labels: &[&str], selected: usize) -> Option<usize> {
    segmented_rect(ui, id, labels, selected).0
}

/// [`segmented`], and where it is.
pub fn segmented_rect(
    ui: &mut Ui,
    id: Id,
    labels: &[&str],
    selected: usize,
) -> (Option<usize>, Rect) {
    let segments: Vec<Segment> = labels.iter().map(|&label| Segment::new(label)).collect();
    segmented_with(ui, id, &segments, selected)
}

/// One segment of [`segmented_with`]: its word, an optional Lucide icon
/// before it, and why it can't be picked (shown as a tooltip).
pub struct Segment<'a> {
    pub label: &'a str,
    pub icon: Option<(char, egui::Color32)>,
    pub disabled: Option<&'a str>,
}

impl<'a> Segment<'a> {
    pub fn new(label: &'a str) -> Self {
        Segment {
            label,
            icon: None,
            disabled: None,
        }
    }
}

/// [`segmented`] with icons and disabled segments (the tunnel editor's
/// *ssh | kubectl* switch).
pub fn segmented_with(
    ui: &mut Ui,
    id: Id,
    segments: &[Segment],
    selected: usize,
) -> (Option<usize>, Rect) {
    let p = &Palette::ONE_DARK;
    const HEIGHT: f32 = 28.0;
    const PADDING: f32 = 10.0;
    const ICON: f32 = 14.0 + 6.0;
    let font = FontId::proportional(12.0);
    let labels: Vec<&str> = segments.iter().map(|s| s.label).collect();
    let galleys: Vec<_> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l.to_string(), font.clone(), p.text)
        })
        .collect();
    let widths: Vec<f32> = galleys
        .iter()
        .zip(segments)
        .map(|(g, s)| g.size().x + PADDING * 2.0 + if s.icon.is_some() { ICON } else { 0.0 })
        .collect();
    let total = widths.iter().sum::<f32>() + 2.0 * (labels.len() as f32 - 1.0) + 4.0;
    let (outer, _) = ui.allocate_exact_size(vec2(total, HEIGHT + 4.0), Sense::hover());
    ui.painter().rect_filled(outer, radius::CONTROL, p.bg_dark);
    let slot = ui.painter().add(Shape::Noop);
    let mut clicked = None;
    let mut x = outer.left() + 2.0;
    for (index, (label, galley)) in labels.iter().zip(galleys).enumerate() {
        let rect = Rect::from_min_size(pos2(x, outer.top() + 2.0), vec2(widths[index], HEIGHT));
        x += widths[index] + 2.0;
        let segment = &segments[index];
        let enabled = segment.disabled.is_none();
        let sense = if enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = ui.interact(rect, id.with(index), sense);
        let is_selected = index == selected;
        response.widget_info(|| WidgetInfo::selected(Role::Tab, enabled, is_selected, *label));
        let response = match segment.disabled {
            Some(why) => {
                let why = why.to_string();
                crate::widgets::popup::tip(response, move |ui| {
                    ui.label(why);
                })
            }
            None => response,
        };
        if is_selected {
            let target =
                motion::tween_rect(ui.ctx(), id.with("highlight"), index, rect, timing::SLIDE);
            ui.painter().set(
                slot,
                Shape::rect_filled(target, radius::CONTROL, p.selection),
            );
        }
        let active = motion::fade(
            ui.ctx(),
            id.with(("active", index)),
            is_selected || response.hovered(),
            timing::SLIDE,
            motion::ease_in_out,
        );
        let text_color = if enabled {
            p.text.lerp_to_gamma(p.text_strong, active)
        } else {
            p.disabled_text
        };
        let mut left = rect.left() + PADDING;
        if let Some((icon, color)) = segment.icon {
            let color = if enabled { color } else { p.disabled_text };
            ui.painter().text(
                pos2(left + 7.0, rect.center().y),
                egui::Align2::CENTER_CENTER,
                icon,
                crate::icons::font(14.0),
                color,
            );
            left += ICON;
        }
        ui.painter().galley_with_override_text_color(
            pos2(left, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
        if response.clicked() && !is_selected && enabled {
            clicked = Some(index);
        }
        if enabled {
            response.on_hover_cursor(CursorIcon::PointingHand);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
        }
    }
    (clicked, outer)
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
