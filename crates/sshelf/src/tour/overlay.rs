//! The tour's overlay (SPEC.md §10 *Onboarding tour*, board `Tour-Step`):
//! the window dimmed except holes around the targets, a 2 px Magenta ring,
//! the popover beside the spotlight, the glint on the element to click, and
//! glossary tooltips. Spotlight and popover glide between steps (240 ms,
//! in-out); *Reduce motion* makes them jump and the glint static.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{
    Align, Area, Color32, Context, CornerRadius, FontId, Frame, Id, LayerId, Layout, Margin, Order,
    Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::theme::{Palette, radius};
use crate::tour::steps::{self, Step};
use crate::tray::TrayModel;
use crate::widgets::button::Button;
use crate::{fonts, icons, motion, tray_menu};

/// Hole padding around the targets.
const PAD: f32 = 6.0;
/// Hole corners.
const HOLE_RADIUS: f32 = 8.0;
const GLIDE: f32 = 0.240;
const POPOVER: f32 = 340.0;
/// Between the spotlight and the popover.
const GAP: f32 = 16.0;
/// Between the popover and the window edge.
const MARGIN: f32 = 12.0;
/// The glint's cycle and its crossing.
const GLINT_PERIOD: f64 = 2.4;
const GLINT_CROSS: f64 = 1.0;

/// What the overlay shows for the current step.
pub struct View<'a> {
    pub step: &'a Step,
    /// 0-based.
    pub index: usize,
    pub total: usize,
    /// The spotlight (targets' union) and extra holes; `None`: no spotlight.
    pub spot: Option<Rect>,
    pub extra: Vec<Rect>,
    /// The element to click on a pending *do it* step.
    pub glint: Option<Rect>,
    /// The action is pending (shows the hint, *Show me*).
    pub pending: bool,
    /// The action runs: the primary button is disabled.
    pub busy: bool,
    /// The tray menu for the picture step.
    pub tray: &'a TrayModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clicked {
    Back,
    /// *Show me*, *Next* or *Open my hosts*.
    Primary,
    Skip,
}

/// Draws the overlay; returns what was clicked in the popover and where
/// input may reach the UI (holes and the popover).
pub fn show(ctx: &Context, view: &View) -> (Option<Clicked>, Vec<Rect>) {
    let screen = ctx.content_rect();
    let key = view.index;
    // No target: the hole shrinks to nothing at the window's centre, so the
    // shape never changes kind mid-glide.
    let target = view
        .spot
        .map(|r| r.expand(PAD))
        .unwrap_or_else(|| Rect::from_center_size(screen.center(), vec2(0.0, 0.0)));
    let hole = motion::tween_rect(ctx, Id::unique("tour_hole"), key, target, GLIDE);
    let ring = motion::fade(
        ctx,
        Id::unique("tour_ring"),
        view.spot.is_some(),
        GLIDE,
        motion::ease_in_out,
    );
    let extra: Vec<Rect> = view.extra.iter().map(|r| r.expand(PAD)).collect();
    let mut holes = vec![hole];
    holes.extend(&extra);

    let layer = LayerId::new(Order::Foreground, Id::unique("tour_dim"));
    let painter = ctx.layer_painter(layer);
    let p = &Palette::ONE_DARK;
    painter.add(dim(screen, &holes, p.overlay));
    for (i, rect) in holes.iter().enumerate() {
        let alpha = if i == 0 { ring } else { 1.0 };
        if rect.width() > 1.0 && alpha > 0.0 {
            painter.rect_stroke(
                *rect,
                HOLE_RADIUS as u8,
                Stroke::new(2.0, p.accent.gamma_multiply(alpha)),
                StrokeKind::Outside,
            );
        }
    }
    ctx.move_to_top(layer);

    if let Some(glint_rect) = view.glint {
        let glint = LayerId::new(Order::Foreground, Id::unique("tour_glint"));
        ctx.layer_painter(glint).add(glint_shape(ctx, glint_rect));
        ctx.move_to_top(glint);
    }

    let (clicked, popover) = popover(ctx, view, hole, screen);
    let mut allowed: Vec<Rect> = holes.into_iter().filter(|r| r.width() > 1.0).collect();
    allowed.push(popover);
    (clicked, allowed)
}

