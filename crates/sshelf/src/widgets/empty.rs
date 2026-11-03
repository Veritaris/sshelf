//! Empty states (board *Empty states*): a glyph tile, a title, one line of
//! explanation and usually the action that fills the place.

use egui::{Align2, RichText, Ui, Vec2, WidgetText};

use crate::theme::{Palette, radius};
use crate::{fonts, icons};

/// Draws an empty state centred in `ui`; `action` adds its button(s).
pub fn empty_state<R>(
    ui: &mut Ui,
    glyph: char,
    title: &str,
    body: impl Into<WidgetText>,
    action: impl FnOnce(&mut Ui) -> R,
) -> R {
    let p = &Palette::ONE_DARK;
    ui.vertical_centered(|ui| {
        ui.add_space(32.0);
        let (tile, _) = ui.allocate_exact_size(Vec2::splat(56.0), egui::Sense::hover());
        ui.painter().rect_filled(tile, radius::LARGE, p.surface);
        ui.painter().text(
            tile.center(),
            Align2::CENTER_CENTER,
            glyph,
            icons::font(28.0),
            p.dim,
        );
        ui.add_space(12.0);
        ui.label(
            RichText::new(title)
                .font(fonts::semibold(16.0))
                .color(p.text_strong),
        );
        ui.add_space(4.0);
        ui.scope(|ui| {
            ui.set_max_width(360.0);
            ui.visuals_mut().override_text_color = Some(p.dim);
            ui.label(body);
        });
        ui.add_space(12.0);
        action(ui)
    })
    .inner
}
