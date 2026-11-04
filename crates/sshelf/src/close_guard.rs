//! Closing the window while tunnels run (SPEC.md §10): the close is held
//! back and a modal asks — *Minimize to tray* (when there is a tray), *Stop
//! and quit* or *Cancel* — unless a quitting choice was remembered. Cmd+Q on
//! macOS arrives as the same close request; Quit from the tray too, except
//! that it never just minimizes.
//!
//! [`CloseGuard::on_close_request`] runs from `App::logic`, so it also works
//! while the window is hidden; the modal is drawn from `App::ui`.

use egui::{Align, FontId, Frame, Layout, Margin, RichText, Stroke, Ui, ViewportCommand, vec2};
use sshelf_core::core::{AppData, Command};
use sshelf_core::settings::{CloseBehavior, Settings};
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::fonts;
use crate::theme::{Palette, radius};
use crate::tour::targets::{self, Target};
use crate::widgets::button::Button;
use crate::widgets::marks;
use crate::widgets::switch::switch;

#[derive(Default)]
pub struct CloseGuard {
    /// The modal is open.
    asking: bool,
    /// The modal offers *Minimize to tray*.
    offer_tray: bool,
    /// "Remember my choice".
    remember: bool,
    /// The user chose to quit; let the next close request through.
    confirmed: bool,
    /// The pending close request is Quit from the tray.
    from_tray: bool,
    /// The tour runs: a choice is only reported ([`Self::take_tour_choice`]).
    pub tour: bool,
    tour_choice: Option<CloseBehavior>,
    /// The dialog opened this frame.
    fresh: bool,
}

/// Tunnels that would be stopped by quitting.
pub fn running_count(states: Option<&Snapshot>) -> usize {
    states.map_or(0, |s| {
        s.tunnels
            .values()
            .filter(|t| matches!(t.state, TunnelState::Starting | TunnelState::Running))
            .count()
    })
}

