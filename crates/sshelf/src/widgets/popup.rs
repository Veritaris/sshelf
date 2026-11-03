//! Tooltips and context menus with the timing of SPEC.md §10 *Interaction
//! and motion*.
//!
//! Tooltips: after 400 ms of hover they fade in (120 ms) with a 3 px slide
//! and fade out the same way; the next tooltip within 300 ms of one closing
//! shows at once.
//!
//! Context menus (right click): fade in and scale 98 → 100 % from the click
//! point, fade out (120 ms); the hover highlight slides between rows (50 ms)
//! and fades in place when the pointer leaves.

use egui::emath::TSTransform;
use egui::{
    Align2, Area, CursorIcon, FontId, Frame, Id, Key, Margin, Order, Pos2, Rect, Response, Role,
    Sense, Shape, Stroke, Ui, WidgetInfo, vec2,
};

use crate::icons;
use crate::motion;
use crate::theme::{Palette, radius, timing};
use crate::widgets::nav;

/// Distance between the widget and its tooltip.
const GAP: f32 = 8.0;
/// How far the tooltip travels while it fades.
const SLIDE: f32 = 3.0;

/// When the last tooltip was visible (for the "next one shows at once" rule).
fn last_visible_id() -> Id {
    Id::unique("sshelf_tooltip_last_visible")
}

#[derive(Clone, Copy)]
enum Side {
    Right,
    Below,
}

/// A tooltip to the right of `response`'s widget (the rail's), built by
/// `add_contents` while it shows.
pub fn tip(response: Response, add_contents: impl FnOnce(&mut Ui)) -> Response {
    tooltip(response, Side::Right, add_contents)
}

/// A tooltip below `response`'s widget.
pub fn tip_below(response: Response, add_contents: impl FnOnce(&mut Ui)) -> Response {
    tooltip(response, Side::Below, add_contents)
}

fn tooltip(response: Response, side: Side, add_contents: impl FnOnce(&mut Ui)) -> Response {
    let ctx = response.ctx.clone();
    let id = response.id.with("tooltip");
    let now = ctx.input(|i| i.time);
    // A press hides the tooltip until the pointer leaves the widget (the
    // "next tooltip shows at once" rule is for other widgets).
    let suppressed_id = id.with("suppressed");
    let pressed = ctx.input(|i| i.pointer.any_down());
    // A disabled widget is never "hovered", but its tooltip says why it is
    // disabled (SPEC.md §10 *Interaction and motion*).
    let over = response.hovered() || (!response.enabled() && response.contains_pointer());
    if over && pressed {
        ctx.data_mut(|d| d.insert_temp(suppressed_id, true));
    } else if !over {
        ctx.data_mut(|d| d.remove::<bool>(suppressed_id));
    }
    let suppressed = ctx.data(|d| d.get_temp::<bool>(suppressed_id).unwrap_or(false));
    let hovered = over && !pressed && !suppressed;
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
        let slide = SLIDE * (1.0 - shown);
        let (pos, pivot) = match side {
            Side::Right => (
                response.rect.right_center() + vec2(GAP - slide, 0.0),
                Align2::LEFT_CENTER,
            ),
            Side::Below => (
                response.rect.left_bottom() + vec2(0.0, GAP - slide),
                Align2::LEFT_TOP,
            ),
        };
        Area::new(id)
            .order(Order::Tooltip)
            .fixed_pos(pos)
            .pivot(pivot)
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
                        ui.set_max_width(320.0);
                        ui.style_mut().override_font_id = Some(FontId::proportional(12.0));
                        ui.visuals_mut().override_text_color = Some(p.text_strong);
                        add_contents(ui);
                    });
            });
    }
    response
}

/// Width of a context menu.
const MENU_WIDTH: f32 = 240.0;
const ITEM_HEIGHT: f32 = 30.0;

/// Which menu is open (only one at a time).
#[derive(Clone, Copy)]
struct OpenMenu {
    id: Id,
}

fn open_menu_id() -> Id {
    Id::unique("sshelf_open_context_menu")
}

/// Whether the context menu of `response`'s widget is open (e.g. to draw
/// the row's "menu open" outline).
pub fn menu_is_open(response: &Response) -> bool {
    let id = response.id.with("context_menu");
    response
        .ctx
        .data(|d| d.get_temp::<OpenMenu>(open_menu_id()))
        .is_some_and(|open| open.id == id)
}

