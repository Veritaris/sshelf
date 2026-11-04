//! Host key modals (SPEC.md §9, §10; boards Dialog-HostKey,
//! Dialog-HostKey-Changed): confirm an unknown key, or replace a changed one
//! after typing the host's name. Prompts queue up; a changed key is shown
//! before unknown ones. The changed-key dialog looks different on purpose:
//! a Light Red outline and a red header band.

use std::collections::VecDeque;
use std::sync::Arc;

use egui::{Align, Color32, FontId, Frame, Id, Layout, Margin, RichText, Stroke, StrokeKind, Ui};
use sshelf_core::core::{ChangedKeyPrompt, Command, HostKeyPrompt};
use sshelf_core::hostkeys::KeyInfo;

use crate::theme::{Palette, radius};
use crate::widgets::button::{self, Button};
use crate::widgets::field::TextField;
use crate::widgets::{marks, masked};
use crate::{fonts, icons, privacy};

const COMPARE_HINT: &str = "ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub";

#[derive(Default)]
pub struct HostKeyModals {
    unknown: VecDeque<Arc<HostKeyPrompt>>,
    changed: VecDeque<Arc<ChangedKeyPrompt>>,
    /// What was typed to confirm the changed prompt on screen.
    typed: String,
    /// Toasts for the app ("Copied command").
    notices: Vec<String>,
}

impl HostKeyModals {
    pub fn on_unknown(&mut self, prompt: Arc<HostKeyPrompt>) {
        self.unknown.push_back(prompt);
    }

    pub fn on_changed(&mut self, prompt: Arc<ChangedKeyPrompt>) {
        self.changed.push_back(prompt);
    }

    pub fn on_closed(&mut self, id: u64) {
        self.unknown.retain(|p| p.id != id);
        let front = self.changed.front().map(|p| p.id);
        self.changed.retain(|p| p.id != id);
        if self.changed.front().map(|p| p.id) != front {
            self.typed.clear();
        }
    }

    /// A prompt waits for the user (the tray menu says so).
    pub fn waiting(&self) -> bool {
        !self.unknown.is_empty() || !self.changed.is_empty()
    }

