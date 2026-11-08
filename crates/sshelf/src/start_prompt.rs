//! The start dialogs (SPEC.md §10 *Group order and dependencies*, M15):
//! starting a tunnel alone whose dependencies aren't running, or a group
//! whose tunnels depend on ones outside it. It lists what would start too,
//! offers "Don't ask again", and answers core's [`StartPrompt`].

use std::sync::Arc;

use egui::{Align, Id, Layout, Margin, RichText, Stroke, Ui};
use sshelf_core::core::{AppData, Command, StartPrompt, StartTarget};
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::theme::{Palette, radius};
use crate::tunnels_view::{self, mark, tunnel_name};
use crate::widgets::button::Button;
use crate::widgets::marks;
use crate::widgets::switch::switch;
use crate::{fonts, icons, privacy};

#[derive(Default)]
pub struct StartPrompts {
    prompt: Option<Arc<StartPrompt>>,
    dont_ask: bool,
}

/// "a tunnel that isn’t" / "2 tunnels that aren’t".
fn tunnels(n: usize) -> String {
    if n == 1 {
        "a tunnel that isn\u{2019}t".into()
    } else {
        format!("{n} tunnels that aren\u{2019}t")
    }
}

impl StartPrompts {
    pub fn on_prompt(&mut self, prompt: Arc<StartPrompt>) {
        self.prompt = Some(prompt);
        self.dont_ask = false;
    }

    pub fn show(&mut self, ctx: &egui::Context, data: &AppData, states: &Snapshot) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some(prompt) = self.prompt.clone() else {
            return commands;
        };
        let p = &Palette::ONE_DARK;
        let file = &data.tunnels;
        let n = prompt.deps.len();
        let (one, many) = (n == 1, n > 1);
        let (subject, title, body, start) = match &prompt.target {
            StartTarget::Tunnel(id) => {
                let name = tunnel_name(file, id);
                let (it, them) = if one { ("it", "it") } else { ("them", "them") };
                (
                    name.clone(),
                    format!("Start \u{201c}{name}\u{201d}?"),
                    format!(
                        "It depends on {} running. sshelf starts {it} first, then \
                         \u{201c}{name}\u{201d}, and stops {them} again when nothing needs {them}.",
                        tunnels(n)
                    ),
                    format!("Start {} tunnels", n + 1),
                )
            }
            StartTarget::Group(id) => {
                let group = file.group(id);
                let name = group.map_or_else(|| id.clone(), |g| g.name.clone());
                let needer = group
                    .and_then(|g| {
                        g.tunnels.iter().find(|t| {
                            file.tunnel(t).is_some_and(|t| {
                                t.depends_on.iter().any(|d| prompt.deps.contains(d))
                            })
                        })
                    })
                    .map(|t| tunnel_name(file, t))
                    .unwrap_or_default();
                let (what, it, uses) = if many {
                    ("tunnels that aren\u{2019}t", "them", "them")
                } else {
                    ("a tunnel that isn\u{2019}t", "it", "it")
                };
                (
                    name.clone(),
                    format!("Start \u{201c}{name}\u{201d}?"),
                    format!(
                        "\u{201c}{needer}\u{201d} depends on {what} in this group. sshelf starts \
                         {it} too, and stops {it} when {name} stops (unless something else uses \
                         {uses})."
                    ),
                    format!("Start group and {n} more"),
                )
            }
        };
        let mut answer = None;
        let response = egui::Modal::new(Id::unique(("start_prompt", prompt.id)))
            .backdrop_color(p.overlay)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_dark)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::LARGE)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(452.0);
                ui.spacing_mut().item_spacing.y = 16.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    marks::tile(ui, icons::CORNER_DOWN_RIGHT, p.blue_tint, p.blue);
                    ui.label(
                        RichText::new(&title)
                            .font(fonts::semibold(17.0))
                            .color(p.text_strong),
                    );
                });
                ui.add(egui::Label::new(RichText::new(&body).size(13.0)).wrap());
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for id in &prompt.deps {
                        dep_row(ui, id, &prompt.target, data, states);
                    }
                });
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let label = format!("Don\u{2019}t ask again for \u{201c}{subject}\u{201d}");
                        switch(ui, &mut self.dont_ask, &label);
                        ui.label(RichText::new(&label).size(13.0));
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(46.0);
                        ui.label(
                            RichText::new("Asks again if its dependencies change.")
                                .size(12.0)
                                .color(p.dim),
                        );
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if ui.add(Button::new(&start).primary().dialog()).clicked() {
                        answer = Some(true);
                    }
                    let cancel = ui.add(Button::new("Cancel").ghost().dialog());
                    if ui.memory(|m| m.focused().is_none()) {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        answer = Some(false);
                    }
                });
            });
        if response.should_close() && answer.is_none() {
            answer = Some(false);
        }
        match answer {
            Some(true) => commands.push(Command::ConfirmStart {
                id: prompt.id,
                dont_ask: self.dont_ask,
            }),
            Some(false) => commands.push(Command::CancelStart(prompt.id)),
            None => {}
        }
        if answer.is_some() {
            self.prompt = None;
        }
        commands
    }
}

