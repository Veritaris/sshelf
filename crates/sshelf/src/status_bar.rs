//! The status bar (SPEC.md §10 *Layout*): always shown, 28 px, along the
//! bottom of the window: running and failed tunnels, the ssh version, the
//! agent state and when the config was last read. An error takes its place,
//! tinted red, until dismissed.

use std::time::SystemTime;

use egui::{Align, CornerRadius, Label, Layout, RichText, Ui, UiBuilder, vec2};
use sshelf_core::agent_service::AgentService;
use sshelf_core::core::SshInfo;
use sshelf_core::ssh::AgentStatus;
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::icons;
use crate::theme::Palette;
use crate::tour::targets::{self, Target};
use crate::widgets::button::Button;
use crate::widgets::marks;
use crate::window;

/// Height of the bar.
pub const HEIGHT: f32 = 28.0;
/// Height while it shows an error: room for the square *Dismiss* button.
const ERROR_HEIGHT: f32 = 36.0;

/// The bar's height, taller with an error.
pub fn height(error: bool) -> f32 {
    if error { ERROR_HEIGHT } else { HEIGHT }
}

/// What the bar reports.
pub struct Status<'a> {
    pub running: usize,
    pub failed: usize,
    pub ssh: Option<&'a SshInfo>,
    /// When the last config snapshot arrived.
    pub config_loaded: Option<SystemTime>,
}

/// Tunnels running now and tunnels that failed.
pub fn tunnel_counts(states: Option<&Snapshot>) -> (usize, usize) {
    let Some(states) = states else {
        return (0, 0);
    };
    let count =
        |f: fn(&TunnelState) -> bool| states.tunnels.values().filter(|t| f(&t.state)).count();
    (
        count(|s| *s == TunnelState::Running),
        count(|s| matches!(s, TunnelState::Failed(_))),
    )
}

fn small(text: impl Into<String>, color: egui::Color32) -> RichText {
    RichText::new(text).size(12.0).color(color)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The agent's state, when there is something to say about it.
fn agent(ssh: &SshInfo) -> Option<(String, egui::Color32)> {
    let p = &Palette::ONE_DARK;
    if let Some(Ok(AgentService::Installed { running: false, .. })) = &ssh.service {
        return Some(("agent service stopped".into(), p.yellow));
    }
    match &ssh.agent {
        Some(AgentStatus::Keys(n)) => {
            Some((format!("{} in agent", plural(*n, "key", "keys")), p.dim))
        }
        Some(AgentStatus::Unreachable(_)) => Some(("agent not reachable".into(), p.yellow)),
        Some(AgentStatus::Error(e)) => Some((format!("agent: {e}"), p.yellow)),
        None => None,
    }
}

/// Draws the bar over the whole of `ui` (its panel).
pub fn show(ui: &mut Ui, status: &Status, error: &mut Option<String>) {
    let p = &Palette::ONE_DARK;
    let rect = ui.max_rect();
    targets::register(ui.ctx(), Target::StatusBar, rect);
    // The window's bottom corners.
    let corner = window::corner(ui.ctx());
    let corners = CornerRadius {
        sw: corner,
        se: corner,
        ..CornerRadius::ZERO
    };
    let fill = if error.is_some() {
        p.red_tint
    } else {
        p.bg_dark
    };
    ui.painter().rect_filled(rect, corners, fill);
    // With an error, the square button sits 4 px from the right edge.
    let inner = if error.is_some() {
        rect.shrink2(vec2(16.0, 0.0)).with_max_x(rect.right() - 4.0)
    } else {
        rect.shrink2(vec2(16.0, 0.0))
    };
    let layout = Layout::left_to_right(Align::Center);
    ui.scope_builder(UiBuilder::new().max_rect(inner).layout(layout), |ui| {
        if let Some(text) = error.clone() {
            ui.spacing_mut().item_spacing.x = 8.0;
            icons::show(ui, icons::CIRCLE_X, icons::INLINE, p.red);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dismiss = Button::icon_only(icons::X, "Dismiss").small().ghost();
                if ui.add(dismiss).clicked() {
                    *error = None;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(Label::new(small(text, p.text_strong)).truncate());
                });
            });
            return;
        }
        // The right-hand time first, so the left items get what remains
        // (and truncate) instead of running into it.
        if let Some(loaded) = status.config_loaded {
            let time = chrono::DateTime::<chrono::Local>::from(loaded).format("%H:%M");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(small(format!("config reloaded {time}"), p.dim));
                ui.add_space(16.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    left_items(ui, status);
                });
            });
        } else {
            left_items(ui, status);
        }
    });
}

