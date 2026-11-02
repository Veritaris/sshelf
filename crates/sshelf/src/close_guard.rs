//! Closing the window while tunnels run (SPEC.md §10): the close is held
//! back and a modal asks, unless the user chose to always stop and quit.
//! Cmd+Q on macOS arrives as the same close request. "Minimize to tray"
//! joins in M6.

use egui::{RichText, ViewportCommand};
use sshelf_core::core::Command;
use sshelf_core::settings::{CloseBehavior, Settings};
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::theme::Palette;

#[derive(Default)]
pub struct CloseGuard {
    /// The modal is open.
    asking: bool,
    /// "Remember my choice".
    remember: bool,
    /// The user chose to quit; let the next close request through.
    confirmed: bool,
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

/// Whether a close request has to ask first.
pub fn should_ask(running: usize, behavior: CloseBehavior) -> bool {
    running > 0 && behavior == CloseBehavior::Ask
}

impl CloseGuard {
    /// Call once per frame.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        running: usize,
        settings: Option<&Settings>,
    ) -> Vec<Command> {
        let behavior = settings.map_or(CloseBehavior::Ask, |s| s.close_behavior);
        let requested = ctx.input(|i| i.viewport().close_requested());
        if requested && !self.confirmed && should_ask(running, behavior) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.asking = true;
        }
        if self.asking {
            self.modal(ctx, running, settings)
        } else {
            Vec::new()
        }
    }

    fn modal(
        &mut self,
        ctx: &egui::Context,
        running: usize,
        settings: Option<&Settings>,
    ) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let mut quit = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("close_with_tunnels")).show(ctx, |ui| {
            ui.set_max_width(440.0);
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
                let label = format!(
                    "Stop {running} tunnel{} and quit",
                    if running == 1 { "" } else { "s" }
                );
                quit = ui.button(label).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if quit {
            if self.remember
                && let Some(settings) = settings
            {
                commands.push(Command::SaveSettings(Settings {
                    close_behavior: CloseBehavior::StopAndQuit,
                    ..settings.clone()
                }));
            }
            self.asking = false;
            self.confirmed = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
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
    fn asks_only_with_running_tunnels_and_no_remembered_choice() {
        assert!(should_ask(2, CloseBehavior::Ask));
        assert!(!should_ask(0, CloseBehavior::Ask));
        assert!(!should_ask(2, CloseBehavior::StopAndQuit));
    }

    fn harness(settings: Settings) -> Harness<'static, (CloseGuard, Vec<Command>)> {
        let guard = CloseGuard {
            asking: true,
            ..CloseGuard::default()
        };
        Harness::builder().with_size([800.0, 600.0]).build_ui_state(
            move |ui, (guard, commands): &mut (CloseGuard, Vec<Command>)| {
                commands.extend(guard.update(ui.ctx(), 2, Some(&settings)));
            },
            (guard, Vec::new()),
        )
    }

    #[test]
    fn quitting_can_remember_the_choice() {
        let mut harness = harness(Settings::default());
        harness.step();
        harness.get_by_label("2 tunnels are running and will be stopped.");
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
    fn cancel_keeps_running_and_saves_nothing() {
        let mut harness = harness(Settings::default());
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
