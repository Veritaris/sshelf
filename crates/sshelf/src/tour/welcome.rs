//! The first-launch screen (SPEC.md §10 *Onboarding tour*, board
//! `Tour-Welcome`): shown instead of the main UI while `settings.toml` has
//! no `tour` key. *Take the tour*, or *Start with my config*.

use egui::{
    Align, CornerRadius, FontId, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, pos2, vec2,
};
use sshelf_core::core::{ConfigSnapshot, ConfigState};

use crate::theme::{Palette, radius};
use crate::widgets::button::Button;
use crate::{fonts, icons};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// *Start the tour*.
    Tour,
    /// *Open my hosts*.
    MyHosts,
    /// *New host* (no config yet).
    NewHost,
}

const CARDS: f32 = 880.0;

/// Draws the screen over `ui`; returns the choice made.
pub fn show(ui: &mut Ui, config: Option<&ConfigSnapshot>) -> Option<Choice> {
    let p = &Palette::ONE_DARK;
    let mut choice = None;
    let area = ui.max_rect();
    let width = CARDS.min(area.width() - 64.0);
    // Laid out once to measure, then centred.
    let height = 520.0;
    let top = (area.center().y - height / 2.0).max(area.top() + 8.0);
    let column = Rect::from_min_size(
        pos2(area.center().x - width / 2.0, top),
        vec2(width, height),
    );
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(column)
            .layout(Layout::top_down(Align::Center)),
    );
    ui.spacing_mut().item_spacing.y = 16.0;
    let (logo, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
    paint_logo(ui.painter(), logo);
    ui.label(
        RichText::new("Welcome to sshelf")
            .font(fonts::semibold(24.0))
            .color(p.text_strong),
    );
    ui.add(
        egui::Label::new(
            RichText::new(
                "A visual layer over your own ~/.ssh/config: browse hosts, connect, forward ports \
                 and run groups of tunnels from the window or the tray.",
            )
            .size(14.0)
            .color(p.dim),
        )
        .wrap(),
    );
    ui.add_space(12.0);
    let card_width = (width - 20.0) / 2.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 20.0;
        card(ui, card_width, true, |ui| {
            heading(ui, icons::COMPASS, "Take the tour");
            dim(
                ui,
                "About 6 minutes: 6 short chapters with sample hosts and tunnels. Skip any time.",
            );
            point(
                ui,
                icons::CIRCLE_CHECK,
                "Everything really works: start a tunnel, watch it fail, fix it.",
            );
            point(
                ui,
                icons::SHIELD_CHECK,
                "Nothing touches your files or connects anywhere: the sample lives in a \
                 temporary folder that is deleted afterwards.",
            );
            if ui.add(Button::new("Start the tour").primary()).clicked() {
                choice = Some(Choice::Tour);
            }
        });
        card(ui, card_width, false, |ui| {
            heading(ui, icons::FILE_TEXT, "Start with my config");
            let found = config.map(|c| (c.path.display().to_string(), &c.state));
            match found {
                Some((path, ConfigState::Loaded { hosts, .. })) => {
                    let n = hosts.hosts.len();
                    dim(
                        ui,
                        &format!(
                            "Found {path} with {n} host{}.",
                            if n == 1 { "" } else { "s" }
                        ),
                    );
                    point(
                        ui,
                        icons::SHIELD_CHECK,
                        "sshelf only reads it. It writes only when you save an edit, and backs \
                         it up first.",
                    );
                    if ui.add(Button::new("Open my hosts")).clicked() {
                        choice = Some(Choice::MyHosts);
                    }
                }
                Some((_, ConfigState::Missing)) => {
                    dim(
                        ui,
                        "No ~/.ssh/config yet \u{2014} your first host creates it.",
                    );
                    if ui.add(Button::new("New host")).clicked() {
                        choice = Some(Choice::NewHost);
                    }
                }
                Some((path, ConfigState::Error(e))) => {
                    dim(ui, &format!("{path} could not be read: {e}"));
                    if ui.add(Button::new("Open my hosts")).clicked() {
                        choice = Some(Choice::MyHosts);
                    }
                }
                None => {
                    ui.spinner();
                }
            }
        });
    });
    ui.add_space(8.0);
    footnote(
        &mut ui,
        icons::INFO,
        "You can take the tour again any time from Settings \u{203a} Help.",
    );
    // The update check's first-run note (SPEC.md §10 *Update check*).
    ui.add_space(4.0);
    footnote(&mut ui, icons::CIRCLE_ARROW_UP, UPDATE_NOTE);
    choice
}

