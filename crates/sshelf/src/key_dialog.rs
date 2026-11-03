//! "Generate key…" (SPEC.md §11): a window (not a modal, so the host editor
//! stays usable for saving) with the key options, then the fingerprint, the
//! public key and the follow-ups: use it in the host, add it to the agent,
//! install it on the server. Generation and every file and process are the
//! core's; typed passphrases are wiped as soon as they are sent.

use std::path::{Path, PathBuf};

use egui::RichText;
use sshelf_core::core::{Command, ConfigSnapshot, SshInfo};
use sshelf_core::keygen::{
    self, DEFAULT_KDF_ROUNDS, GeneratedKey, KeyAlgorithm, KeyRequest, MAX_KDF_ROUNDS, Secret,
};
use zeroize::Zeroize;

use crate::icons;
use crate::theme::Palette;

/// What the host editor should do with a generated key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Add `IdentityFile <path>` and `IdentitiesOnly yes` to the draft.
    UseInHost { identity_file: String },
}

fn default_comment(alias: &str) -> String {
    if alias.is_empty() {
        "sshelf".into()
    } else {
        format!("{alias} (sshelf)")
    }
}

struct Form {
    /// The host the key is for (may still be unsaved, or empty).
    alias: String,
    /// `~/.ssh`, for default names and `~/` paths.
    ssh_dir: PathBuf,
    algorithm: KeyAlgorithm,
    path: String,
    /// The file name was typed: keep it when the algorithm changes.
    path_edited: bool,
    comment: String,
    passphrase: String,
    confirm: String,
    rounds: u32,
    pending: Option<u64>,
    error: Option<String>,
    generated: Option<GeneratedKey>,
    /// A security key: ssh-keygen was opened in a terminal; waiting for Done.
    in_terminal: bool,
}

impl Drop for Form {
    fn drop(&mut self) {
        self.passphrase.zeroize();
        self.confirm.zeroize();
    }
}

impl Form {
    fn new(alias: &str, ssh_dir: &Path) -> Self {
        let mut form = Self {
            alias: alias.to_string(),
            ssh_dir: ssh_dir.to_path_buf(),
            algorithm: KeyAlgorithm::default(),
            path: String::new(),
            path_edited: false,
            comment: default_comment(alias),
            passphrase: String::new(),
            confirm: String::new(),
            rounds: DEFAULT_KDF_ROUNDS,
            pending: None,
            error: None,
            generated: None,
            in_terminal: false,
        };
        form.path = form.default_path();
        form
    }

    fn default_path(&self) -> String {
        let stem = self.algorithm.file_stem();
        let name = if self.alias.is_empty() {
            stem.to_string()
        } else {
            format!("{stem}_{}", self.alias)
        };
        self.ssh_dir.join(name).display().to_string()
    }

    /// The typed path, with a leading `~/` meaning the home directory.
    fn target(&self) -> PathBuf {
        let typed = self.path.trim();
        match typed
            .strip_prefix("~/")
            .or_else(|| typed.strip_prefix("~\\"))
        {
            Some(rest) => self.ssh_dir.parent().unwrap_or(&self.ssh_dir).join(rest),
            None => PathBuf::from(typed),
        }
    }

    /// Reasons Generate is disabled.
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.path.trim().is_empty() {
            problems.push("Choose a file name.".to_string());
        }
        if let Err(e) = keygen::check_comment(&self.comment) {
            problems.push(format!("The comment: {e}."));
        }
        if self.passphrase != self.confirm {
            problems.push("The passphrases do not match.".to_string());
        }
        if !(1..=MAX_KDF_ROUNDS).contains(&self.rounds) {
            problems.push(format!("KDF rounds must be 1 to {MAX_KDF_ROUNDS}."));
        }
        problems
    }

    /// The request, taking (not copying) the passphrase; both fields are
    /// wiped.
    fn take_request(&mut self) -> KeyRequest {
        self.confirm.zeroize();
        KeyRequest {
            algorithm: self.algorithm,
            path: self.target(),
            comment: self.comment.trim().to_string(),
            passphrase: Secret::new(std::mem::take(&mut self.passphrase)),
            kdf_rounds: self.rounds,
        }
    }

    /// How the key is written in the config: `~/.ssh/<name>` when it is in
    /// the ssh dir (portable and readable), else the full path.
    fn identity_file(&self, key: &GeneratedKey) -> String {
        match key.path.strip_prefix(&self.ssh_dir) {
            Ok(rest) => format!("~/.ssh/{}", rest.display()).replace('\\', "/"),
            Err(_) => key.path.display().to_string(),
        }
    }
}

