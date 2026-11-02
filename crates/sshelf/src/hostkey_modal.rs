//! Host key modals (SPEC.md §9): confirm an unknown key, or replace a
//! changed one after an explicit extra confirmation. Prompts queue up; a
//! changed key is shown before unknown ones.

use std::collections::VecDeque;
use std::sync::Arc;

use egui::RichText;
use sshelf_core::core::{ChangedKeyPrompt, Command, HostKeyPrompt};
use sshelf_core::hostkeys::KeyInfo;

use crate::theme::Palette;

const COMPARE_HINT: &str = "ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub";

#[derive(Default)]
pub struct HostKeyModals {
    unknown: VecDeque<Arc<HostKeyPrompt>>,
    changed: VecDeque<Arc<ChangedKeyPrompt>>,
    /// "I know this server was reinstalled" for the changed prompt on screen.
    reinstalled: bool,
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
            self.reinstalled = false;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Vec<Command> {
        if let Some(prompt) = self.changed.front().cloned() {
            return self.changed_modal(ctx, &prompt);
        }
        if let Some(prompt) = self.unknown.front().cloned() {
            return unknown_modal(ctx, &prompt);
        }
        Vec::new()
    }

    fn changed_modal(&mut self, ctx: &egui::Context, prompt: &ChangedKeyPrompt) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let changed = &prompt.changed;
        let mut commands = Vec::new();
        let response = egui::Modal::new(egui::Id::unique(("hostkey_changed", prompt.id))).show(ctx, |ui| {
            ui.set_max_width(620.0);
            ui.heading(RichText::new("⚠ Host key changed").color(p.red).strong());
            ui.label(format!(
                "{} presents a different key than the one stored for it (tunnel \"{}\").",
                changed.target.display(),
                prompt.tunnel_name
            ));
            if let Some(final_host) = &changed.hop_of {
                ui.label(format!("It is a jump host on the way to {final_host}."));
            }
            ui.label(
                "Either the server was reinstalled or its keys were regenerated — or someone \
                 is intercepting the connection. Only replace the key if you know which.",
            );
            ui.add_space(6.0);
            egui::Grid::new("changed_keys")
                .num_columns(2)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Stored key").strong());
                    ui.label(RichText::new("Key the server presents now").strong());
                    ui.end_row();
                    for i in 0..changed.old.len().max(changed.new.len()) {
                        key_cell(ui, changed.old.get(i), p.red);
                        key_cell(ui, changed.new.get(i), p.yellow);
                        ui.end_row();
                    }
                });
            ui.add_space(6.0);
            ui.label("To check the server's key from its console:");
            ui.code(COMPARE_HINT);
            ui.add_space(6.0);
            let replaceable = !changed.user_files.is_empty();
            if !replaceable {
                ui.colored_label(
                    p.yellow,
                    "The stored key is in the system-wide known_hosts; an administrator has to replace it.",
                );
            }
            ui.add_enabled(
                replaceable,
                egui::Checkbox::new(&mut self.reinstalled, "I know this server was reinstalled"),
            );
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    commands.push(Command::CancelChangedKey(prompt.id));
                }
                let replace = egui::Button::new(RichText::new("Replace key").color(p.red));
                if ui
                    .add_enabled(replaceable && self.reinstalled, replace)
                    .on_disabled_hover_text("Tick the box first")
                    .clicked()
                {
                    commands.push(Command::ReplaceHostKey(prompt.id));
                }
            });
        });
        if response.should_close() && commands.is_empty() {
            commands.push(Command::CancelChangedKey(prompt.id));
        }
        commands
    }
}