/// A context menu on `response`'s widget, opened by a right click. `add`
/// builds its items each frame it shows.
pub fn context_menu(response: &Response, add: impl FnOnce(&mut Menu)) {
    let ctx = response.ctx.clone();
    let id = response.id.with("context_menu");
    let at_id = id.with("at");
    if response.secondary_clicked() {
        let at = response
            .interact_pointer_pos()
            .unwrap_or(response.rect.center());
        ctx.data_mut(|d| {
            d.insert_temp(open_menu_id(), OpenMenu { id });
            d.insert_temp(at_id, at);
        });
    }
    let open = menu_is_open(response);
    let easing = if open {
        motion::ease_out
    } else {
        motion::ease_in
    };
    let shown = motion::fade(&ctx, id, open, timing::MENU, easing);
    if shown <= 0.0 {
        return;
    }
    let Some(at) = ctx.data(|d| d.get_temp::<Pos2>(at_id)) else {
        return;
    };
    let p = &Palette::ONE_DARK;
    let mut close = false;
    let area = Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(at)
        .constrain(true)
        .fade_in(false)
        .show(&ctx, |ui| {
            ui.multiply_opacity(shown);
            Frame::new()
                .fill(p.bg_dark)
                .stroke(Stroke::new(1.0, p.selection))
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin::same(4))
                .show(ui, |ui| {
                    ui.set_width(MENU_WIDTH - 8.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut menu = Menu::new(ui, id);
                    add(&mut menu);
                    close = menu.finish();
                });
        });
    // Grows from the click point.
    let scale = 0.98 + 0.02 * shown;
    let origin = area.response.rect.min.to_vec2();
    ctx.set_transform_layer(
        area.response.layer_id,
        TSTransform::from_translation(origin)
            * TSTransform::from_scaling(scale)
            * TSTransform::from_translation(-origin),
    );
    if !open {
        return;
    }
    let clicked_outside = ctx.input(|i| {
        (i.pointer.primary_pressed() || i.pointer.secondary_pressed())
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| !area.response.rect.contains(pos))
    }) && !response.secondary_clicked();
    if close || clicked_outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
        ctx.data_mut(|d| d.remove::<OpenMenu>(open_menu_id()));
    }
}

/// The items of an open context menu.
pub struct Menu<'a> {
    ui: &'a mut Ui,
    id: Id,
    /// Where the hover highlight goes (painted under the rows).
    highlight: egui::layers::ShapeIdx,
    hovered: Option<(usize, Rect)>,
    count: usize,
    chosen: bool,
}

impl<'a> Menu<'a> {
    fn new(ui: &'a mut Ui, id: Id) -> Self {
        let highlight = ui.painter().add(Shape::Noop);
        Self {
            ui,
            id,
            highlight,
            hovered: None,
            count: 0,
            chosen: false,
        }
    }

    /// An item; returns `true` when it was clicked (the menu then closes).
    pub fn item(&mut self, icon: Option<char>, label: &str, enabled: bool) -> bool {
        let p = &Palette::ONE_DARK;
        let index = self.count;
        self.count += 1;
        let (rect, response) = self.ui.allocate_exact_size(
            vec2(self.ui.available_width(), ITEM_HEIGHT),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        response.widget_info(|| WidgetInfo::labeled(Role::MenuItem, enabled, label));
        if enabled && response.hovered() {
            self.hovered = Some((index, rect));
        }
        let on = motion::fade(
            self.ui.ctx(),
            response.id.with("hover"),
            enabled && response.hovered(),
            timing::MENU,
            motion::ease_in_out,
        );
        let color = if enabled {
            p.text.lerp_to_gamma(p.text_strong, on)
        } else {
            p.disabled_text
        };
        let painter = self.ui.painter();
        let mut x = rect.left() + 10.0;
        if let Some(icon) = icon {
            painter.text(
                egui::pos2(x + 8.0, rect.center().y),
                Align2::CENTER_CENTER,
                icon,
                icons::font(16.0),
                color,
            );
            x += 16.0 + 10.0;
        }
        painter.text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            color,
        );
        if enabled {
            response.clone().on_hover_cursor(CursorIcon::PointingHand);
        }
        let clicked = enabled && response.clicked();
        self.chosen |= clicked;
        clicked
    }

    pub fn separator(&mut self) {
        let p = &Palette::ONE_DARK;
        let (rect, _) = self
            .ui
            .allocate_exact_size(vec2(self.ui.available_width(), 9.0), Sense::hover());
        self.ui.painter().hline(
            rect.x_range().shrink(6.0),
            rect.center().y,
            Stroke::new(1.0, p.selection),
        );
    }

    /// Paints the hover highlight; returns whether an item was chosen.
    fn finish(self) -> bool {
        let p = &Palette::ONE_DARK;
        nav::hover_highlight(
            self.ui,
            self.id.with("highlight"),
            self.highlight,
            self.hovered,
            p.selection,
        );
        self.chosen
    }
}