/// The dimming: `screen` less rounded `holes`, as one mesh of strips plus
/// the corner pieces outside each hole's rounded corners.
fn dim(screen: Rect, holes: &[Rect], color: Color32) -> Shape {
    let holes: Vec<Rect> = holes
        .iter()
        .map(|h| h.intersect(screen))
        .filter(|h| h.width() > 1.0 && h.height() > 1.0)
        .collect();
    let mut mesh = Mesh::default();
    // Horizontal bands between every hole edge; in each band, the parts
    // not covered by a hole.
    let mut ys = vec![screen.top(), screen.bottom()];
    for h in &holes {
        ys.extend([h.top(), h.bottom()]);
    }
    ys.sort_by(f32::total_cmp);
    ys.dedup();
    for band in ys.windows(2) {
        let (top, bottom) = (band[0], band[1]);
        if bottom <= top {
            continue;
        }
        let mid = (top + bottom) / 2.0;
        let mut covered: Vec<(f32, f32)> = holes
            .iter()
            .filter(|h| h.top() <= mid && mid <= h.bottom())
            .map(|h| (h.left(), h.right()))
            .collect();
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut x = screen.left();
        for (left, right) in covered {
            if left > x {
                mesh.add_colored_rect(Rect::from_min_max(pos2(x, top), pos2(left, bottom)), color);
            }
            x = x.max(right);
        }
        if x < screen.right() {
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, top), pos2(screen.right(), bottom)),
                color,
            );
        }
    }
    let r = HOLE_RADIUS;
    for h in &holes {
        let r = r.min(h.width() / 2.0).min(h.height() / 2.0);
        // Corner point, the arc's centre, and the arc's start angle.
        let corners = [
            (h.left_top(), h.left_top() + vec2(r, r), 180.0_f32),
            (h.right_top(), h.right_top() + vec2(-r, r), 270.0),
            (h.right_bottom(), h.right_bottom() + vec2(-r, -r), 0.0),
            (h.left_bottom(), h.left_bottom() + vec2(r, -r), 90.0),
        ];
        for (corner, center, start) in corners {
            corner_piece(&mut mesh, corner, center, r, start, color);
        }
    }
    Shape::mesh(mesh)
}

/// The area between a square corner and its rounding: a fan from the corner
/// to the quarter arc.
fn corner_piece(mesh: &mut Mesh, corner: Pos2, center: Pos2, r: f32, start: f32, color: Color32) {
    const STEPS: usize = 8;
    let base = mesh.vertices.len() as u32;
    mesh.vertices.push(Vertex {
        pos: corner,
        uv: WHITE_UV,
        color,
    });
    for i in 0..=STEPS {
        let angle = (start + 90.0 * i as f32 / STEPS as f32).to_radians();
        mesh.vertices.push(Vertex {
            pos: center + vec2(angle.cos(), angle.sin()) * r,
            uv: WHITE_UV,
            color,
        });
    }
    for i in 0..STEPS as u32 {
        mesh.indices.extend([base, base + 1 + i, base + 2 + i]);
    }
}

/// The light sweep over `rect`'s rounded shape: a diagonal white band
/// (peak alpha 0.32) crossing in ~1 s every 2.4 s. egui clips to
/// rectangles only, so the band is built from scanlines of the rounded
/// outline. *Reduce motion*: a static faint highlight.
fn glint_shape(ctx: &Context, rect: Rect) -> Shape {
    let rect = rect.expand(1.0);
    let still = motion::duration(ctx, 1.0) == 0.0;
    let time = ctx.input(|i| i.time);
    // The band's centre along the diagonal axis (x + y), from before the
    // shape to after it.
    let span = rect.width() + rect.height();
    let phase = (time % GLINT_PERIOD) / GLINT_CROSS;
    let progress = motion::IN_OUT.eval(phase.min(1.0) as f32);
    let band_center = rect.left() + rect.top() - span * 0.25 + progress * span * 1.5;
    let band_width = 28.0;
    let alpha_at = |pos: Pos2| -> f32 {
        if still {
            return 0.10;
        }
        if phase > 1.0 {
            return 0.0;
        }
        let d = ((pos.x + pos.y) - band_center).abs() / band_width;
        0.32 * (1.0 - d).max(0.0)
    };
    if !still {
        ctx.request_repaint();
    }
    let r = (radius::CONTROL as f32).min(rect.height() / 2.0);
    let mut mesh = Mesh::default();
    let row = 2.0;
    let col = 4.0;
    let mut y = rect.top();
    while y < rect.bottom() {
        let y2 = (y + row).min(rect.bottom());
        let (l1, r1) = extent(rect, r, y);
        let (l2, r2) = extent(rect, r, y2);
        let columns = ((rect.width() / col).ceil() as usize).max(1);
        let base = mesh.vertices.len() as u32;
        for i in 0..=columns {
            let t = i as f32 / columns as f32;
            for (yy, left, right) in [(y, l1, r1), (y2, l2, r2)] {
                let pos = pos2(left + (right - left) * t, yy);
                let a = alpha_at(pos);
                mesh.vertices.push(Vertex {
                    pos,
                    uv: WHITE_UV,
                    color: Color32::from_white_alpha((a * 255.0) as u8),
                });
            }
        }
        for i in 0..columns as u32 {
            let (a, b, c, d) = (
                base + 2 * i,
                base + 2 * i + 1,
                base + 2 * i + 2,
                base + 2 * i + 3,
            );
            mesh.indices.extend([a, b, c, b, d, c]);
        }
        y = y2;
    }
    Shape::mesh(mesh)
}