#[derive(Default)]
pub struct KeyDialog {
    form: Option<Form>,
}

impl KeyDialog {
    pub fn open(&mut self, alias: &str, ssh_dir: &Path) {
        self.form = Some(Form::new(alias, ssh_dir));
    }

    /// Follows the host's alias while it is being typed (a new host): the
    /// install target, and the default file name and comment unless edited.
    pub fn follow_alias(&mut self, alias: &str) {
        let Some(form) = &mut self.form else {
            return;
        };
        if form.alias == alias {
            return;
        }
        let old_comment = default_comment(&form.alias);
        form.alias = alias.to_string();
        if form.generated.is_none() {
            if !form.path_edited {
                form.path = form.default_path();
            }
            if form.comment == old_comment {
                form.comment = default_comment(alias);
            }
        }
    }

    pub fn on_generated(&mut self, id: u64, result: Result<GeneratedKey, String>) {
        let Some(form) = &mut self.form else {
            return;
        };
        if form.pending != Some(id) {
            return;
        }
        form.pending = None;
        match result {
            Ok(key) => form.generated = Some(key),
            Err(e) => form.error = Some(e),
        }
    }

    /// The window. `id` is the next request id; `editing` whether a host
    /// editor is open to take [`KeyAction::UseInHost`].
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id: u64,
        snapshot: &ConfigSnapshot,
        ssh: Option<&SshInfo>,
        editing: bool,
    ) -> (Vec<Command>, Option<KeyAction>) {
        let mut commands = Vec::new();
        let mut action = None;
        let Some(form) = &mut self.form else {
            return (commands, action);
        };
        let mut open = true;
        let title = if form.alias.is_empty() {
            "Generate key".to_string()
        } else {
            format!("Generate key for {}", form.alias)
        };
        egui::Window::new(title)
            .id(egui::Id::unique("generate_key"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| match form.generated.clone() {
                None => options(ui, form, id, &mut commands),
                Some(key) => {
                    action = result(ui, form, &key, snapshot, ssh, editing, &mut commands);
                }
            });
        if !open {
            self.form = None;
        }
        (commands, action)
    }
}