fn left_items(ui: &mut Ui, status: &Status) {
    let p = &Palette::ONE_DARK;
    ui.spacing_mut().item_spacing.x = 16.0;
    let count = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (dot, text) = if status.running == 0 {
            (p.comment, "no tunnels running".to_string())
        } else {
            (
                p.green,
                format!("{} running", plural(status.running, "tunnel", "tunnels")),
            )
        };
        marks::dot(ui, dot);
        ui.label(small(text, p.dim));
    });
    targets::register(
        ui.ctx(),
        Target::StatusRunning,
        count.response.rect.expand(4.0),
    );
    if status.failed > 0 {
        ui.label(small(format!("{} failed", status.failed), p.red));
    }
    if let Some(ssh) = status.ssh {
        if let Some(Ok(version)) = &ssh.version {
            // "OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2": the first part.
            let version = version.split(',').next().unwrap_or_default().trim();
            ui.add(Label::new(small(version, p.dim).monospace()).truncate());
        }
        if let Some((text, color)) = agent(ssh) {
            ui.add(Label::new(small(text, color)).truncate());
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_util;

    fn ssh() -> SshInfo {
        SshInfo {
            tools: Err("not needed".into()),
            version: Some(Ok("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2".into())),
            agent: Some(AgentStatus::Keys(2)),
            service: Some(Ok(AgentService::Installed {
                running: false,
                start_type: sshelf_core::agent_service::StartType::Manual,
            })),
            terminal: None,
            tray: Ok(true),
        }
    }

    fn harness(
        running: usize,
        failed: usize,
        error: Option<String>,
    ) -> egui_kittest::Harness<'static, Option<String>> {
        let ssh = ssh();
        test_util::harness()
            .with_size([900.0, 28.0])
            .build_ui_state(
                move |ui, error: &mut Option<String>| {
                    let status = Status {
                        running,
                        failed,
                        ssh: Some(&ssh),
                        config_loaded: Some(SystemTime::now()),
                    };
                    show(ui, &status, error);
                },
                error,
            )
    }

    #[test]
    fn reports_tunnels_ssh_and_agent() {
        let mut harness = harness(1, 2, None);
        harness.run();
        harness.get_by_label("1 tunnel running");
        harness.get_by_label("2 failed");
        harness.get_by_label("OpenSSH_for_Windows_9.5p1");
        // A stopped service says more than the key count.
        harness.get_by_label("agent service stopped");
        harness.get_by_label_contains("config reloaded ");
    }

    #[test]
    fn nothing_running_and_no_failures() {
        let mut harness = harness(0, 0, None);
        harness.run();
        harness.get_by_label("no tunnels running");
        assert!(harness.query_by_label_contains("failed").is_none());
    }

    #[test]
    fn an_error_replaces_the_bar_until_dismissed() {
        let mut harness = harness(1, 0, Some("Couldn't open a terminal".into()));
        harness.run();
        harness.get_by_label("Couldn't open a terminal");
        assert!(harness.query_by_label("1 tunnel running").is_none());
        harness.get_by_label("Dismiss").click();
        harness.run();
        assert_eq!(harness.state(), &None);
        harness.get_by_label("1 tunnel running");
    }
}