/// One tunnel that would start too: its marks, name over where it goes
/// (or, for a group, who needs it), and its state.
fn dep_row(ui: &mut Ui, id: &str, target: &StartTarget, data: &AppData, states: &Snapshot) {
    let p = &Palette::ONE_DARK;
    let file = &data.tunnels;
    let Some(tunnel) = file.tunnel(id) else {
        return;
    };
    let state = states.state(id);
    egui::Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                mark(ui, &state);
                marks::type_mark(ui, tunnel.kind, None);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&tunnel.name).size(13.0).color(p.text_strong));
                    let line = match target {
                        StartTarget::Group(_) => {
                            let needers: Vec<String> = file
                                .tunnels
                                .iter()
                                .filter(|t| t.depends_on.iter().any(|d| d == id))
                                .map(|t| t.name.clone())
                                .collect();
                            format!("needed by {}", needers.join(", "))
                        }
                        StartTarget::Tunnel(_) => {
                            let privacy = privacy::get(ui.ctx());
                            let place = if tunnel.is_kubectl() {
                                tunnels_view::kube_place(tunnel, privacy.on)
                            } else {
                                privacy.alias_text(&tunnel.host).into_owned()
                            };
                            match tunnel.forwards.first() {
                                Some(f) if !privacy.on => {
                                    format!("{place} \u{b7} {:?} {}:{}", f.kind, f.bind, f.listen)
                                }
                                _ => place,
                            }
                        }
                    };
                    ui.label(
                        RichText::new(line)
                            .font(egui::FontId::monospace(11.0))
                            .color(p.dim),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (text, color) = match state {
                        TunnelState::Failed(_) => ("Failed \u{b7} retried", p.red),
                        TunnelState::Starting => ("Starting\u{2026}", p.yellow),
                        TunnelState::Stopping => ("Stopping\u{2026}", p.dim),
                        _ => ("Stopped", p.dim),
                    };
                    ui.label(RichText::new(text).size(12.0).color(color));
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sshelf_core::tunnels::{Group, Tunnel};

    use super::*;
    use crate::test_util;

    /// Mailpit depends on k3s; Dev holds only Mailpit.
    fn data() -> AppData {
        let mut data = AppData::new(PathBuf::from("app"));
        data.tunnels
            .upsert_tunnel(Tunnel::ssh("t-k", "k3s", "h", Vec::new()));
        let mut mailpit = Tunnel::ssh("t-m", "mailpit", "h", Vec::new());
        mailpit.depends_on = vec!["t-k".into()];
        data.tunnels.upsert_tunnel(mailpit);
        data.tunnels.upsert_group(Group {
            id: "g-1".into(),
            name: "Dev".into(),
            tunnels: vec!["t-m".into()],
            start_on_launch: false,
            start_deps_without_asking: false,
        });
        data
    }

    fn harness(target: StartTarget) -> Harness<'static, (StartPrompts, Vec<Command>)> {
        let data = data();
        let mut prompts = StartPrompts::default();
        prompts.on_prompt(Arc::new(StartPrompt {
            id: 7,
            target,
            deps: vec!["t-k".into()],
        }));
        let mut harness = test_util::harness()
            .with_size([1000.0, 800.0])
            .build_ui_state(
                move |ui, (prompts, commands): &mut (StartPrompts, Vec<Command>)| {
                    commands.extend(prompts.show(ui.ctx(), &data, &Snapshot::default()));
                },
                (prompts, Vec::new()),
            );
        harness.run_steps(3);
        harness
    }

    #[test]
    fn starting_alone_lists_the_dependency_and_remembers_dont_ask() {
        let mut harness = harness(StartTarget::Tunnel("t-m".into()));
        let drawn = test_util::drawn_text(&harness);
        assert!(drawn.contains("Start \u{201c}mailpit\u{201d}?"), "{drawn}");
        assert!(drawn.contains("k3s"), "{drawn}");
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::Switch,
                "Don\u{2019}t ask again for \u{201c}mailpit\u{201d}",
            )
            .click();
        harness.run_steps(2);
        harness.get_by_label("Start 2 tunnels").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().1,
            [Command::ConfirmStart {
                id: 7,
                dont_ask: true
            }]
        );
        assert!(!test_util::drawn_text(&harness).contains("Start 2 tunnels"));
    }

    #[test]
    fn a_group_says_who_needs_it_and_cancel_answers_no() {
        let mut harness = harness(StartTarget::Group("g-1".into()));
        let drawn = test_util::drawn_text(&harness);
        assert!(drawn.contains("needed by mailpit"), "{drawn}");
        harness.get_by_label("Start group and 1 more");
        harness.get_by_label("Cancel").click();
        harness.run_steps(2);
        assert_eq!(harness.state().1, [Command::CancelStart(7)]);
    }

    #[test]
    fn confirming_without_the_switch_asks_again_next_time() {
        let mut harness = harness(StartTarget::Group("g-1".into()));
        harness.get_by_label("Start group and 1 more").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().1,
            [Command::ConfirmStart {
                id: 7,
                dont_ask: false
            }]
        );
    }
}
