//! Text inputs (SPEC.md §10, board *Components*): 32 px (36 in dialogs),
//! an inset fill, 8 px corners, a Magenta outline fading in on focus
//! (80 ms) and a Light Red one while the value has a problem.

use egui::{
    Color32, CursorIcon, FontId, Margin, Response, Stroke, StrokeKind, TextEdit, Ui, Widget, vec2,
};

use crate::motion;
use crate::theme::{Palette, radius, timing};

pub struct TextField<'a> {
    text: &'a mut String,
    hint: &'a str,
    mono: bool,
    error: bool,
    width: Option<f32>,
    height: f32,
    /// `None`: the style's inset colour (`extreme_bg_color`), which a pane
    /// whose own fill is the dark one sets lighter.
    fill: Option<Color32>,
}

impl<'a> TextField<'a> {
    pub fn new(text: &'a mut String) -> Self {
        Self {
            text,
            hint: "",
            mono: false,
            error: false,
            width: None,
            height: 32.0,
            fill: None,
        }
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    /// Values, paths and aliases are mono.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// The value has a problem: a Light Red outline.
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// Fixed width; otherwise the width left.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// 36 px high on `fill`, for dialogs.
    pub fn dialog(mut self, fill: Color32) -> Self {
        self.height = 36.0;
        self.fill = Some(fill);
        self
    }
}

impl Widget for TextField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = &Palette::ONE_DARK;
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, _) = ui.allocate_exact_size(vec2(width, self.height), egui::Sense::hover());
        let fill = self.fill.unwrap_or(ui.visuals().extreme_bg_color);
        ui.painter().rect_filled(rect, radius::CONTROL, fill);
        let font = if self.mono {
            FontId::monospace(13.0)
        } else {
            FontId::proportional(13.0)
        };
        // The text sits 12 px in; the edit fills the rest of the field.
        let text_rect = rect.shrink2(vec2(12.0, 0.0));
        let response = ui.put(
            text_rect,
            TextEdit::singleline(self.text)
                .frame(egui::Frame::NONE)
                .hint_text(self.hint)
                .font(font)
                .text_color(p.text_strong)
                .margin(Margin::ZERO)
                .min_size(text_rect.size())
                .align(egui::Align2::LEFT_CENTER)
                .desired_width(text_rect.width()),
        );
        let focus = motion::fade(
            ui.ctx(),
            response.id.with("focus"),
            response.has_focus(),
            timing::HOVER,
            motion::ease_out,
        );
        let outline = if self.error {
            Some(p.red)
        } else if focus > 0.0 {
            Some(p.accent.gamma_multiply(focus))
        } else {
            None
        };
        if let Some(color) = outline {
            ui.painter().rect_stroke(
                rect,
                radius::CONTROL,
                Stroke::new(1.0, color),
                StrokeKind::Inside,
            );
        }
        response.on_hover_cursor(CursorIcon::Text)
    }
}