/// The update check's line under the cards.
pub const UPDATE_NOTE: &str = "sshelf checks GitHub for new versions at start and once a day. \
                               Turn it off in Settings \u{203a} Updates.";

/// A centred dim line under the cards with its 14 px icon.
fn footnote(ui: &mut Ui, icon: char, text: &str) {
    let p = &Palette::ONE_DARK;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let galley =
            ui.painter()
                .layout_no_wrap(text.to_string(), FontId::proportional(12.0), p.dim);
        ui.add_space(((ui.available_width() - galley.size().x - 20.0) / 2.0).max(0.0));
        crate::icons::show(ui, icon, 14.0, p.dim);
        ui.label(RichText::new(text).size(12.0).color(p.dim));
    });
}

/// A choice card: 16 px corners; the tour's has a Magenta outline.
fn card(ui: &mut Ui, width: f32, main: bool, add: impl FnOnce(&mut Ui)) {
    let p = &Palette::ONE_DARK;
    let response = Frame::new()
        .fill(p.surface)
        .corner_radius(radius::LARGE)
        .inner_margin(Margin::same(24))
        .show(ui, |ui| {
            ui.set_width(width - 48.0);
            ui.set_min_height(230.0);
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.with_layout(Layout::top_down(Align::Min), add);
        })
        .response;
    if main {
        ui.painter().rect_stroke(
            response.rect,
            radius::LARGE,
            Stroke::new(1.0, p.accent),
            StrokeKind::Inside,
        );
    }
}

fn heading(ui: &mut Ui, icon: char, text: &str) {
    let p = &Palette::ONE_DARK;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        crate::icons::show(ui, icon, 18.0, p.accent);
        ui.label(
            RichText::new(text)
                .font(fonts::semibold(17.0))
                .color(p.text_strong),
        );
    });
}

fn dim(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(13.0).color(Palette::ONE_DARK.dim)).wrap());
}

fn point(ui: &mut Ui, icon: char, text: &str) {
    let p = &Palette::ONE_DARK;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        crate::icons::show(ui, icon, 14.0, p.green);
        ui.add(egui::Label::new(RichText::new(text).size(13.0).color(p.text)).wrap());
    });
}

/// The app mark at any size: a Magenta square with three shelf bars.
fn paint_logo(painter: &egui::Painter, rect: Rect) {
    let p = &Palette::ONE_DARK;
    let unit = rect.width() / 32.0;
    painter.rect_filled(rect, CornerRadius::same((8.0 * unit) as u8), p.accent);
    let bar = vec2(rect.width() - 14.0 * unit, 3.0 * unit);
    for i in 0..3 {
        let top = rect.center().y - 8.5 * unit + i as f32 * 7.0 * unit;
        painter.rect_filled(
            Rect::from_min_size(pos2(rect.left() + 7.0 * unit, top), bar),
            1,
            p.bg,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_util;

    #[test]
    fn without_a_config_the_second_card_offers_a_new_host() {
        let snapshot = ConfigSnapshot {
            generation: 1,
            path: PathBuf::from("~/.ssh/config"),
            state: ConfigState::Missing,
            read_only: Vec::new(),
        };
        let mut picked = None;
        let mut harness = test_util::harness()
            .with_size([1280.0, 800.0])
            .build_ui(|ui| {
                if let Some(choice) = show(ui, Some(&snapshot)) {
                    picked = Some(choice);
                }
            });
        harness.step();
        harness.get_by_label("Welcome to sshelf");
        harness.get_by_label("New host").click();
        harness.step();
        harness.get_by_label("Start the tour");
        drop(harness);
        assert_eq!(picked, Some(Choice::NewHost));
    }
}