/// The tunnels quitting would stop, for the modal: name and first listen
/// port (":6443"), in the order of `tunnels.toml`.
pub fn running_tunnels(states: Option<&Snapshot>, data: Option<&AppData>) -> Vec<(String, String)> {
    let (Some(states), Some(data)) = (states, data) else {
        return Vec::new();
    };
    data.tunnels
        .tunnels
        .iter()
        .filter(|t| {
            matches!(
                states.state(&t.id),
                TunnelState::Starting | TunnelState::Running
            )
        })
        .map(|t| {
            let port = t
                .forwards
                .first()
                .map(|f| format!(":{}", f.listen))
                .unwrap_or_default();
            (t.name.clone(), port)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Quit,
    Ask,
    /// Hide the window; the tunnels keep running.
    Hide,
}

/// What a close request does. `tray` is whether minimizing is possible.
/// A remembered *Minimize to tray* keeps sshelf in the tray whether or not
/// tunnels run; otherwise closing without running tunnels quits.
pub fn decide(running: usize, behavior: CloseBehavior, tray: bool) -> Decision {
    match behavior {
        CloseBehavior::MinimizeToTray if tray => Decision::Hide,
        _ if running == 0 => Decision::Quit,
        CloseBehavior::StopAndQuit => Decision::Quit,
        CloseBehavior::MinimizeToTray | CloseBehavior::Ask => Decision::Ask,
    }
}

/// Hides the window; the app keeps running in the tray.
pub fn hide(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
}

/// Shows the window (from the tray, or to ask).
pub fn show(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
}

impl CloseGuard {
    /// The dialog is open.
    pub fn is_asking(&self) -> bool {
        self.asking
    }

    /// Opens the dialog (the tour entering its step), offering the tray.
    pub fn ask(&mut self) {
        if !self.asking {
            // Opened by a click whose release is in this frame: that
            // release is not a click on the backdrop.
            self.fresh = true;
        }
        self.asking = true;
        self.offer_tray = true;
    }

    /// Closes the dialog as *Cancel* would.
    pub fn dismiss(&mut self) {
        self.asking = false;
        self.remember = false;
    }

    /// What was chosen in the dialog during the tour, once.
    pub fn take_tour_choice(&mut self) -> Option<CloseBehavior> {
        self.tour_choice.take()
    }

    /// Quit from the tray: the same close request as the window's.
    pub fn quit_from_tray(&mut self, ctx: &egui::Context) {
        self.from_tray = true;
        ctx.send_viewport_cmd(ViewportCommand::Close);
        ctx.request_repaint();
    }

    /// Call from `App::logic`: holds a close request back when needed.
    pub fn on_close_request(
        &mut self,
        ctx: &egui::Context,
        running: usize,
        settings: Option<&Settings>,
        tray: bool,
    ) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let from_tray = std::mem::take(&mut self.from_tray);
        if self.confirmed {
            return;
        }
        let behavior = settings.map_or(CloseBehavior::Ask, |s| s.close_behavior);
        // From the tray, "minimize" would just undo the Quit.
        let can_hide = tray && !from_tray;
        match decide(running, behavior, can_hide) {
            Decision::Quit => {}
            Decision::Hide => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                hide(ctx);
            }
            Decision::Ask => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.asking = true;
                self.offer_tray = can_hide;
                show(ctx);
            }
        }
    }

    /// Call from `App::ui`: the modal, while asking. `tunnels` are the
    /// running ones with their ports ([`running_tunnels`]).
    pub fn modal(
        &mut self,
        ctx: &egui::Context,
        tunnels: &[(String, String)],
        settings: Option<&Settings>,
    ) -> Vec<Command> {
        if !self.asking {
            return Vec::new();
        }
        let p = &Palette::ONE_DARK;
        let running = tunnels.len();
        let fresh = std::mem::take(&mut self.fresh);
        let plural = if running == 1 { "" } else { "s" };
        let mut commands = Vec::new();
        let mut choice = None;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("close_with_tunnels"))
            .backdrop_color(p.overlay)
            .frame(
                Frame::new()
                    .fill(p.bg_dark)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::LARGE)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(452.0);
                ui.spacing_mut().item_spacing.y = 16.0;
                ui.label(
                    RichText::new(format!(
                        "{running} tunnel{plural} {} running",
                        if running == 1 { "is" } else { "are" }
                    ))
                    .font(fonts::semibold(17.0))
                    .color(p.text_strong),
                );
                tunnel_list(ui, tunnels);
                ui.label(
                    RichText::new(if self.offer_tray {
                        "Quitting stops them. In the tray they keep running."
                    } else {
                        "Quitting stops them."
                    })
                    .size(13.0)
                    .color(p.dim),
                );
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    switch(ui, &mut self.remember, "Remember my choice");
                    ui.label(RichText::new("Remember my choice").size(13.0));
                    ui.label(
                        RichText::new("(change it in Settings, under Window & tray)")
                            .size(12.0)
                            .color(p.dim),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // Right to left: the main button ends up rightmost.
                    if self.offer_tray {
                        let minimize = ui.add(
                            Button::new("Minimize to tray")
                                .primary()
                                .dialog()
                                .tip("Hide the window; the tunnels keep running"),
                        );
                        if targets::clicked(ui.ctx(), Target::CloseMinimize, &minimize) {
                            choice = Some(CloseBehavior::MinimizeToTray);
                        }
                    }
                    let mut stop =
                        Button::new(format!("Stop {running} tunnel{plural} and quit")).dialog();
                    if !self.offer_tray {
                        stop = stop.primary();
                    }
                    if ui.add(stop).clicked() {
                        choice = Some(CloseBehavior::StopAndQuit);
                    }
                    cancel = ui.add(Button::new("Cancel").ghost().dialog()).clicked();
                });
            });
        targets::register(ctx, Target::CloseDialog, response.response.rect);
        if let Some(behavior) = choice
            && self.tour
        {
            // The tour shows what would happen instead (SPEC.md §10).
            self.asking = false;
            self.remember = false;
            self.tour_choice = Some(behavior);
        } else if let Some(behavior) = choice {
            if self.remember
                && let Some(settings) = settings
            {
                commands.push(Command::SaveSettings(Settings {
                    close_behavior: behavior,
                    ..settings.clone()
                }));
            }
            self.asking = false;
            self.remember = false;
            if behavior == CloseBehavior::StopAndQuit {
                self.confirmed = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                hide(ctx);
            }
        // In the tour the dialog is a step: only its buttons close it.
        } else if cancel || (response.should_close() && !fresh && !self.tour) {
            self.asking = false;
            self.remember = false;
        }
        commands
    }
}