    /// Toasts to show, once.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Vec<Command> {
        if let Some(prompt) = self.changed.front().cloned() {
            return self.changed_modal(ctx, &prompt);
        }
        if let Some(prompt) = self.unknown.front().cloned() {
            return self.unknown_modal(ctx, &prompt);
        }
        Vec::new()
    }

    fn unknown_modal(&mut self, ctx: &egui::Context, prompt: &HostKeyPrompt) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let unknown = &prompt.unknown;
        let target = &unknown.target;
        let mut commands = Vec::new();
        let mut copied = false;
        let response = egui::Modal::new(Id::unique(("hostkey_unknown", prompt.id)))
            .backdrop_color(p.overlay)
            .frame(dialog_frame(p.selection).inner_margin(Margin::same(24)))
            .show(ctx, |ui| {
                ui.set_width(552.0);
                ui.spacing_mut().item_spacing.y = 18.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    marks::tile(ui, icons::KEY_ROUND, p.blue_tint, p.blue);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let privacy = privacy::get(ui.ctx());
                        ui.label(
                            RichText::new(format!(
                                "First connection to {}",
                                privacy.host(&target.destination)
                            ))
                            .font(fonts::semibold(17.0))
                            .color(p.text_strong),
                        );
                        ui.label(
                            RichText::new(format!(
                                "sshelf hasn't seen this server's key before; {} waits for it.",
                                privacy.plain(&prompt.purpose)
                            ))
                            .size(13.0)
                            .color(p.dim),
                        );
                    });
                });
                if let Some(final_host) = &unknown.hop_of {
                    let privacy = privacy::get(ui.ctx());
                    let (hop, last) = (privacy.host(&target.destination), privacy.host(final_host));
                    jump_steps(ui, &hop, &last);
                }
                Frame::new()
                    .fill(p.bg)
                    .corner_radius(radius::CONTROL)
                    .inner_margin(Margin::symmetric(16, 14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 10.0;
                        let value = masked::Style {
                            font: FontId::monospace(13.0),
                            color: p.text,
                            on_card: false,
                        };
                        kv(ui, "Host", |ui| {
                            masked::value(ui, &target.hostname, &value);
                        });
                        kv(ui, "Port", |ui| {
                            masked::value(ui, &target.port.to_string(), &value);
                            if target.port != 22 {
                                let note = masked::Style {
                                    font: FontId::proportional(12.0),
                                    color: p.dim,
                                    on_card: false,
                                };
                                let text = format!("(stored as [host]:{})", target.port);
                                masked::text(ui, &text, &note, false);
                            }
                        });
                        for key in &unknown.keys {
                            kv(ui, "Key type", |ui| {
                                ui.label(mono(
                                    &format!("{} ({} bits)", key.kind, key.bits),
                                    p.text,
                                ));
                            });
                            kv(ui, "Fingerprint", |ui| {
                                fingerprint(ui, &key.fingerprint, p.text_strong);
                            });
                        }
                    });
                copied = compare_hint(
                    ui,
                    "Compare it with the server before accepting. On the server run",
                );
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let lines = unknown.lines.len();
                    let file = target
                        .user_files
                        .first()
                        .map(|f| f.display().to_string())
                        .unwrap_or_else(|| "known_hosts".into());
                    let note = format!(
                        "Accepting adds {lines} line{} to {file} (backed up first).",
                        if lines == 1 { "" } else { "s" }
                    );
                    let room = ui.available_width() - 300.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(room.max(100.0), 36.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let style = masked::Style {
                                font: FontId::proportional(12.0),
                                color: p.dim,
                                on_card: true,
                            };
                            masked::text(ui, &note, &style, true);
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(Button::new("Accept and continue").primary().dialog())
                            .clicked()
                        {
                            commands.push(Command::AcceptHostKey(prompt.id));
                        }
                        if ui.add(Button::new("Reject").ghost().dialog()).clicked() {
                            commands.push(Command::RejectHostKey(prompt.id));
                        }
                    });
                });
            });
        if copied {
            self.notices.push("Copied command".into());
        }
        if response.should_close() && commands.is_empty() {
            commands.push(Command::RejectHostKey(prompt.id));
        }
        commands
    }

    fn changed_modal(&mut self, ctx: &egui::Context, prompt: &ChangedKeyPrompt) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let changed = &prompt.changed;
        let target = &changed.target;
        let name = target.destination.clone();
        // What the dialog shows for it (typing the real name still works).
        let shown = privacy::get(ctx).host(&name);
        let mut commands = Vec::new();
        let mut copied = false;
        let response = egui::Modal::new(Id::unique(("hostkey_changed", prompt.id)))
            .backdrop_color(p.overlay)
            .frame(dialog_frame(p.red))
            .show(ctx, |ui| {
                ui.set_width(700.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                // The red header band.
                Frame::new()
                    .fill(p.red_tint)
                    .corner_radius(egui::CornerRadius {
                        nw: radius::LARGE,
                        ne: radius::LARGE,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(Margin::symmetric(24, 18))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            marks::tile(ui, icons::SHIELD_ALERT, p.red, p.bg_dark);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(format!("The host key of {shown} changed"))
                                        .font(fonts::semibold(17.0))
                                        .color(p.red),
                                );
                                ui.label(
                                    RichText::new(
                                        "The server was reinstalled — or someone is \
                                         intercepting this connection.",
                                    )
                                    .size(13.0),
                                );
                            });
                        });
                    });
                Frame::new()
                    .inner_margin(Margin::symmetric(24, 20))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 16.0;
                        let mut context = format!(
                            "{} presents a different key than the one stored for it ({}).",
                            target.display(),
                            prompt.subject
                        );
                        if let Some(final_host) = &changed.hop_of {
                            context.push_str(&format!(
                                " It is a jump host on the way to {final_host}."
                            ));
                        }
                        let style = masked::Style {
                            font: FontId::proportional(13.0),
                            color: p.text,
                            on_card: true,
                        };
                        masked::text(ui, &context, &style, true);
                        ui.columns(2, |columns| {
                            fingerprints(
                                &mut columns[0],
                                "Stored in known_hosts",
                                &changed.old,
                                false,
                            );
                            fingerprints(
                                &mut columns[1],
                                "Server presents now",
                                &changed.new,
                                true,
                            );
                        });
                        copied = compare_hint(
                            ui,
                            "Only replace it if you know the server was reinstalled. Ask \
                             your provider or check on the server:",
                        );
                        let replaceable = !changed.user_files.is_empty();
                        if replaceable {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                let label = ui.label(
                                    RichText::new(format!(
                                        "Type {shown} to replace the stored key"
                                    ))
                                    .size(13.0),
                                );
                                ui.add(TextField::new(&mut self.typed).mono().dialog(p.bg))
                                    .labelled_by(label.id);
                            });
                        } else {
                            marks::banner(ui, p.yellow_tint, |ui| {
                                ui.label(
                                    RichText::new(
                                        "The stored key is in the system-wide known_hosts; \
                                         an administrator has to replace it.",
                                    )
                                    .color(p.yellow),
                                );
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width() - 290.0, 36.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(
                                                "Replacing runs ssh-keygen -R (keeps .old) and \
                                                 stores the new key.",
                                            )
                                            .size(12.0)
                                            .color(p.dim),
                                        )
                                        .wrap(),
                                    );
                                },
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let cancel = ui.add(Button::new("Don't connect").dialog());
                                if ui.memory(|m| m.focused().is_none()) {
                                    cancel.request_focus();
                                }
                                if cancel.clicked() {
                                    commands.push(Command::CancelChangedKey(prompt.id));
                                }
                                // What the dialog asks for, or the real name.
                                let typed = self.typed.trim();
                                let confirmed = typed == name || typed == shown;
                                let tip = if !replaceable {
                                    "Only an administrator can replace a system-wide key"
                                } else {
                                    "Type the host's name first"
                                };
                                let mut replace = Button::new("Replace key").danger().dialog();
                                if !confirmed || !replaceable {
                                    replace = replace.tip(tip);
                                }
                                if ui.add_enabled(replaceable && confirmed, replace).clicked() {
                                    commands.push(Command::ReplaceHostKey(prompt.id));
                                }
                            });
                        });
                    });
            });
        if copied {
            self.notices.push("Copied command".into());
        }
        if response.should_close() && commands.is_empty() {
            commands.push(Command::CancelChangedKey(prompt.id));
        }
        commands
    }
}

