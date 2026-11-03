//! Buttons (SPEC.md §10, board *Components*): Primary (Magenta), Secondary
//! (raised) and icon-only; 32 px high (28 small), 8 px corners, the hover
//! background fading in 80 ms. An icon is painted, so it is never part of
//! the accessible name.

use egui::{Align2, CursorIcon, FontId, Response, Role, Sense, Ui, Widget, WidgetInfo, vec2};

use crate::theme::{Palette, radius, timing};
use crate::widgets::{marks, popup};
use crate::{fonts, icons, motion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
}

/// What sits before the text.
#[derive(Debug, Clone, Copy)]
enum Lead {
    None,
    Icon(char),
    /// A pulsing yellow ring: something is in progress.
    Busy,
}

pub struct Button<'a> {
    text: String,
    lead: Lead,
    kind: Kind,
    /// Accessible name and tooltip of an icon-only button.
    name: Option<&'a str>,
    tip: Option<&'a str>,
    small: bool,
}

impl<'a> Button<'a> {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            lead: Lead::None,
            kind: Kind::Secondary,
            name: None,
            tip: None,
            small: false,
        }
    }

    /// 28 px high, 12 px text, 14 px icon (health lines, banners, rows).
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// A 32 × 32 icon-only button; `name` is its accessible name and tooltip.
    pub fn icon_only(icon: char, name: &'a str) -> Self {
        Self {
            text: String::new(),
            lead: Lead::Icon(icon),
            kind: Kind::Secondary,
            name: Some(name),
            tip: Some(name),
            small: false,
        }
    }

    pub fn icon(mut self, icon: char) -> Self {
        self.lead = Lead::Icon(icon);
        self
    }

    pub fn busy(mut self) -> Self {
        self.lead = Lead::Busy;
        self
    }

    pub fn primary(mut self) -> Self {
        self.kind = Kind::Primary;
        self
    }

    /// The accessible name, when it should say more than the text.
    pub fn name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    /// A tooltip below the button.
    pub fn tip(mut self, tip: &'a str) -> Self {
        self.tip = Some(tip);
        self
    }
}

const GAP: f32 = 6.0;

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = &Palette::ONE_DARK;
        // Height, padding, icon and text size.
        let (height, padding, icon_size, text_size) = if self.small {
            (28.0, 10.0, 14.0, 12.0)
        } else {
            (32.0, 12.0, 16.0, 13.0)
        };
        let font = match self.kind {
            Kind::Primary => fonts::semibold(text_size),
            Kind::Secondary => FontId::proportional(text_size),
        };
        let (fg, fill, hover_fill, pressed_fill) = match self.kind {
            Kind::Primary => (p.bg_dark, p.accent, p.accent_hover, p.accent_pressed),
            Kind::Secondary => (p.text, p.surface, p.selection, p.border),
        };
        let galley = (!self.text.is_empty())
            .then(|| ui.painter().layout_no_wrap(self.text.clone(), font, fg));
        let lead = !matches!(self.lead, Lead::None);
        let width = match &galley {
            None => height,
            Some(g) => padding * 2.0 + g.size().x + if lead { icon_size + GAP } else { 0.0 },
        };
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        let enabled = ui.is_enabled();
        let name = self.name.unwrap_or(&self.text).to_string();
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &name));
        let hover = motion::fade(
            ui.ctx(),
            response.id.with("hover"),
            response.hovered() && enabled,
            timing::HOVER,
            motion::ease_out,
        );
        let fill = if response.is_pointer_button_down_on() && enabled {
            pressed_fill
        } else {
            fill.lerp_to_gamma(hover_fill, hover)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, radius::CONTROL, fill);
        let fg = if enabled { fg } else { fg.gamma_multiply(0.45) };
        let mut x = match &galley {
            None => rect.center().x - icon_size / 2.0,
            Some(_) => rect.left() + padding,
        };
        match self.lead {
            Lead::None => {}
            Lead::Icon(icon) => {
                painter.text(
                    egui::pos2(x + icon_size / 2.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    icon,
                    icons::font(icon_size),
                    fg,
                );
            }
            Lead::Busy => {
                let alpha = motion::pulse(ui.ctx());
                marks::paint_ring(
                    painter,
                    egui::pos2(x + icon_size / 2.0, rect.center().y),
                    marks::DOT,
                    p.yellow.gamma_multiply(alpha),
                );
            }
        }
        if lead {
            x += icon_size + GAP;
        }
        if let Some(galley) = galley {
            let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
            painter.galley_with_override_text_color(pos, galley, fg);
        }
        let response = if enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            // A disabled widget is never "hovered".
            if response.contains_pointer() {
                ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
            }
            response
        };
        match self.tip {
            Some(tip) => popup::tip_below(response, |ui| {
                ui.label(tip);
            }),
            None => response,
        }
    }
}

/// A Blue text link that turns lighter on hover.
pub fn link(ui: &mut Ui, text: &str, size: f32) -> Response {
    let p = &Palette::ONE_DARK;
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(size), p.blue);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    let enabled = response.enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Link, enabled, text));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    ui.painter().galley_with_override_text_color(
        rect.min,
        galley,
        p.blue.lerp_to_gamma(p.link_hover, hover),
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}