fn options(ui: &mut egui::Ui, form: &mut Form, id: u64, commands: &mut Vec<Command>) {
    let p = &Palette::ONE_DARK;
    let busy = form.pending.is_some();
    ui.add_enabled_ui(!busy, |ui| {
        egui::Grid::new("key_options")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                let label = ui.label("Algorithm");
                let before = form.algorithm;
                egui::ComboBox::from_id_salt("key_algorithm")
                    .selected_text(form.algorithm.label())
                    .show_ui(ui, |ui| {
                        for algorithm in KeyAlgorithm::ALL {
                            ui.selectable_value(&mut form.algorithm, algorithm, algorithm.label());
                        }
                    })
                    .response
                    .labelled_by(label.id);
                if form.algorithm != before && !form.path_edited {
                    form.path = form.default_path();
                }
                ui.end_row();

                // A security key's passphrase is asked for by ssh-keygen.
                if !form.algorithm.is_hardware() {
                    let label = ui.label("Passphrase");
                    ui.add(egui::TextEdit::singleline(&mut form.passphrase).password(true))
                        .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label("Repeat passphrase");
                    ui.add(egui::TextEdit::singleline(&mut form.confirm).password(true))
                        .labelled_by(label.id);
                    ui.end_row();
                }

                let label = ui.label("KDF rounds");
                ui.add(egui::DragValue::new(&mut form.rounds).range(1..=MAX_KDF_ROUNDS))
                    .labelled_by(label.id)
                    .on_hover_text("bcrypt rounds: more is slower to unlock and to guess");
                ui.end_row();

                let label = ui.label("Comment");
                ui.add(egui::TextEdit::singleline(&mut form.comment).desired_width(320.0))
                    .labelled_by(label.id);
                ui.end_row();

                let label = ui.label("File");
                let response = ui
                    .add(egui::TextEdit::singleline(&mut form.path).desired_width(360.0))
                    .labelled_by(label.id);
                if response.changed() {
                    form.path_edited = true;
                }
                ui.end_row();
            });
    });
    ui.label(
        RichText::new(
            "The public key is written next to it as .pub. Existing files are never replaced.",
        )
        .small()
        .color(p.dim),
    );
    let hardware = form.algorithm.is_hardware();
    if hardware {
        ui.label(
            "ssh-keygen runs in a terminal: touch the security key (and enter its PIN) \
             when asked, and set a passphrase there if you want one. Then come back and \
             click Done.",
        );
    } else if form.passphrase.is_empty() {
        icons::label(
            ui,
            icons::TRIANGLE_ALERT,
            p.yellow,
            "Without a passphrase, anyone who gets this file can use the key.",
        );
    }
    let problems = form.problems();
    for problem in &problems {
        ui.colored_label(p.red, problem);
    }
    if let Some(e) = &form.error {
        ui.colored_label(p.red, e);
    }
    if form.in_terminal && hardware {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Done"))
                .on_hover_text("Reads the new .pub file (never the private key)")
                .clicked()
            {
                form.error = None;
                form.pending = Some(id);
                commands.push(Command::LoadPublicKey {
                    id,
                    path: form.target(),
                });
            }
            ui.label(
                RichText::new("when ssh-keygen has finished")
                    .small()
                    .color(p.dim),
            );
        });
    }
    ui.horizontal(|ui| {
        let can_start = problems.is_empty() && !busy && !(hardware && form.in_terminal);
        if ui
            .add_enabled(can_start, egui::Button::new("Generate"))
            .clicked()
        {
            form.error = None;
            if hardware {
                form.in_terminal = true;
                commands.push(Command::GenerateHardwareKey(form.take_request()));
            } else {
                form.pending = Some(id);
                commands.push(Command::GenerateKey {
                    id,
                    request: form.take_request(),
                });
            }
        }
        if busy {
            ui.spinner();
            ui.label("Generating…");
        }
    });
}