/// The dialog frame: 16 px corners and a 1 px `outline`, no shadow.
fn dialog_frame(outline: Color32) -> Frame {
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(p.bg_dark)
        .stroke(Stroke::new(1.0, outline))
        .corner_radius(radius::LARGE)
}

/// A fingerprint in mono, wrapped anywhere (it has no spaces), never cut;
/// a token in streamer mode.
fn fingerprint(ui: &mut Ui, text: &str, color: Color32) {
    if privacy::get(ui.ctx()).on {
        let style = masked::Style {
            font: FontId::monospace(12.0),
            color,
            on_card: false,
        };
        masked::value(ui, text, &style);
        return;
    }
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(FontId::monospace(12.0), color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: ui.available_width(),
        break_anywhere: true,
        ..Default::default()
    };
    ui.label(job);
}

fn mono(text: &str, color: Color32) -> RichText {
    RichText::new(text)
        .font(FontId::monospace(12.0))
        .color(color)
}

/// A detail row: the dim label (120 px), then `add`.
fn kv(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    let p = &Palette::ONE_DARK;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 18.0), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            p.dim,
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            add(ui);
        });
    });
}

/// The jump chain: the host asked about now (Magenta step) on the way to
/// the final one; jump hosts are checked first.
fn jump_steps(ui: &mut Ui, hop: &str, final_host: &str) {
    let p = &Palette::ONE_DARK;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        step(ui, "1", hop, true);
        icons::show(ui, icons::CHEVRON_RIGHT, icons::INLINE, p.dim);
        step(ui, "2", final_host, false);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new("jump hosts first").size(12.0).color(p.dim));
        });
    });
}

fn step(ui: &mut Ui, number: &str, name: &str, current: bool) {
    let p = &Palette::ONE_DARK;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(20.0), egui::Sense::hover());
    let (fill, color) = if current {
        (p.accent, p.bg_dark)
    } else {
        (p.surface, p.text)
    };
    ui.painter().rect_filled(rect, radius::CONTROL, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        number,
        fonts::semibold(11.0),
        color,
    );
    let color = if current { p.text_strong } else { p.dim };
    ui.label(RichText::new(name).size(12.0).color(color));
}

/// One side of the changed key: an overline, then type and fingerprint of
/// each key; the server's side has a Light Red outline.
fn fingerprints(ui: &mut Ui, title: &str, keys: &[KeyInfo], presented: bool) {
    let p = &Palette::ONE_DARK;
    let response = Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(
                RichText::new(title.to_uppercase())
                    .font(fonts::semibold(11.0))
                    .color(if presented { p.red } else { p.dim }),
            );
            for key in keys {
                ui.label(mono(&format!("{} ({} bits)", key.kind, key.bits), p.text));
                let color = if presented { p.text_strong } else { p.text };
                fingerprint(ui, &key.fingerprint, color);
            }
        })
        .response;
    if presented {
        ui.painter().rect_stroke(
            response.rect,
            radius::CONTROL,
            Stroke::new(1.0, p.red),
            StrokeKind::Inside,
        );
    }
}

