//! Status marks and chips (SPEC.md §10 *Visual design*): a filled dot is a
//! settled state (running green, stopped grey, failed red), a ring a
//! transition (starting yellow, waiting blue), pulsing while it lasts. They
//! are painted shapes, never glyphs. Chips are small rounded labels; filter
//! chips can be clicked.

use egui::{
    Color32, CursorIcon, Painter, Pos2, Response, RichText, Role, Sense, Stroke, StrokeKind, Ui,
    Vec2, WidgetInfo, vec2,
};
use sshelf_core::tunnels::TunnelType;

use crate::icons;
use crate::motion;
use crate::theme::{Palette, radius, timing};

/// Diameter of a status dot.
pub const DOT: f32 = 8.0;

/// A status dot in the layout; hover it for a tooltip.
pub fn dot(ui: &mut Ui, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(DOT), Sense::hover());
    paint_dot(ui.painter(), rect.center(), DOT, color);
    response
}

/// A dot of `diameter` at `center`, e.g. on top of another shape.
pub fn paint_dot(painter: &Painter, center: Pos2, diameter: f32, color: Color32) {
    painter.circle_filled(center, diameter / 2.0, color);
}

/// A ring of `diameter` (2 px stroke inside it) at `center`.
pub fn paint_ring(painter: &Painter, center: Pos2, diameter: f32, color: Color32) {
    painter.circle_stroke(center, diameter / 2.0 - 1.0, Stroke::new(2.0, color));
}

/// A pulsing ring in the layout (something in progress).
pub fn pulsing_ring(ui: &mut Ui, diameter: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let alpha = motion::pulse(ui.ctx());
    paint_ring(
        ui.painter(),
        rect.center(),
        diameter,
        color.gamma_multiply(alpha),
    );
    response
}

/// A small label on a tinted background, e.g. a green "1 tunnel".
pub fn chip(ui: &mut Ui, text: &str, fill: Color32, color: Color32) -> Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), color);
    let size = galley.size() + vec2(12.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, radius::CONTROL, fill);
    ui.painter()
        .galley(rect.min + vec2(6.0, 2.0), galley, Color32::PLACEHOLDER);
    response
}

/// A clickable filter chip: selected ones are raised and bright.
pub fn filter_chip(ui: &mut Ui, icon: Option<char>, text: &str, selected: bool) -> Response {
    let p = &Palette::ONE_DARK;
    let color = if selected { p.text_strong } else { p.text };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), color);
    let icon_width = if icon.is_some() {
        icons::INLINE + 4.0
    } else {
        0.0
    };
    let size = vec2(galley.size().x + icon_width + 16.0, galley.size().y + 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = response.enabled();
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, selected, text));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered() && !selected,
        timing::HOVER,
        motion::ease_out,
    );
    let fill = if selected {
        p.selection
    } else {
        p.surface.lerp_to_gamma(p.selection, hover)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, fill);
    let mut x = rect.left() + 8.0;
    if let Some(icon) = icon {
        painter.text(
            egui::pos2(x + icons::INLINE / 2.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon,
            icons::font(icons::INLINE - 2.0),
            color,
        );
        x += icon_width;
    }
    painter.galley(
        egui::pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        Color32::PLACEHOLDER,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A dim, small label (e.g. "+wildcard"), with a tooltip.
pub fn note(ui: &mut Ui, text: &str) -> Response {
    let p = &Palette::ONE_DARK;
    ui.add(
        egui::Label::new(RichText::new(text).size(11.0).color(p.dim))
            .selectable(false)
            .sense(Sense::hover()),
    )
}

/// A tinted banner (SPEC.md §10: the status tint behind full-colour text),
/// 8 px corners, as wide as the space left.
pub fn banner<R>(ui: &mut Ui, tint: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(tint)
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
            add(ui)
        })
        .inner
}

/// A dialog's 36 px icon tile: `icon` in `color` on `fill`.
pub fn tile(ui: &mut Ui, icon: char, fill: Color32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
    ui.painter().rect_filled(rect, radius::CONTROL, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        icons::font(18.0),
        color,
    );
}

/// A tunnel's type (SPEC.md §10 *kubectl tunnels*): ssh is Lucide `terminal`
/// in secondary grey, kubectl `ship-wheel` in Blue; on a `tile`-sized
/// `#2C313A` tile (8 px corners, icon 14), or bare at 14 px with `None`.
/// The tooltip names the type.
pub fn type_mark(ui: &mut Ui, kind: TunnelType, tile: Option<f32>) -> Response {
    let p = &Palette::ONE_DARK;
    let (icon, color, name) = match kind {
        TunnelType::Ssh => (icons::TERMINAL, p.dim, "ssh tunnel"),
        TunnelType::Kubectl => (icons::SHIP_WHEEL, p.blue, "kubectl tunnel"),
    };
    let size = tile.unwrap_or(icons::INLINE);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if tile.is_some() {
        ui.painter().rect_filled(rect, radius::CONTROL, p.surface);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        icons::font(icons::INLINE),
        color,
    );
    crate::widgets::popup::tip(response, |ui| {
        ui.label(name);
    })
}

/// An outline around `rect` inside it, e.g. a row whose menu is open.
pub fn outline(painter: &Painter, rect: egui::Rect, color: Color32) {
    painter.rect_stroke(
        rect,
        radius::CONTROL,
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
}
