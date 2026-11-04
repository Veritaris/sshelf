//! The sshelf tray menu (SPEC.md §10 *Tray*; boards Tray, Tray-States): a
//! small borderless, always-on-top window docked by the tray icon, with the
//! same widgets as the main window — a status header, a banner while a host
//! key waits, group switches (*Retry* on degraded ones), the active tunnels
//! (*Stop* for a manual start; a failed one opens the window on it), the
//! hosts to connect to, *Show window* and *Quit*. It closes on focus loss,
//! `Esc` or a click that opens something.
//!
//! It must work while the main window is hidden: a right click arrives in
//! `App::logic`, which asks eframe for a full pass of the hidden root
//! (`ViewportCommand::RequestPaintWhileHidden`); from then on the visible
//! flyout keeps the root's passes running, so [`TrayMenu::show`] is called
//! from `App::ui` like every other window.
//!
//! The menu is a deferred viewport: eframe creates its window from the event
//! loop and runs its passes on their own. (An immediate viewport is created
//! inside the root's pass, which a tray click can start where no window can
//! be made; egui then panics.) Its clicks go back to the root through
//! [`Shared`].

use std::sync::{Arc, Mutex};

use egui::{
    Align, Align2, CursorIcon, FontId, Id, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, pos2, vec2,
};
use sshelf_core::supervisor::{GroupState, TunnelState};

use crate::theme::{Palette, radius, timing};
use crate::tray::{IconRect, TrayAction, TrayModel};
use crate::widgets::button::Button;
use crate::widgets::marks;
use crate::widgets::switch::switch;
use crate::{fonts, icons, motion};

const WIDTH: f32 = 340.0;
/// Padding inside the window.
const PAD: f32 = 12.0;
const HEADER: f32 = 34.0;
/// A section's overline, with the space above it.
const SECTION: f32 = 26.0;
const ROW: f32 = 36.0;
/// The host key banner.
const BANNER: f32 = 48.0;
/// The line and the *Show window* / *Quit* buttons.
const FOOTER: f32 = 46.0;
/// Hosts to connect to (the native menu has more).
const RECENT: usize = 5;
/// Space between the icon and the menu.
const GAP: f32 = 8.0;

#[derive(Default)]
pub struct TrayMenu {
    /// Open, by this icon.
    open: Option<IconRect>,
    /// Shared with the menu's own window, which runs its passes apart from
    /// the root's (a deferred viewport).
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    /// What to show; the root keeps it current.
    model: TrayModel,
    /// Clicked in the menu, for the root to carry out.
    actions: Vec<TrayAction>,
    /// The menu asked to close.
    close: bool,
    /// It had the focus once: losing it closes the menu.
    had_focus: bool,
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("sshelf_tray_menu")
}

/// The menu's height for `model` (every row has a fixed height).
fn height(model: &TrayModel) -> f32 {
    let mut height = PAD * 2.0 + HEADER + FOOTER;
    if model.host_key_waiting {
        height += BANNER;
    }
    if !model.groups.is_empty() {
        height += SECTION + model.groups.len() as f32 * ROW;
    }
    if !model.tunnels.is_empty() {
        height += SECTION + model.tunnels.len() as f32 * ROW;
    }
    height + SECTION + model.recent.len().clamp(1, RECENT) as f32 * ROW
}

/// Where the menu goes, in points: centred on the icon, above it when the
/// icon is in the lower half of the screen (a bottom taskbar), else below,
/// kept on the screen.
fn placement(
    icon: IconRect,
    pixels_per_point: f32,
    screen: egui::Vec2,
    size: egui::Vec2,
) -> egui::Pos2 {
    let ppp = f64::from(pixels_per_point);
    let (x, y) = ((icon.x / ppp) as f32, (icon.y / ppp) as f32);
    let (w, h) = ((icon.width / ppp) as f32, (icon.height / ppp) as f32);
    let left = (x + w / 2.0 - size.x / 2.0).clamp(GAP, (screen.x - size.x - GAP).max(GAP));
    let top = if y + h / 2.0 > screen.y / 2.0 {
        y - size.y - GAP
    } else {
        y + h + GAP
    };
    pos2(left, top.clamp(GAP, (screen.y - size.y - GAP).max(GAP)))
}