/// The running tunnels on an inset card: a green dot, the name, the port;
/// past six, "+N more".
fn tunnel_list(ui: &mut Ui, tunnels: &[(String, String)]) {
    const SHOWN: usize = 6;
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(p.bg)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(14, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for (name, port) in tunnels.iter().take(SHOWN) {
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 30.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        marks::dot(ui, p.green);
                        ui.label(RichText::new(name).size(13.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(port)
                                    .font(FontId::monospace(11.0))
                                    .color(p.dim),
                            );
                        });
                    },
                );
            }
            if tunnels.len() > SHOWN {
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 30.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(format!("+{} more", tunnels.len() - SHOWN))
                                .size(12.0)
                                .color(p.dim),
                        );
                    },
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_util;

    #[test]
    fn decisions() {
        use CloseBehavior::{MinimizeToTray, StopAndQuit};
        for tray in [false, true] {
            for behavior in [CloseBehavior::Ask, StopAndQuit] {
                assert_eq!(decide(0, behavior, tray), Decision::Quit, "nothing running");
            }
            assert_eq!(decide(2, CloseBehavior::Ask, tray), Decision::Ask);
            assert_eq!(decide(2, StopAndQuit, tray), Decision::Quit);
        }
        // Minimize to tray hides, tunnels or not.
        assert_eq!(decide(0, MinimizeToTray, true), Decision::Hide);
        assert_eq!(decide(2, MinimizeToTray, true), Decision::Hide);
        // Remembered, but no tray (now, or for a Quit from the tray): quit
        // when nothing runs, else ask.
        assert_eq!(decide(0, MinimizeToTray, false), Decision::Quit);
        assert_eq!(decide(2, MinimizeToTray, false), Decision::Ask);
    }

    fn harness(
        settings: Settings,
        offer_tray: bool,
    ) -> Harness<'static, (CloseGuard, Vec<Command>)> {
        let guard = CloseGuard {
            asking: true,
            offer_tray,
            ..CloseGuard::default()
        };
        test_util::harness()
            .with_size([800.0, 600.0])
            .build_ui_state(
                move |ui, (guard, commands): &mut (CloseGuard, Vec<Command>)| {
                    let tunnels = [
                        ("dev cluster".to_string(), ":6443".to_string()),
                        ("vault".to_string(), ":8200".to_string()),
                    ];
                    commands.extend(guard.modal(ui.ctx(), &tunnels, Some(&settings)));
                },
                (guard, Vec::new()),
            )
    }

    #[test]
    fn quitting_can_remember_the_choice() {
        let mut harness = harness(Settings::default(), false);
        harness.step();
        harness.get_by_label("2 tunnels are running");
        harness.get_by_label("dev cluster");
        harness.get_by_label(":8200");
        harness.get_by_label("Quitting stops them.");
        assert!(harness.query_by_label("Minimize to tray").is_none());
        harness
            .get_by_role_and_label(egui::accesskit::Role::Switch, "Remember my choice")
            .click();
        harness.step();
        harness.get_by_label("Stop 2 tunnels and quit").click();
        harness.step();
        let (guard, commands) = harness.state();
        assert!(guard.confirmed && !guard.asking);
        assert_eq!(
            commands,
            &[Command::SaveSettings(Settings {
                close_behavior: CloseBehavior::StopAndQuit,
                ..Settings::default()
            })]
        );
    }

    #[test]
    fn minimizing_keeps_running_and_can_be_remembered() {
        let mut harness = harness(Settings::default(), true);
        harness.step();
        harness.get_by_label("Quitting stops them. In the tray they keep running.");
        harness
            .get_by_role_and_label(egui::accesskit::Role::Switch, "Remember my choice")
            .click();
        harness.step();
        harness.get_by_label("Minimize to tray").click();
        harness.step();
        let (guard, commands) = harness.state();
        assert!(!guard.asking && !guard.confirmed, "nothing quits");
        assert_eq!(
            commands,
            &[Command::SaveSettings(Settings {
                close_behavior: CloseBehavior::MinimizeToTray,
                ..Settings::default()
            })]
        );
    }

    #[test]
    fn cancel_keeps_running_and_saves_nothing() {
        let mut harness = harness(Settings::default(), true);
        harness.step();
        harness
            .get_by_role_and_label(egui::accesskit::Role::Switch, "Remember my choice")
            .click();
        harness.step();
        harness.get_by_label("Cancel").click();
        harness.step();
        let (guard, commands) = harness.state();
        assert!(!guard.asking && !guard.confirmed && !guard.remember);
        assert!(commands.is_empty());
    }
}