/// Left and right of a rounded rect (corner `r`) at height `y`.
fn extent(rect: Rect, r: f32, y: f32) -> (f32, f32) {
    let dy = if y < rect.top() + r {
        rect.top() + r - y
    } else if y > rect.bottom() - r {
        y - (rect.bottom() - r)
    } else {
        0.0
    };
    let inset = r - (r * r - dy.min(r) * dy.min(r)).max(0.0).sqrt();
    (rect.left() + inset, rect.right() - inset)
}

/// Where the popover goes: right of the spotlight, else below, else left,
/// else above; on no spotlight, the window's centre.
fn place(spot: Rect, size: egui::Vec2, screen: Rect) -> Pos2 {
    let inner = screen.shrink(MARGIN);
    if spot.width() < 1.0 {
        return inner.center() - size / 2.0;
    }
    let clamp_y = |y: f32| y.clamp(inner.top(), (inner.bottom() - size.y).max(inner.top()));
    let clamp_x = |x: f32| x.clamp(inner.left(), (inner.right() - size.x).max(inner.left()));
    if spot.right() + GAP + size.x <= inner.right() {
        return pos2(spot.right() + GAP, clamp_y(spot.top()));
    }
    if spot.bottom() + GAP + size.y <= inner.bottom() {
        return pos2(clamp_x(spot.left()), spot.bottom() + GAP);
    }
    if spot.left() - GAP - size.x >= inner.left() {
        return pos2(spot.left() - GAP - size.x, clamp_y(spot.top()));
    }
    if spot.top() - GAP - size.y >= inner.top() {
        return pos2(clamp_x(spot.left()), spot.top() - GAP - size.y);
    }
    // Nowhere free: over the spotlight's corner, inside the window.
    pos2(
        clamp_x(inner.right() - size.x),
        clamp_y(inner.bottom() - size.y),
    )
}

/// The popover; returns what was clicked and its rect.
fn popover(ctx: &Context, view: &View, hole: Rect, screen: Rect) -> (Option<Clicked>, Rect) {
    let p = &Palette::ONE_DARK;
    let size_id = Id::unique("tour_popover_size");
    let height: f32 = ctx.data(|d| d.get_temp(size_id)).unwrap_or(220.0);
    let target = Rect::from_min_size(
        place(hole, vec2(POPOVER, height), screen),
        vec2(POPOVER, height),
    );
    let at = motion::tween_rect(ctx, Id::unique("tour_popover"), view.index, target, GLIDE);
    let mut clicked = None;
    let mut terms: Vec<(Rect, &'static str)> = Vec::new();
    let area = Area::new(Id::unique("tour_popover_area"))
        .order(Order::Tooltip)
        .fixed_pos(at.min)
        .default_height(600.0)
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.bg_dark)
                .stroke(Stroke::new(1.0, p.selection))
                .corner_radius(radius::LARGE)
                .inner_margin(Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(POPOVER - 36.0);
                    ui.spacing_mut().item_spacing.y = 10.0;
                    clicked = contents(ui, view, &mut terms);
                });
        });
    let rect = area.response.rect;
    if (rect.height() - height).abs() > 0.5 {
        ctx.data_mut(|d| d.insert_temp(size_id, rect.height()));
        ctx.request_repaint();
    }
    term_tooltip(ctx, &terms, rect, screen);
    (clicked, rect)
}