impl TrayMenu {
    /// Opens the menu by `icon`; call from `App::logic` (the root may be
    /// hidden, so it asks for a pass anyway).
    pub fn open(&mut self, ctx: &egui::Context, icon: IconRect) {
        self.open = Some(icon);
        {
            let mut shared = lock(&self.shared);
            shared.close = false;
            shared.had_focus = false;
            shared.actions.clear();
        }
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::RequestPaintWhileHidden);
        ctx.request_repaint();
    }

    /// The menu while open; returns what was clicked in it since the last
    /// call. Call from `App::ui` on every pass: the menu's window lives as
    /// long as it is shown each pass.
    pub fn show(&mut self, ctx: &egui::Context, model: &TrayModel) -> Vec<TrayAction> {
        let Some(icon) = self.open else {
            return Vec::new();
        };
        let (actions, close) = {
            let mut shared = lock(&self.shared);
            if shared.model != *model {
                shared.model = model.clone();
                ctx.request_repaint_of(viewport_id());
            }
            (std::mem::take(&mut shared.actions), shared.close)
        };
        if close {
            self.open = None;
            // One more pass without it removes its window.
            ctx.request_repaint();
            return actions;
        }
        let size = vec2(WIDTH, height(model));
        let (ppp, screen) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.native_pixels_per_point.unwrap_or(1.0),
                viewport.monitor_size.unwrap_or(vec2(1920.0, 1080.0)),
            )
        });
        let builder = ViewportBuilder::default()
            .with_title("sshelf menu")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(true)
            .with_position(placement(icon, ppp, screen, size))
            .with_inner_size(size);
        let shared = self.shared.clone();
        ctx.show_viewport_deferred(viewport_id(), builder, move |ui, class| {
            let (model, had_focus) = {
                let shared = lock(&shared);
                (shared.model.clone(), shared.had_focus)
            };
            let (focused, escape, close_requested) = ui.input(|i| {
                (
                    i.viewport().focused,
                    i.key_pressed(egui::Key::Escape),
                    i.viewport().close_requested(),
                )
            });
            // No native windows (tests): nothing to dock.
            let mut close = class == ViewportClass::EmbeddedWindow;
            if focused != Some(true) && !had_focus {
                ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
            }
            close |= escape || close_requested || (had_focus && focused == Some(false));
            let mut actions = Vec::new();
            if !close {
                contents(ui, &model, &mut actions);
            }
            // Anything that opens or quits closes the menu; switches don't.
            close |= actions.iter().any(|a| {
                matches!(
                    a,
                    TrayAction::Show
                        | TrayAction::Quit
                        | TrayAction::ShowTunnel(_)
                        | TrayAction::Connect(_)
                )
            });
            let mut shared = lock(&shared);
            shared.had_focus |= focused == Some(true);
            if close || !actions.is_empty() {
                shared.close |= close;
                shared.actions.extend(actions);
                // The root carries them out (and drops the window).
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
        actions
    }
}

/// Everything inside the window.
fn contents(ui: &mut Ui, model: &TrayModel, actions: &mut Vec<TrayAction>) {
    let p = &Palette::ONE_DARK;
    let rect = ui.ctx().content_rect();
    ui.painter().rect_filled(rect, radius::LARGE, p.bg_dark);
    ui.painter().rect_stroke(
        rect,
        radius::LARGE,
        Stroke::new(1.0, p.selection),
        StrokeKind::Inside,
    );
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(PAD))
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut ui;
    ui.spacing_mut().item_spacing.y = 0.0;

    // Header: the name and what runs.
    fixed(ui, HEADER, |ui| {
        ui.add_space(6.0);
        ui.label(
            RichText::new("sshelf")
                .font(fonts::semibold(14.0))
                .color(p.text_strong),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            if model.failed > 0 {
                ui.label(
                    RichText::new(format!("{} failed", model.failed))
                        .size(12.0)
                        .color(p.red),
                );
            }
            if model.running > 0 || model.failed == 0 {
                let (text, color) = if model.running > 0 {
                    (format!("{} running", model.running), p.green)
                } else {
                    ("nothing running".to_string(), p.dim)
                };
                if model.failed > 0 {
                    ui.label(RichText::new("·").size(12.0).color(p.dim));
                }
                ui.label(RichText::new(text).size(12.0).color(color));
            }
        });
    });

    if model.host_key_waiting {
        fixed(ui, BANNER, |ui| {
            let banner = ui.max_rect().shrink2(vec2(0.0, 4.0));
            ui.painter()
                .rect_filled(banner, radius::CONTROL, p.blue_tint);
            ui.add_space(10.0);
            icons::show(ui, icons::KEY_ROUND, icons::INLINE, p.blue);
            ui.add_space(6.0);
            ui.label(
                RichText::new("A host key waits for you")
                    .size(12.0)
                    .color(p.blue),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.add(Button::new("Show").small()).clicked() {
                    actions.push(TrayAction::Show);
                }
            });
        });
    }

    if !model.groups.is_empty() {
        section(ui, "Groups");
        for group in &model.groups {
            fixed(ui, ROW, |ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(&group.name).size(13.0).color(p.text));
                match group.state {
                    GroupState::Degraded { running, total } => {
                        ui.label(
                            RichText::new(format!("{running}/{total}"))
                                .size(11.0)
                                .color(p.yellow),
                        );
                    }
                    GroupState::Starting => {
                        ui.label(RichText::new("starting…").size(11.0).color(p.yellow));
                    }
                    _ => {}
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(10.0);
                    let active = group.state != GroupState::Stopped;
                    let mut on = active;
                    if switch(ui, &mut on, &group.name).changed() {
                        actions.push(if active {
                            TrayAction::StopGroup(group.id.clone())
                        } else {
                            TrayAction::StartGroup(group.id.clone())
                        });
                    }
                    if matches!(group.state, GroupState::Degraded { .. })
                        && ui
                            .add(Button::new("Retry").icon(icons::ROTATE_CW).small())
                            .clicked()
                    {
                        actions.push(TrayAction::StartGroup(group.id.clone()));
                    }
                });
            });
        }
    }

    if !model.tunnels.is_empty() {
        section(ui, "Active tunnels");
        for tunnel in &model.tunnels {
            let failed = matches!(tunnel.state, TunnelState::Failed(_));
            let response = row(ui, ("tunnel", &tunnel.id), failed, |ui| {
                ui.add_space(10.0);
                match &tunnel.state {
                    TunnelState::Running => marks::dot(ui, p.green),
                    TunnelState::Failed(_) => marks::dot(ui, p.red),
                    TunnelState::Starting => marks::pulsing_ring(ui, marks::DOT, p.yellow),
                    _ => marks::pulsing_ring(ui, marks::DOT, p.dim),
                };
                ui.add_space(8.0);
                ui.label(RichText::new(&tunnel.name).size(13.0).color(p.text));
                let (note, color) = if failed {
                    let text = if tunnel.reconnecting {
                        "reconnecting"
                    } else {
                        "failed"
                    };
                    (text.to_string(), p.red)
                } else if !tunnel.manual {
                    (format!("used by {}", tunnel.used_by.join(", ")), p.dim)
                } else {
                    (
                        tunnel.port.map(|p| format!(":{p}")).unwrap_or_default(),
                        p.dim,
                    )
                };
                ui.label(
                    RichText::new(note)
                        .font(FontId::monospace(11.0))
                        .color(color),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    let name = format!("Stop {}", tunnel.name);
                    if tunnel.manual
                        && ui
                            .add(Button::icon_only(icons::SQUARE, &name).small())
                            .clicked()
                    {
                        actions.push(TrayAction::StopTunnel(tunnel.id.clone()));
                    }
                });
            });
            if failed && response.clicked() {
                actions.push(TrayAction::ShowTunnel(tunnel.id.clone()));
            }
        }
    }

    section(ui, "Connect");
    if model.recent.is_empty() {
        fixed(ui, ROW, |ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new("No sessions launched from sshelf yet")
                    .size(12.0)
                    .color(p.dim),
            );
        });
    }
    for alias in model.recent.iter().take(RECENT) {
        let response = row(ui, ("connect", alias), true, |ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new(alias)
                    .font(FontId::monospace(12.0))
                    .color(p.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(10.0);
                icons::show(ui, icons::SQUARE_TERMINAL, icons::INLINE, p.dim);
            });
        });
        if response.clicked() {
            actions.push(TrayAction::Connect(alias.clone()));
        }
    }

    fixed(ui, FOOTER, |ui| {
        let line = ui.max_rect();
        ui.painter().hline(
            line.x_range(),
            line.top() + 6.0,
            Stroke::new(1.0, p.selection),
        );
        ui.spacing_mut().item_spacing.x = 6.0;
        let quit_width = 64.0;
        let show = Button::new("Show window")
            .small()
            .min_width(ui.available_width() - quit_width - 6.0);
        if ui.add(show).clicked() {
            actions.push(TrayAction::Show);
        }
        if ui
            .add(Button::new("Quit").small().min_width(quit_width))
            .clicked()
        {
            actions.push(TrayAction::Quit);
        }
    });
}