/// `intro`, the command to run on the server, and *Copy*; returns whether
/// it was copied.
fn compare_hint(ui: &mut Ui, intro: &str) -> bool {
    let p = &Palette::ONE_DARK;
    let mut copied = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.add(egui::Label::new(RichText::new(intro).size(13.0)).wrap());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            Frame::new()
                .fill(p.bg)
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.label(mono(COMPARE_HINT, p.text));
                });
            if button::link(ui, "Copy", 12.0).clicked() {
                ui.ctx().copy_text(COMPARE_HINT.to_string());
                copied = true;
            }
        });
    });
    copied
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshelf_core::hostkeys::{ChangedKey, KeyTarget, UnknownKey};

    use super::*;
    use crate::test_util;

    fn target() -> KeyTarget {
        KeyTarget {
            destination: "web".into(),
            hostname: "web.example.com".into(),
            port: 2222,
            lookup: "[web.example.com]:2222".into(),
            user_files: vec![PathBuf::from("known_hosts")],
            global_files: Vec::new(),
            jumps: Vec::new(),
        }
    }

    fn key(fingerprint: &str) -> KeyInfo {
        KeyInfo {
            bits: "256".into(),
            fingerprint: fingerprint.into(),
            kind: "ED25519".into(),
        }
    }

    fn harness(modals: HostKeyModals) -> Harness<'static, (HostKeyModals, Vec<Command>)> {
        test_util::harness()
            .with_size([1000.0, 800.0])
            .build_ui_state(
                |ui, (modals, commands): &mut (HostKeyModals, Vec<Command>)| {
                    commands.extend(modals.show(ui.ctx()));
                },
                (modals, Vec::new()),
            )
    }

    #[test]
    fn unknown_key_shows_the_fingerprint_and_can_be_accepted() {
        let mut modals = HostKeyModals::default();
        modals.on_unknown(Arc::new(HostKeyPrompt {
            id: 7,
            purpose: "tunnel \"API\"".into(),
            unknown: UnknownKey {
                target: target(),
                lines: vec!["[web.example.com]:2222 ssh-ed25519 AAAA".into()],
                keys: vec![key("SHA256:new")],
                hop_of: Some("internal".into()),
            },
        }));
        let mut harness = harness(modals);
        harness.step();
        harness.get_by_label("First connection to web");
        harness.get_by_label("SHA256:new");
        harness.get_by_label("(stored as [host]:2222)");
        harness.get_by_label("internal");
        harness.get_by_label("Accepting adds 1 line to known_hosts (backed up first).");
        harness.get_by_label("Copy").click();
        harness.step();
        assert_eq!(harness.state_mut().0.take_notices(), ["Copied command"]);
        harness.get_by_label("Accept and continue").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::AcceptHostKey(7)]);

        harness.state_mut().0.on_closed(7);
        harness.step();
        assert!(harness.query_by_label("SHA256:new").is_none());
    }

    #[test]
    fn changed_key_needs_the_host_name_typed() {
        let mut modals = HostKeyModals::default();
        modals.on_changed(Arc::new(ChangedKeyPrompt {
            id: 3,
            subject: "tunnel \u{201c}API\u{201d}".into(),
            changed: ChangedKey {
                target: target(),
                user_files: vec![PathBuf::from("known_hosts")],
                old: vec![key("SHA256:old")],
                new: vec![key("SHA256:new")],
                new_lines: vec!["web.example.com ssh-ed25519 BBBB".into()],
                hop_of: None,
            },
        }));
        let mut harness = harness(modals);
        harness.step();
        harness.get_by_label("The host key of web changed");
        harness.get_by_label("SHA256:old");
        harness.get_by_label("SHA256:new");
        assert!(
            harness
                .get_by_label("Replace key")
                .accesskit_node()
                .is_disabled()
        );
        harness.state_mut().0.typed = "we".into();
        harness.step();
        assert!(
            harness
                .get_by_label("Replace key")
                .accesskit_node()
                .is_disabled()
        );
        harness.state_mut().0.typed = "web".into();
        harness.step();
        harness.get_by_label("Replace key").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::ReplaceHostKey(3)]);
    }

    #[test]
    fn dont_connect_cancels_the_changed_key() {
        let mut modals = HostKeyModals::default();
        modals.on_changed(Arc::new(ChangedKeyPrompt {
            id: 4,
            subject: "tunnel \u{201c}API\u{201d}".into(),
            changed: ChangedKey {
                target: target(),
                user_files: Vec::new(),
                old: vec![key("SHA256:old")],
                new: vec![key("SHA256:new")],
                new_lines: Vec::new(),
                hop_of: None,
            },
        }));
        let mut harness = harness(modals);
        harness.step();
        harness.get_by_label(
            "The stored key is in the system-wide known_hosts; an administrator has to \
             replace it.",
        );
        harness.get_by_label("Don't connect").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::CancelChangedKey(4)]);
    }
}