fn unknown_modal(ctx: &egui::Context, prompt: &HostKeyPrompt) -> Vec<Command> {
    let p = &Palette::ONE_DARK;
    let unknown = &prompt.unknown;
    let mut commands = Vec::new();
    let response = egui::Modal::new(egui::Id::unique(("hostkey_unknown", prompt.id))).show(ctx, |ui| {
        ui.set_max_width(560.0);
        ui.heading(RichText::new("Unknown host key").color(p.accent).strong());
        ui.label(format!(
            "Starting {} connects to {}, whose key is not stored yet.",
            prompt.purpose,
            unknown.target.display()
        ));
        if let Some(final_host) = &unknown.hop_of {
            ui.label(format!("It is a jump host on the way to {final_host}."));
        }
        ui.add_space(6.0);
        egui::Grid::new("unknown_key")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label("Host");
                ui.label(&unknown.target.hostname);
                ui.end_row();
                ui.label("Port");
                ui.label(unknown.target.port.to_string());
                ui.end_row();
                for key in &unknown.keys {
                    ui.label(format!("{} ({} bits)", key.kind, key.bits));
                    ui.label(RichText::new(&key.fingerprint).monospace());
                    ui.end_row();
                }
            });
        ui.add_space(6.0);
        ui.label("Compare the fingerprint with the server's before trusting it, e.g. on its console:");
        ui.code(COMPARE_HINT);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Trust and continue").clicked() {
                commands.push(Command::AcceptHostKey(prompt.id));
            }
            if ui.button("Don't trust").clicked() {
                commands.push(Command::RejectHostKey(prompt.id));
            }
        });
    });
    if response.should_close() && commands.is_empty() {
        commands.push(Command::RejectHostKey(prompt.id));
    }
    commands
}

fn key_cell(ui: &mut egui::Ui, key: Option<&KeyInfo>, color: egui::Color32) {
    match key {
        Some(key) => {
            ui.vertical(|ui| {
                ui.label(format!("{} ({} bits)", key.kind, key.bits));
                ui.label(RichText::new(&key.fingerprint).monospace().color(color));
            });
        }
        None => {
            ui.label("");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sshelf_core::hostkeys::{ChangedKey, KeyTarget, UnknownKey};

    use super::*;

    fn target() -> KeyTarget {
        KeyTarget {
            destination: "web".into(),
            hostname: "web.example.com".into(),
            port: 22,
            lookup: "web.example.com".into(),
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
        Harness::builder().with_size([900.0, 700.0]).build_ui_state(
            |ui, (modals, commands): &mut (HostKeyModals, Vec<Command>)| {
                commands.extend(modals.show(ui.ctx()));
            },
            (modals, Vec::new()),
        )
    }

    #[test]
    fn unknown_key_shows_the_fingerprint_and_can_be_trusted() {
        let mut modals = HostKeyModals::default();
        modals.on_unknown(Arc::new(HostKeyPrompt {
            id: 7,
            purpose: "tunnel \"API\"".into(),
            unknown: UnknownKey {
                target: target(),
                lines: vec!["web.example.com ssh-ed25519 AAAA".into()],
                keys: vec![key("SHA256:new")],
                hop_of: Some("internal".into()),
            },
        }));
        let mut harness = harness(modals);
        harness.step();
        harness.get_by_label("SHA256:new");
        harness.get_by_label("It is a jump host on the way to internal.");
        harness.get_by_label("Trust and continue").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::AcceptHostKey(7)]);

        harness.state_mut().0.on_closed(7);
        harness.step();
        assert!(harness.query_by_label("SHA256:new").is_none());
    }

    #[test]
    fn changed_key_needs_the_extra_confirmation() {
        let mut modals = HostKeyModals::default();
        modals.on_changed(Arc::new(ChangedKeyPrompt {
            id: 3,
            tunnel_name: "API".into(),
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
        harness.get_by_label("SHA256:old");
        harness.get_by_label("SHA256:new");
        harness.get_by_label("Replace key").click();
        harness.step();
        assert!(harness.state().1.is_empty(), "disabled until confirmed");

        harness
            .get_by_label("I know this server was reinstalled")
            .click();
        harness.step();
        harness.get_by_label("Replace key").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::ReplaceHostKey(3)]);
    }
}