fn contents(ui: &mut Ui, view: &View, terms: &mut Vec<(Rect, &'static str)>) -> Option<Clicked> {
    let p = &Palette::ONE_DARK;
    let step = view.step;
    let last = view.index + 1 == view.total;
    let mut clicked = None;
    // Header: "Chapter · N of 27", and *Skip tour*.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(
            RichText::new(step.chapter.title())
                .font(fonts::semibold(12.0))
                .color(p.accent),
        );
        ui.label(
            RichText::new(format!("· {} of {}", view.index + 1, view.total))
                .size(12.0)
                .color(p.dim),
        );
        if !last {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new("Skip tour").ghost().small()).clicked() {
                    clicked = Some(Clicked::Skip);
                }
            });
        }
    });
    ui.label(
        RichText::new(step.title)
            .font(fonts::semibold(16.0))
            .color(p.text_strong),
    );
    text_with_terms(ui, step.text, terms);
    if view.pending
        && let Some((_, hint)) = step.action
    {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            icons::show(ui, icons::MOUSE_POINTER_CLICK, 14.0, p.accent);
            ui.label(RichText::new(hint).size(12.0).color(p.accent));
        });
    }
    if step.tray_picture {
        let size = tray_menu::size(view.tray);
        let scale = (ui.available_width() / size.x).min(1.0);
        let (rect, _) = ui.allocate_exact_size(size * scale, Sense::hover());
        // Drawn at full size, then shown scaled down when it doesn't fit.
        if scale < 1.0 {
            ui.painter().rect_filled(rect, radius::LARGE, p.bg);
        }
        let full = Rect::from_min_size(rect.min, size);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(full));
        child.set_clip_rect(rect);
        tray_menu::picture(&mut child, full, view.tray);
    }
    // Footer: the progress bar, *Back*, the primary button.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (bar, _) = ui.allocate_exact_size(vec2(110.0, 4.0), Sense::hover());
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), p.surface);
        let fraction = (view.index + 1) as f32 / view.total as f32;
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * fraction, bar.height()));
        let fill = motion::tween_rect(
            ui.ctx(),
            Id::unique("tour_progress"),
            view.index,
            fill,
            GLIDE,
        );
        ui.painter()
            .rect_filled(fill, CornerRadius::same(2), p.accent);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if last {
                "Open my hosts"
            } else if view.pending {
                "Show me"
            } else {
                "Next"
            };
            let mut primary = Button::new(label).primary().small();
            if view.busy {
                primary = primary.tip("Wait until it finishes");
            }
            if ui.add_enabled(!view.busy, primary).clicked() {
                clicked = Some(Clicked::Primary);
            }
            if ui.add(Button::new("Back").small()).clicked() {
                clicked = Some(Clicked::Back);
            }
        });
    });
    clicked
}