fn result(
    ui: &mut egui::Ui,
    form: &mut Form,
    key: &GeneratedKey,
    snapshot: &ConfigSnapshot,
    ssh: Option<&SshInfo>,
    editing: bool,
    commands: &mut Vec<Command>,
) -> Option<KeyAction> {
    let p = &Palette::ONE_DARK;
    let mut action = None;
    icons::label(
        ui,
        icons::CIRCLE_CHECK,
        p.green,
        format!("Key written to {}", key.path.display()),
    );
    egui::Grid::new("key_result")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("Fingerprint");
            ui.monospace(&key.fingerprint);
            ui.end_row();
            ui.label("Passphrase");
            ui.label(if key.encrypted { "yes" } else { "none" });
            ui.end_row();
        });
    ui.horizontal(|ui| {
        ui.label("Public key");
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(key.public_line.clone());
        }
    });
    ui.add(egui::Label::new(RichText::new(&key.public_line).monospace().small()).wrap());
    ui.separator();

    // 1. Use in this host.
    ui.horizontal(|ui| {
        let identity_file = form.identity_file(key);
        if ui
            .add_enabled(editing, egui::Button::new("Use in this host"))
            .on_hover_text(format!(
                "Adds `IdentityFile {identity_file}` and `IdentitiesOnly yes` to the host you are editing"
            ))
            .on_disabled_hover_text("Open the host in the editor first")
            .clicked()
        {
            action = Some(KeyAction::UseInHost { identity_file });
        }
        ui.label(RichText::new("then Save the host").small().color(p.dim));
    });

    // 2. Add to the agent.
    let service_stopped = ssh
        .and_then(|s| s.service.as_ref())
        .and_then(|s| s.as_ref().ok())
        .is_some_and(|s| !s.is_running());
    ui.horizontal(|ui| {
        let add = ui
            .add_enabled(!service_stopped, egui::Button::new("Add to agent"))
            .on_hover_text(if key.encrypted {
                "Runs ssh-add in a terminal, where you type the passphrase"
            } else {
                "Runs ssh-add"
            })
            .on_disabled_hover_text("The ssh-agent service is not running (see Settings)");
        if add.clicked() {
            commands.push(Command::AddKeyToAgent {
                path: key.path.clone(),
                encrypted: key.encrypted,
            });
        }
        if service_stopped {
            ui.label(
                RichText::new("the ssh-agent service is not running")
                    .small()
                    .color(p.yellow),
            );
        }
    });

    // 3. Install on the server.
    let saved = !form.alias.is_empty()
        && snapshot
            .hosts()
            .is_some_and(|h| h.find(&form.alias).is_some());
    ui.horizontal(|ui| {
        let install = ui
            .add_enabled(saved, egui::Button::new("Install on server"))
            .on_hover_text(
                "Opens a terminal that adds the public key to the server's authorized_keys; \
                 the first login there usually asks for a password",
            )
            .on_disabled_hover_text("Save the host first");
        if install.clicked() {
            commands.push(Command::InstallKey {
                alias: form.alias.clone(),
                public_line: key.public_line.clone(),
            });
        }
        ui.label(
            RichText::new("needs a POSIX shell on the server")
                .small()
                .color(p.dim),
        );
    });
    action
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshconf::{ConfigSet, HostList, ResolveOptions};
    use sshelf_core::core::ConfigState;

    use super::*;
    use crate::test_util;

    fn snapshot(text: &str) -> Arc<ConfigSnapshot> {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        fs::write(&config, text).unwrap();
        let set = ConfigSet::load(&config, &ResolveOptions::new(tmp.path())).unwrap();
        let hosts = HostList::from_set(&set);
        Arc::new(ConfigSnapshot {
            generation: 1,
            path: config,
            state: ConfigState::Loaded { set, hosts },
            read_only: Vec::new(),
        })
    }

    struct State {
        dialog: KeyDialog,
        snapshot: Arc<ConfigSnapshot>,
        commands: Vec<Command>,
        actions: Vec<KeyAction>,
    }

    fn harness(alias: &str, config: &str) -> Harness<'static, State> {
        let mut dialog = KeyDialog::default();
        dialog.open(alias, Path::new("/home/me/.ssh"));
        test_util::harness()
            .with_size([900.0, 800.0])
            .build_ui_state(
                |ui, state: &mut State| {
                    let (commands, action) =
                        state.dialog.show(ui.ctx(), 1, &state.snapshot, None, true);
                    state.commands.extend(commands);
                    state.actions.extend(action);
                },
                State {
                    dialog,
                    snapshot: snapshot(config),
                    commands: Vec::new(),
                    actions: Vec::new(),
                },
            )
    }

    fn form<'a>(harness: &'a mut Harness<'_, State>) -> &'a mut Form {
        harness.state_mut().dialog.form.as_mut().unwrap()
    }

    #[test]
    fn defaults_follow_the_algorithm_until_the_name_is_typed() {
        let mut harness = harness("web", "Host web\n");
        harness.step();
        let path = |h: &mut Harness<'_, State>| form(h).path.replace('\\', "/");
        assert_eq!(path(&mut harness), "/home/me/.ssh/id_ed25519_web");
        assert_eq!(form(&mut harness).comment, "web (sshelf)");
        form(&mut harness).algorithm = KeyAlgorithm::Rsa4096;
        harness.step();
        // Changing the algorithm in the combo box is what renames; set the
        // field the way the combo box does and check the default helper.
        assert!(
            form(&mut harness)
                .default_path()
                .replace('\\', "/")
                .ends_with("id_rsa_web")
        );
        form(&mut harness).path = "~/.ssh/custom".into();
        assert_eq!(
            form(&mut harness).target(),
            Path::new("/home/me").join(".ssh/custom")
        );
    }

    #[test]
    fn generate_waits_for_matching_passphrases_and_wipes_them() {
        let mut harness = harness("web", "Host web\n");
        harness.step();
        harness.get_by_label_contains("Without a passphrase");
        form(&mut harness).passphrase = "secret one".into();
        form(&mut harness).confirm = "secret two".into();
        harness.step();
        harness.get_by_label("The passphrases do not match.");
        assert!(
            harness
                .get_by_label("Generate")
                .accesskit_node()
                .is_disabled()
        );

        form(&mut harness).confirm = "secret one".into();
        form(&mut harness).comment = "it's mine".into();
        harness.step();
        assert!(
            harness
                .get_by_label("Generate")
                .accesskit_node()
                .is_disabled()
        );
        form(&mut harness).comment = "web (sshelf)".into();
        harness.step();
        harness.get_by_label("Generate").click();
        harness.step();
        let Some(Command::GenerateKey { id, request }) = harness.state().commands.last().cloned()
        else {
            panic!("no request sent");
        };
        assert_eq!(id, 1);
        assert_eq!(request.passphrase.expose(), "secret one");
        assert_eq!(request.kdf_rounds, DEFAULT_KDF_ROUNDS);
        assert!(form(&mut harness).passphrase.is_empty());
        assert!(form(&mut harness).confirm.is_empty());
        harness.step();
        harness.get_by_label("Generating…");
    }

    fn generated() -> GeneratedKey {
        GeneratedKey {
            path: Path::new("/home/me/.ssh").join("id_ed25519_web"),
            public_path: Path::new("/home/me/.ssh").join("id_ed25519_web.pub"),
            public_line: "ssh-ed25519 AAAAC3Nza web (sshelf)".into(),
            fingerprint: "SHA256:abc".into(),
            encrypted: true,
        }
    }

    #[test]
    fn security_keys_are_made_in_the_terminal_then_loaded() {
        let mut harness = harness("web", "Host web\n");
        harness.step();
        form(&mut harness).algorithm = KeyAlgorithm::Ed25519Sk;
        form(&mut harness).path = form(&mut harness).default_path();
        harness.step();
        assert!(harness.query_by_label("Passphrase").is_none());
        harness.get_by_label_contains("touch the security key");
        harness.get_by_label("Generate").click();
        harness.step();
        let Some(Command::GenerateHardwareKey(request)) = harness.state().commands.last().cloned()
        else {
            panic!("{:?}", harness.state().commands);
        };
        assert_eq!(request.algorithm, KeyAlgorithm::Ed25519Sk);
        assert!(request.path.ends_with("id_ed25519_sk_web"));
        assert!(request.passphrase.is_empty());
        harness.step();
        // Generate waits; Done loads the result.
        assert!(
            harness
                .get_by_label("Generate")
                .accesskit_node()
                .is_disabled()
        );
        harness.get_by_label("Done").click();
        harness.step();
        assert_eq!(
            harness.state().commands.last(),
            Some(&Command::LoadPublicKey {
                id: 1,
                path: request.path.clone()
            })
        );
        harness.state_mut().dialog.on_generated(1, Ok(generated()));
        harness.step();
        harness.get_by_label("SHA256:abc");
    }

    #[test]
    fn errors_come_back_into_the_form() {
        let mut harness = harness("web", "Host web\n");
        harness.step();
        form(&mut harness).pending = Some(1);
        harness
            .state_mut()
            .dialog
            .on_generated(1, Err("/home/me/.ssh/id_ed25519_web already exists".into()));
        harness.step();
        harness.get_by_label("/home/me/.ssh/id_ed25519_web already exists");
        assert!(
            !harness
                .get_by_label("Generate")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn follow_ups_after_generating() {
        let mut harness = harness("web", "Host web\n");
        harness.step();
        form(&mut harness).pending = Some(1);
        harness.state_mut().dialog.on_generated(1, Ok(generated()));
        harness.step();
        harness.get_by_label("SHA256:abc");

        harness.get_by_label("Use in this host").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            [KeyAction::UseInHost {
                identity_file: "~/.ssh/id_ed25519_web".into()
            }]
        );

        harness.get_by_label("Add to agent").click();
        harness.step();
        harness.get_by_label("Install on server").click();
        harness.step();
        assert_eq!(
            harness.state().commands,
            [
                Command::AddKeyToAgent {
                    path: generated().path,
                    encrypted: true
                },
                Command::InstallKey {
                    alias: "web".into(),
                    public_line: generated().public_line
                }
            ]
        );
    }

    #[test]
    fn install_waits_until_the_host_is_saved() {
        let mut harness = harness("fresh", "Host web\n");
        harness.step();
        form(&mut harness).pending = Some(1);
        harness.state_mut().dialog.on_generated(1, Ok(generated()));
        harness.step();
        assert!(
            harness
                .get_by_label("Install on server")
                .accesskit_node()
                .is_disabled()
        );
    }
}