/// A strip `height` tall across the menu, laid out left to right.
fn fixed(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    add(&mut child);
}

/// A section's overline.
fn section(ui: &mut Ui, title: &str) {
    let p = &Palette::ONE_DARK;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), SECTION), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 10.0, rect.bottom() - 8.0),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        fonts::semibold(11.0),
        p.dim,
    );
}

/// A row; `clickable` ones get the hover background and the hand cursor.
fn row(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    clickable: bool,
    add: impl FnOnce(&mut Ui),
) -> egui::Response {
    let p = &Palette::ONE_DARK;
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    let hover = motion::fade(
        ui.ctx(),
        Id::unique(&id).with("hover"),
        clickable && response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    if hover > 0.0 {
        ui.painter()
            .rect_filled(rect, radius::CONTROL, p.surface.gamma_multiply(hover));
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    add(&mut child);
    if clickable {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::tray::{GroupRow, TunnelRow};

    fn model() -> TrayModel {
        TrayModel {
            groups: vec![GroupRow {
                id: "g-1".into(),
                name: "Work".into(),
                state: GroupState::Degraded {
                    running: 1,
                    total: 2,
                },
            }],
            tunnels: vec![TunnelRow {
                id: "t-a".into(),
                name: "API".into(),
                port: Some(6443),
                state: TunnelState::Running,
                manual: true,
                reconnecting: false,
                used_by: Vec::new(),
            }],
            recent: vec!["web".into(), "db".into()],
            host_key_waiting: true,
            ..TrayModel::default()
        }
    }

    #[test]
    fn height_counts_every_row() {
        let model = model();
        let expected = PAD * 2.0
            + HEADER
            + FOOTER
            + BANNER
            + (SECTION + ROW)
            + (SECTION + ROW)
            + (SECTION + 2.0 * ROW);
        assert_eq!(height(&model), expected);
        let empty = TrayModel::default();
        assert_eq!(height(&empty), PAD * 2.0 + HEADER + FOOTER + SECTION + ROW);
    }

    #[test]
    fn it_docks_above_a_bottom_taskbar_and_below_a_top_bar() {
        let screen = vec2(1920.0, 1080.0);
        let size = vec2(WIDTH, 300.0);
        let icon = |y| IconRect {
            x: 1800.0,
            y,
            width: 24.0,
            height: 24.0,
        };
        let bottom = placement(icon(1050.0), 1.0, screen, size);
        assert_eq!(bottom.y, 1050.0 - 300.0 - GAP);
        // Kept on the screen at the right edge.
        assert_eq!(bottom.x, 1920.0 - WIDTH - GAP);
        let top = placement(icon(4.0), 1.0, screen, size);
        assert_eq!(top.y, 4.0 + 24.0 + GAP);
        // Physical pixels at 150 %.
        let scaled = placement(icon(1575.0), 1.5, screen, size);
        assert_eq!(scaled.y, 1050.0 - 300.0 - GAP);
    }

    #[test]
    fn the_rows_carry_their_actions() {
        let model = model();
        let mut actions = Vec::new();
        let mut harness = crate::test_util::harness()
            .with_size([WIDTH, height(&model)])
            .build_ui(|ui| contents(ui, &model, &mut actions));
        harness.step();
        harness
            .get_by_role_and_label(egui::accesskit::Role::Switch, "Work")
            .click();
        harness.step();
        harness.get_by_label("Retry").click();
        harness.step();
        harness.get_by_label("Stop API").click();
        harness.step();
        harness.get_by_label("Quit").click();
        harness.step();
        harness.get_by_label("A host key waits for you");
        drop(harness);
        assert_eq!(
            actions,
            [
                TrayAction::StopGroup("g-1".into()),
                TrayAction::StartGroup("g-1".into()),
                TrayAction::StopTunnel("t-a".into()),
                TrayAction::Quit,
            ]
        );
    }
}