/// The step text with its `[terms]` in place: a dotted Magenta underline,
/// and their rects for the tooltip.
fn text_with_terms(ui: &mut Ui, text: &str, terms: &mut Vec<(Rect, &'static str)>) {
    let p = &Palette::ONE_DARK;
    let font = FontId::proportional(13.0);
    let mut job = LayoutJob::default();
    let mut spans: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut chars = 0;
    for (part, term) in steps::parts(text) {
        let color = if term { p.text_strong } else { p.text };
        job.append(part, 0.0, TextFormat::simple(font.clone(), color));
        let len = part.chars().count();
        if term && let Some(tip) = steps::gloss(part) {
            spans.push((chars, chars + len, tip));
        }
        chars += len;
    }
    job.wrap.max_width = ui.available_width();
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let origin = rect.min;
    for &(start, end, tip) in &spans {
        // One rect per row the term spans.
        let mut row_start = start;
        while row_start < end {
            let a = galley.pos_from_cursor(CCursor::new(row_start));
            let mut row_end = row_start + 1;
            while row_end < end
                && (galley.pos_from_cursor(CCursor::new(row_end + 1)).top() - a.top()).abs() < 1.0
            {
                row_end += 1;
            }
            let b = galley.pos_from_cursor(CCursor::new(row_end));
            let span = Rect::from_min_max(
                origin + vec2(a.left(), a.top()),
                origin + vec2(b.left().max(a.left() + 4.0), a.bottom()),
            );
            dotted_underline(ui.painter(), span, p.accent);
            terms.push((span, tip));
            row_start = row_end;
        }
    }
    ui.painter().galley(origin, galley, p.text);
}

fn dotted_underline(painter: &egui::Painter, span: Rect, color: Color32) {
    let y = span.bottom() + 1.0;
    let mut x = span.left();
    while x < span.right() {
        painter.circle_filled(pos2(x + 0.75, y), 0.75, color);
        x += 3.0;
    }
}

/// The hovered term's tooltip, at the popover's width: below it in the
/// window's upper part, above it otherwise; never outside the window.
fn term_tooltip(ctx: &Context, terms: &[(Rect, &'static str)], popover: Rect, screen: Rect) {
    let p = &Palette::ONE_DARK;
    let hovered = ctx
        .pointer_hover_pos()
        .and_then(|pos| terms.iter().find(|(r, _)| r.expand(2.0).contains(pos)))
        .map(|(_, tip)| *tip);
    let id = Id::unique("tour_term_tip");
    let shown = motion::fade(ctx, id, hovered.is_some(), 0.120, motion::ease_out);
    if let Some(tip) = hovered {
        ctx.data_mut(|d| d.insert_temp(id.with("text"), tip));
    }
    let Some(text) = ctx.data(|d| d.get_temp::<&'static str>(id.with("text"))) else {
        return;
    };
    if shown <= 0.0 {
        return;
    }
    let below = popover.center().y < screen.center().y;
    let height_id = id.with("height");
    let height: f32 = ctx.data(|d| d.get_temp(height_id)).unwrap_or(60.0);
    let y = if below {
        (popover.bottom() + 8.0).min(screen.bottom() - height - MARGIN)
    } else {
        (popover.top() - 8.0 - height).max(screen.top() + MARGIN)
    };
    let area = Area::new(id.with("area"))
        .order(Order::Tooltip)
        .fixed_pos(pos2(popover.left(), y))
        .interactable(false)
        .show(ctx, |ui| {
            ui.multiply_opacity(shown);
            Frame::new()
                .fill(p.tooltip)
                .stroke(Stroke::new(1.0, p.selection))
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(popover.width() - 24.0);
                    ui.add(
                        egui::Label::new(RichText::new(text).size(12.0).color(p.text_strong))
                            .wrap(),
                    );
                });
        });
    ctx.data_mut(|d| d.insert_temp(height_id, area.response.rect.height()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_popover_goes_right_then_below_then_left_then_above() {
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0));
        let size = vec2(POPOVER, 200.0);
        let spot = Rect::from_min_size(pos2(100.0, 100.0), vec2(200.0, 40.0));
        assert_eq!(place(spot, size, screen), pos2(316.0, 100.0));
        let wide = Rect::from_min_size(pos2(100.0, 100.0), vec2(1000.0, 40.0));
        assert_eq!(place(wide, size, screen), pos2(100.0, 156.0));
        let right_tall = Rect::from_min_size(pos2(700.0, 20.0), vec2(560.0, 700.0));
        assert_eq!(
            place(right_tall, size, screen),
            pos2(700.0 - 16.0 - 340.0, 20.0)
        );
        // No spotlight: centred.
        let none = Rect::from_center_size(screen.center(), vec2(0.0, 0.0));
        assert_eq!(place(none, size, screen), pos2(470.0, 300.0));
    }

    #[test]
    fn the_dimming_leaves_the_holes_open() {
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(100.0, 100.0));
        let hole = Rect::from_min_size(pos2(20.0, 20.0), vec2(40.0, 30.0));
        let Shape::Mesh(mesh) = dim(screen, &[hole], Color32::BLACK) else {
            panic!("a mesh");
        };
        // Every triangle's centre lies outside the hole's inner part.
        let inner = hole.shrink(HOLE_RADIUS);
        for tri in mesh.indices.chunks(3) {
            let c = tri
                .iter()
                .map(|&i| mesh.vertices[i as usize].pos.to_vec2())
                .fold(vec2(0.0, 0.0), |a, b| a + b)
                / 3.0;
            assert!(!inner.contains(c.to_pos2()), "{c:?} dims the hole");
        }
    }

    #[test]
    fn rounded_extent() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(100.0, 40.0));
        assert_eq!(extent(rect, 8.0, 20.0), (0.0, 100.0));
        let (l, r) = extent(rect, 8.0, 0.0);
        assert_eq!((l, r), (8.0, 92.0));
    }
}
