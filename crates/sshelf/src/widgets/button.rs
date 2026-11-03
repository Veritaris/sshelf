//! Buttons (SPEC.md §10, board *Components*): Primary (Magenta), Secondary
//! (raised) and icon-only; 32 px high (28 small), 8 px corners, the hover
//! background fading in 80 ms. An icon is painted, so it is never part of
//! the accessible name.

use egui::{
    Align2, Color32, CursorIcon, FontId, Response, Role, Sense, Ui, Widget, WidgetInfo, vec2,
};

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
}

impl<'a> Button<'a> {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            lead: Lead::None,
            kind: Kind::Secondary,
            name: None,
            tip: None,
        }
    }

    /// A 32 × 32 icon-only button; `name` is its accessible name and tooltip.
    pub fn icon_only(icon: char, name: &'a str) -> Self {
        Self {
            text: String::new(),
            lead: Lead::Icon(icon),
            kind: Kind::Secondary,
            name: Some(name),
            tip: Some(name),
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

const HEIGHT: f32 = 32.0;
const PADDING: f32 = 12.0;
const ICON: f32 = 16.0;
const GAP: f32 = 6.0;

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = &Palette::ONE_DARK;
        let font = match self.kind {
            Kind::Primary => fonts::semibold(13.0),
            Kind::Secondary => FontId::proportional(13.0),
        };
        let (fg, fill, hover_fill, pressed_fill) = match self.kind {
            Kind::Primary => (p.bg_dark, p.accent, p.accent_hover, p.accent_pressed),
            Kind::Secondary => (p.text, p.surface, p.selection, p.border),
        };
        let galley = (!self.text.is_empty())
            .then(|| ui.painter().layout_no_wrap(self.text.clone(), font, fg));
        let lead = !matches!(self.lead, Lead::None);
        let width = match &galley {
            None => HEIGHT,
            Some(g) => PADDING * 2.0 + g.size().x + if lead { ICON + GAP } else { 0.0 },
        };
        let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::click());
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
        let mut x = match &galley {
            None => rect.center().x - ICON / 2.0,
            Some(_) => rect.left() + PADDING,
        };
        match self.lead {
            Lead::None => {}
            Lead::Icon(icon) => {
                painter.text(
                    egui::pos2(x + ICON / 2.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    icon,
                    icons::font(ICON),
                    fg,
                );
            }
            Lead::Busy => {
                let alpha = motion::pulse(ui.ctx());
                marks::paint_ring(
                    painter,
                    egui::pos2(x + ICON / 2.0, rect.center().y),
                    marks::DOT,
                    p.yellow.gamma_multiply(alpha),
                );
            }
        }
        if lead {
            x += ICON + GAP;
        }
        if let Some(galley) = galley {
            let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
            painter.galley(pos, galley, Color32::PLACEHOLDER);
        }
        let response = if enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response.on_hover_cursor(CursorIcon::NotAllowed)
        };
        match self.tip {
            Some(tip) => popup::tip_below(response, |ui| {
                ui.label(tip);
            }),
            None => response,
        }
    }
}
