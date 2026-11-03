//! Closing the window while tunnels run (SPEC.md §10): the close is held
//! back and a modal asks — *Minimize to tray* (when there is a tray), *Stop
//! and quit* or *Cancel* — unless a quitting choice was remembered. Cmd+Q on
//! macOS arrives as the same close request; Quit from the tray too, except
//! that it never just minimizes.
//!
//! [`CloseGuard::on_close_request`] runs from `App::logic`, so it also works
//! while the window is hidden; the modal is drawn from `App::ui`.

use egui::{RichText, ViewportCommand};
use sshelf_core::core::Command;
use sshelf_core::settings::{CloseBehavior, Settings};
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::theme::Palette;

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

    /// Call from `App::ui`: the modal, while asking.
    pub fn modal(
        &mut self,
        ctx: &egui::Context,
        running: usize,
        settings: Option<&Settings>,
    ) -> Vec<Command> {
        if !self.asking {
            return Vec::new();
        }
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let mut choice = None;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("close_with_tunnels")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading(RichText::new("Quit sshelf?").color(p.accent).strong());
            ui.label(format!(
                "{running} tunnel{} running and will be stopped.",
                if running == 1 { " is" } else { "s are" }
            ));
            ui.checkbox(&mut self.remember, "Remember my choice");
            ui.label(
                RichText::new("You can change this later in Settings.")
                    .small()
                    .color(p.dim),
            );
            ui.horizontal(|ui| {
                if self.offer_tray
                    && ui
                        .button("Minimize to tray")
                        .on_hover_text("Hide the window; the tunnels keep running")
                        .clicked()
                {
                    choice = Some(CloseBehavior::MinimizeToTray);
                }
                let label = format!(
                    "Stop {running} tunnel{} and quit",
                    if running == 1 { "" } else { "s" }
                );
                if ui.button(label).clicked() {
                    choice = Some(CloseBehavior::StopAndQuit);
                }
                cancel = ui.button("Cancel").clicked();
            });
        });
        if let Some(behavior) = choice {
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
        } else if cancel || response.should_close() {
            self.asking = false;
            self.remember = false;
        }
        commands
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

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
        Harness::builder().with_size([800.0, 600.0]).build_ui_state(
            move |ui, (guard, commands): &mut (CloseGuard, Vec<Command>)| {
                commands.extend(guard.modal(ui.ctx(), 2, Some(&settings)));
            },
            (guard, Vec::new()),
        )
    }

    #[test]
    fn quitting_can_remember_the_choice() {
        let mut harness = harness(Settings::default(), false);
        harness.step();
        harness.get_by_label("2 tunnels are running and will be stopped.");
        assert!(harness.query_by_label("Minimize to tray").is_none());
        harness.get_by_label("Remember my choice").click();
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
        harness.get_by_label("Remember my choice").click();
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
        harness.get_by_label("Remember my choice").click();
        harness.step();
        harness.get_by_label("Cancel").click();
        harness.step();
        let (guard, commands) = harness.state();
        assert!(!guard.asking && !guard.confirmed && !guard.remember);
        assert!(commands.is_empty());
    }
}
