//! Text inputs (SPEC.md §10, board *Components*): 32 px (36 in dialogs),
//! an inset fill, 8 px corners, a Magenta outline fading in on focus
//! (80 ms) and a Light Red one while the value has a problem.

use egui::{
    Color32, CursorIcon, FontId, Margin, Response, Stroke, StrokeKind, TextEdit, Ui, Widget, vec2,
};

use sshelf_core::redact::DOTS;

use crate::theme::{Palette, radius, timing};
use crate::widgets::masked;
use crate::{motion, privacy};

pub struct TextField<'a> {
    text: &'a mut String,
    hint: &'a str,
    mono: bool,
    error: bool,
    /// Allowed but risky (a non-loopback bind): a Light Yellow outline.
    warning: bool,
    width: Option<f32>,
    height: f32,
    /// `None`: the style's inset colour (`extreme_bg_color`), which a pane
    /// whose own fill is the dark one sets lighter.
    fill: Option<Color32>,
    password: bool,
    /// Room kept free at the right end (for a button over the field).
    inset_right: f32,
    /// Filled from stored data: masked in streamer mode until focused.
    prefilled: bool,
}

impl<'a> TextField<'a> {
    pub fn new(text: &'a mut String) -> Self {
        Self {
            text,
            hint: "",
            mono: false,
            error: false,
            warning: false,
            width: None,
            height: 32.0,
            fill: None,
            password: false,
            inset_right: 0.0,
            prefilled: false,
        }
    }

    /// The value comes from stored data (SPEC.md §10 *Streamer mode*,
    /// *Pre-filled inputs*): in streamer mode the field shows "•••••" until
    /// the user focuses it; from then on it is their input and shows as
    /// typed.
    pub fn prefilled(mut self) -> Self {
        self.prefilled = true;
        self
    }

    /// The value is allowed but risky: a Light Yellow outline.
    pub fn warning(mut self, warning: bool) -> Self {
        self.warning = warning;
        self
    }

    /// Hides the text (passphrases).
    pub fn password(mut self, hidden: bool) -> Self {
        self.password = hidden;
        self
    }

    /// Keeps `width` free at the right end, e.g. for a show / hide button.
    pub fn inset_right(mut self, width: f32) -> Self {
        self.inset_right = width;
        self
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
        let edit_id = ui.next_auto_id().with("field_edit");
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, _) = ui.allocate_exact_size(vec2(width, self.height), egui::Sense::hover());
        let fill = self.fill.unwrap_or(ui.visuals().extreme_bg_color);
        ui.painter().rect_filled(rect, radius::CONTROL, fill);
        // A pre-filled value stays covered until the field is focused once.
        let taken = edit_id.with("taken");
        if self.prefilled && privacy::get(ui.ctx()).on && !self.text.is_empty() {
            if ui.memory(|m| m.has_focus(edit_id)) {
                ui.ctx().data_mut(|d| d.insert_temp(taken, true));
            }
            if !ui
                .ctx()
                .data(|d| d.get_temp::<bool>(taken))
                .unwrap_or(false)
            {
                let response = ui.interact(rect, edit_id.with("cover"), egui::Sense::click());
                response.widget_info(|| egui::WidgetInfo::text_edit(true, "", DOTS, self.hint));
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect.shrink2(vec2(12.0, 0.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                masked::dots(&mut child, false);
                if response.clicked() {
                    ui.memory_mut(|m| m.request_focus(edit_id));
                }
                return response.on_hover_cursor(CursorIcon::Text);
            }
        }
        let font = if self.mono {
            FontId::monospace(13.0)
        } else {
            FontId::proportional(13.0)
        };
        // The text sits 12 px in; the edit fills the rest of the field.
        let mut text_rect = rect.shrink2(vec2(12.0, 0.0));
        text_rect.max.x -= self.inset_right;
        // In a child: `ui.put` would move the layout cursor to the end of
        // the text area, inside the field, and the next widget would start
        // over the field's right padding.
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let response = child.add(
            TextEdit::singleline(self.text)
                .id(edit_id)
                .password(self.password)
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
        } else if self.warning {
            Some(p.yellow)
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

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::privacy::Privacy;
    use crate::test_util::{self, drawn_text};

    /// Streamer mode: a pre-filled value shows as dots until the field is
    /// focused; from then on it shows, also after focus leaves.
    #[test]
    fn a_prefilled_value_shows_once_focused() {
        let mut harness = test_util::harness()
            .with_size([400.0, 60.0])
            .build_ui_state(
                |ui, text: &mut String| {
                    let on = Privacy {
                        on: true,
                        ..Privacy::default()
                    };
                    privacy::set(ui.ctx(), on);
                    let label = ui.label("Bind");
                    ui.add(TextField::new(text).prefilled())
                        .labelled_by(label.id);
                },
                "10.77.3.41".to_string(),
            );
        harness.run_steps(2);
        assert!(!drawn_text(&harness).contains("10.77"));
        harness.get_by_label("Bind").click();
        harness.run_steps(3);
        assert!(
            drawn_text(&harness).contains("10.77.3.41"),
            "focused: shown"
        );
        harness
            .ctx
            .memory_mut(|m| m.surrender_focus(m.focused().unwrap()));
        harness.run_steps(2);
        assert!(drawn_text(&harness).contains("10.77.3.41"), "still shown");
    }
}
