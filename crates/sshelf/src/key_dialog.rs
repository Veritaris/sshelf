//! "Generate key…" (SPEC.md §10 *Key generation*, §11): a window (not a
//! modal, so the host editor stays usable for saving) styled like the
//! dialogs, with the key options, then the fingerprint, the public key and
//! the next steps: use it in the host, add it to the agent, install it on
//! the server. Generation and every file and process are the core's; typed
//! passphrases are wiped as soon as they are sent.

use std::path::{Path, PathBuf};

use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Frame, Id, Layout, Margin, Rect, RichText, Role,
    Sense, Stroke, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use sshelf_core::core::{Command, ConfigSnapshot, SshInfo};
use sshelf_core::keygen::{
    self, DEFAULT_KDF_ROUNDS, GeneratedKey, KeyAlgorithm, KeyRequest, MAX_KDF_ROUNDS, Secret,
};
use zeroize::Zeroize;

use crate::theme::{Palette, radius, timing};
use crate::tour::targets::{self, Target};
use crate::widgets::button::{self, Button};
use crate::widgets::dropdown::{Choice, Dropdown};
use crate::widgets::field::TextField;
use crate::widgets::{copy, marks, text};
use crate::{fonts, icons, motion};

/// What the host editor (or the app) should do for the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Add `IdentityFile <path>` and `IdentitiesOnly yes` to the draft.
    UseInHost { identity_file: String },
    /// Something was copied: a toast "Copied <what>".
    Copied(String),
    /// "How to start it" next to a stopped agent: *Settings*.
    OpenSettings,
}

/// The dialog's width, its form's label column and the gap after it.
const WIDTH: f32 = 640.0;
const LABEL: f32 = 150.0;
const GAP: f32 = 16.0;

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
    /// The passphrase fields show their text.
    shown: bool,
    rounds: u32,
    /// The KDF rounds as typed (`rounds` is 0 while it isn't a number).
    rounds_text: String,
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
            shown: false,
            rounds: DEFAULT_KDF_ROUNDS,
            rounds_text: DEFAULT_KDF_ROUNDS.to_string(),
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

    fn mismatch(&self) -> bool {
        self.passphrase != self.confirm
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
        if self.mismatch() {
            problems.push("The passphrases do not match.".to_string());
        }
        if !(1..=MAX_KDF_ROUNDS).contains(&self.rounds) {
            problems.push(format!("KDF rounds must be 1 to {MAX_KDF_ROUNDS}."));
        }
        problems
    }

    fn set_rounds(&mut self, rounds: u32) {
        self.rounds = rounds.clamp(1, MAX_KDF_ROUNDS);
        self.rounds_text = self.rounds.to_string();
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
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let mut action = None;
        let Some(form) = &mut self.form else {
            return (commands, action);
        };
        let mut close = false;
        let window = egui::Window::new("Generate key")
            .id(Id::unique("generate_key"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + vec2(0.0, 96.0))
            .frame(
                Frame::new()
                    .fill(p.bg_dark)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::LARGE)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(WIDTH - 48.0);
                // Fields are inset: lighter than the dialog.
                ui.visuals_mut().extreme_bg_color = p.bg;
                ui.spacing_mut().item_spacing.y = 16.0;
                match form.generated.clone() {
                    None => {
                        close = header(
                            ui,
                            (icons::KEY_ROUND, p.surface, p.accent),
                            &title(&form.alias, "Generate a key"),
                            "Made in sshelf; the passphrase never leaves this window.",
                            false,
                        );
                        close |= options(ui, form, id, &mut commands);
                    }
                    Some(key) => {
                        close = header(
                            ui,
                            (icons::CIRCLE_CHECK, p.green_tint, p.green),
                            &title(&form.alias, "Key created"),
                            &key.path.display().to_string(),
                            true,
                        );
                        let (done, picked) =
                            result(ui, form, &key, snapshot, ssh, editing, &mut commands);
                        close |= done;
                        action = picked;
                    }
                }
            });
        if let Some(window) = window {
            targets::register(ctx, Target::KeyWindow, window.response.rect);
        }
        if close {
            self.form = None;
        }
        (commands, action)
    }

    /// Open (`Some`), and whether its key was created.
    pub fn state(&self) -> Option<bool> {
        self.form.as_ref().map(|f| f.generated.is_some())
    }

    pub fn close(&mut self) {
        self.form = None;
    }

    /// Fills both passphrase fields while nothing was typed (the tour).
    pub fn prefill_passphrase(&mut self, passphrase: &str) {
        if let Some(form) = &mut self.form
            && form.passphrase.is_empty()
            && form.generated.is_none()
        {
            form.passphrase = passphrase.to_string();
            form.confirm = passphrase.to_string();
        }
    }
}

/// "Generate a key for web" (or without the host).
fn title(alias: &str, start: &str) -> String {
    if alias.is_empty() {
        start.to_string()
    } else {
        format!("{start} for {alias}")
    }
}

/// The icon tile, title, a second line (`mono`: a path) and *Close*;
/// returns whether *Close* was clicked.
fn header(
    ui: &mut Ui,
    (icon, tile, color): (char, Color32, Color32),
    title: &str,
    second: &str,
    mono: bool,
) -> bool {
    let p = &Palette::ONE_DARK;
    let mut close = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        marks::tile(ui, icon, tile, color);
        let room = ui.available_width() - 32.0 - 12.0;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .font(fonts::semibold(17.0))
                    .color(p.text_strong),
            );
            let font = if mono {
                FontId::monospace(12.0)
            } else {
                FontId::proportional(12.0)
            };
            let (shown, _) = text::elide(ui.painter(), second, &font, room);
            ui.label(RichText::new(shown).font(font).color(p.dim));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui
                .add(Button::icon_only(icons::X, "Close").ghost())
                .clicked();
        });
    });
    close
}

/// A form row: the label (150 px), then `add` in the rest, which is given
/// the label's id.
fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui, Id) -> R) -> R {
    labelled_row(ui, (label, false), add)
}

/// [`row`], whose field (a dropdown) may carry the name itself.
fn labelled_row<R>(
    ui: &mut Ui,
    (label, named_by_field): (&str, bool),
    add: impl FnOnce(&mut Ui, Id) -> R,
) -> R {
    let width = ui.available_width();
    let top = ui.cursor().min;
    let mut label_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(top, vec2(LABEL, 32.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    // A dropdown carries its own name: its label is only painted, so the
    // two don't clash.
    let label_id = if named_by_field {
        let p = &Palette::ONE_DARK;
        let galley = label_ui.painter().layout_no_wrap(
            label.to_string(),
            FontId::proportional(13.0),
            p.text,
        );
        let (rect, _) = label_ui.allocate_exact_size(galley.size(), Sense::hover());
        label_ui.painter().galley(rect.min, galley, p.text);
        Id::NULL
    } else {
        label_ui.label(RichText::new(label).size(13.0)).id
    };
    let field_width = width - LABEL - GAP;
    let mut field_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(
                top + vec2(LABEL + GAP, 0.0),
                vec2(field_width, f32::INFINITY),
            ))
            .layout(Layout::top_down(Align::Min)),
    );
    field_ui.set_width(field_width);
    field_ui.spacing_mut().item_spacing.y = 6.0;
    let inner = add(&mut field_ui, label_id);
    let height = field_ui.min_rect().height().max(32.0);
    ui.allocate_space(vec2(width, height));
    inner
}

/// The key options; returns whether *Cancel* was clicked.
fn options(ui: &mut Ui, form: &mut Form, id: u64, commands: &mut Vec<Command>) -> bool {
    let p = &Palette::ONE_DARK;
    let busy = form.pending.is_some();
    let hardware = form.algorithm.is_hardware();
    ui.add_enabled_ui(!busy, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            labelled_row(ui, ("Algorithm", true), |ui, _| algorithm(ui, form));
            if hardware {
                // A security key's passphrase is asked for by ssh-keygen.
                info(
                    ui,
                    "ssh-keygen runs in a terminal: touch the security key (and enter its \
                     PIN) when asked, and set a passphrase there if you want one. Then come \
                     back and click Done.",
                );
            } else {
                row(ui, "Passphrase", |ui, label| {
                    passphrase_field(ui, form, label, true);
                });
                row(ui, "Repeat", |ui, label| {
                    passphrase_field(ui, form, label, false);
                    if form.mismatch() && !form.confirm.is_empty() {
                        ui.label(
                            RichText::new("The passphrases do not match.")
                                .size(12.0)
                                .color(p.red),
                        );
                    }
                    if form.passphrase.is_empty() {
                        marks::banner(ui, p.yellow_tint, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                                ui.label(
                                    RichText::new(
                                        "Without a passphrase, anyone who gets the file can \
                                         use the key.",
                                    )
                                    .color(p.yellow),
                                );
                            });
                        });
                    }
                });
            }
            row(ui, "KDF rounds", |ui, label| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    stepper(ui, form, label);
                    ui.label(
                        RichText::new("more rounds = slower to unlock and to brute-force")
                            .size(12.0)
                            .color(p.dim),
                    );
                });
            });
            row(ui, "Comment", |ui, label| {
                ui.add(TextField::new(&mut form.comment)).labelled_by(label);
            });
            row(ui, "File", |ui, label| {
                let response = ui
                    .add(TextField::new(&mut form.path).mono())
                    .labelled_by(label);
                if response.changed() {
                    form.path_edited = true;
                }
                ui.label(
                    RichText::new(
                        "The public key goes next to it as .pub. Existing files are never \
                         replaced.",
                    )
                    .size(12.0)
                    .color(p.dim),
                );
            });
        });
    });

    // Problems the fields don't already show.
    let problems: Vec<String> = form
        .problems()
        .into_iter()
        .filter(|p| p != "The passphrases do not match.")
        .collect();
    for problem in &problems {
        ui.label(RichText::new(problem).size(12.0).color(p.red));
    }
    if let Some(e) = &form.error {
        ui.label(RichText::new(e).size(12.0).color(p.red));
    }

    // Footer.
    let mut cancel = false;
    separator(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let blocked = !form.problems().is_empty();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if form.in_terminal && hardware {
                let done = Button::new("Done")
                    .primary()
                    .dialog()
                    .tip("Reads the new .pub file (never the private key)");
                if ui.add_enabled(!busy, done).clicked() {
                    form.error = None;
                    form.pending = Some(id);
                    commands.push(Command::LoadPublicKey {
                        id,
                        path: form.target(),
                    });
                }
                ui.label(
                    RichText::new("when ssh-keygen has finished")
                        .size(12.0)
                        .color(p.dim),
                );
            } else {
                let mut generate = Button::new("Generate").primary().dialog();
                if busy {
                    generate = generate.busy();
                }
                let why = form.problems().into_iter().next();
                if let Some(why) = &why {
                    generate = generate.tip(why);
                }
                let response = ui.add_enabled(!blocked && !busy, generate);
                if targets::clicked(ui.ctx(), Target::KeyGenerate, &response) && !blocked && !busy {
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
            }
            cancel = ui.add(Button::new("Cancel").ghost().dialog()).clicked();
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let note = if busy {
                    "Generating…"
                } else {
                    "Security keys run ssh-keygen in your terminal, because the device needs \
                     a touch or PIN."
                };
                ui.add(egui::Label::new(RichText::new(note).size(12.0).color(p.dim)).wrap());
            });
        });
    });
    cancel
}

/// The algorithm dropdown: Ed25519 recommended; security keys in their own
/// group, noted as opening a terminal (SPEC.md §10).
fn algorithm(ui: &mut Ui, form: &mut Form) {
    let mut first_hardware = true;
    let choices: Vec<Choice<KeyAlgorithm>> = KeyAlgorithm::ALL
        .into_iter()
        .map(|algorithm| {
            let mut choice = Choice::new(
                algorithm,
                algorithm.label().trim_end_matches(" (security key)"),
            );
            if algorithm == KeyAlgorithm::Ed25519 {
                choice.note = Some("recommended".into());
            }
            if algorithm.is_hardware() && std::mem::take(&mut first_hardware) {
                choice.group = Some("Security key · opens a terminal".into());
            }
            choice
        })
        .collect();
    let trigger = form.algorithm.label().to_string();
    let picked = Dropdown::with_choices("key_algorithm", "Algorithm", choices, trigger)
        .field(240.0)
        .panel_width(300.0)
        .show(ui, form.algorithm);
    if let Some(picked) = picked.filter(|a| *a != form.algorithm) {
        form.algorithm = picked;
        if !form.path_edited {
            form.path = form.default_path();
        }
    }
}

/// A passphrase field (`first`: the one with the show / hide eye; both
/// follow it).
fn passphrase_field(ui: &mut Ui, form: &mut Form, label: Id, first: bool) {
    let p = &Palette::ONE_DARK;
    let error = !first && form.mismatch() && !form.confirm.is_empty();
    let (text, hint) = if first {
        (&mut form.passphrase, "empty: no passphrase")
    } else {
        (&mut form.confirm, "type it again")
    };
    let response = ui
        .add(
            TextField::new(text)
                .password(!form.shown)
                .hint(hint)
                .inset_right(28.0)
                .error(error),
        )
        .labelled_by(label);
    if !first {
        return;
    }
    // The eye sits inside the field's right end.
    let center = pos2(response.rect.right() + 22.0, response.rect.center().y);
    let eye = ui.interact(
        Rect::from_center_size(center, Vec2::splat(24.0)),
        response.id.with("eye"),
        Sense::click(),
    );
    let name = if form.shown {
        "Hide passphrase"
    } else {
        "Show passphrase"
    };
    eye.widget_info(|| WidgetInfo::labeled(Role::Button, eye.enabled(), name));
    let hover = motion::fade(
        ui.ctx(),
        eye.id.with("hover"),
        eye.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        if form.shown {
            icons::EYE_OFF
        } else {
            icons::EYE
        },
        icons::font(16.0),
        p.dim.lerp_to_gamma(p.text_strong, hover),
    );
    if eye.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        form.shown = !form.shown;
    }
}

/// − rounds + on one inset strip.
fn stepper(ui: &mut Ui, form: &mut Form, label: Id) {
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(p.bg)
        .corner_radius(radius::CONTROL)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if ui
                    .add(
                        Button::icon_only(icons::MINUS, "Fewer rounds")
                            .ghost()
                            .small(),
                    )
                    .clicked()
                {
                    form.set_rounds(form.rounds.saturating_sub(1));
                }
                let response = ui
                    .add(
                        TextField::new(&mut form.rounds_text)
                            .mono()
                            .width(52.0)
                            .error(!(1..=MAX_KDF_ROUNDS).contains(&form.rounds)),
                    )
                    .labelled_by(label);
                if response.changed() {
                    form.rounds = form.rounds_text.trim().parse().unwrap_or(0);
                }
                if ui
                    .add(
                        Button::icon_only(icons::PLUS, "More rounds")
                            .ghost()
                            .small(),
                    )
                    .clicked()
                {
                    form.set_rounds(form.rounds.saturating_add(1));
                }
            });
        });
}

/// A neutral note on an inset strip.
fn info(ui: &mut Ui, text: &str) {
    let p = &Palette::ONE_DARK;
    marks::banner(ui, p.surface, |ui| {
        ui.horizontal_top(|ui| {
            icons::show(ui, icons::INFO, icons::INLINE, p.blue);
            ui.add(egui::Label::new(RichText::new(text).color(p.text_strong)).wrap());
        });
    });
}

/// The 1 px line above a dialog's buttons.
fn separator(ui: &mut Ui) {
    let p = &Palette::ONE_DARK;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, p.selection),
    );
}

/// "Ed25519", "ECDSA P-256"… from the public key's type.
fn key_type(public_line: &str) -> String {
    let kind = public_line.split_whitespace().next().unwrap_or_default();
    match kind {
        "ssh-ed25519" => "Ed25519".into(),
        "ecdsa-sha2-nistp256" => "ECDSA P-256".into(),
        "ecdsa-sha2-nistp384" => "ECDSA P-384".into(),
        "ecdsa-sha2-nistp521" => "ECDSA P-521".into(),
        "ssh-rsa" => "RSA".into(),
        "sk-ssh-ed25519@openssh.com" => "Ed25519-SK".into(),
        "sk-ecdsa-sha2-nistp256@openssh.com" => "ECDSA-SK".into(),
        other => other.to_string(),
    }
}

/// The created key and the next steps; returns whether *Done* was clicked
/// and what the editor or app should do.
fn result(
    ui: &mut Ui,
    form: &mut Form,
    key: &GeneratedKey,
    snapshot: &ConfigSnapshot,
    ssh: Option<&SshInfo>,
    editing: bool,
    commands: &mut Vec<Command>,
) -> (bool, Option<KeyAction>) {
    let p = &Palette::ONE_DARK;
    let mut action = None;
    const KEY: f32 = 136.0;

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let fingerprint = copy::Row {
            name: "fingerprint",
            key: ("Fingerprint", KEY, p.dim),
            key_font: FontId::proportional(13.0),
            value: &key.fingerprint,
            value_color: p.text_strong,
            height: 30.0,
            fill: None,
            hover_fill: p.surface,
            hover_outline: false,
            corners: egui::CornerRadius::same(radius::CONTROL),
        };
        if copy::row(ui, fingerprint) {
            action = Some(KeyAction::Copied("fingerprint".into()));
        }
        let fact = |ui: &mut Ui, name: &str, add: &mut dyn FnMut(&mut Ui)| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let (rect, _) = ui.allocate_exact_size(vec2(KEY - 8.0, 24.0), Sense::hover());
                ui.painter().text(
                    pos2(rect.left(), rect.center().y),
                    Align2::LEFT_CENTER,
                    name,
                    FontId::proportional(13.0),
                    p.dim,
                );
                add(ui);
            });
        };
        fact(ui, "Type", &mut |ui| {
            ui.label(
                RichText::new(key_type(&key.public_line))
                    .font(FontId::monospace(13.0))
                    .color(p.text_strong),
            );
        });
        fact(ui, "Passphrase", &mut |ui| {
            if form.algorithm.is_hardware() {
                ui.label(RichText::new("whatever you set in the terminal").size(13.0));
            } else if key.encrypted {
                ui.label(RichText::new("set").size(13.0).color(p.text_strong));
            } else {
                icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                ui.label(RichText::new("none").size(13.0).color(p.yellow));
            }
        });
        ui.horizontal_top(|ui| {
            ui.add_space(8.0);
            let (rect, _) = ui.allocate_exact_size(vec2(KEY - 8.0, 32.0), Sense::hover());
            ui.painter().text(
                pos2(rect.left(), rect.center().y),
                Align2::LEFT_CENTER,
                "Public key",
                FontId::proportional(13.0),
                p.dim,
            );
            Frame::new()
                .fill(p.bg)
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        let width = ui.available_width() - 32.0;
                        ui.allocate_ui(vec2(width, 0.0), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&key.public_line)
                                        .font(FontId::monospace(12.0))
                                        .color(p.text_strong),
                                )
                                .wrap(),
                            );
                        });
                        let copy = Button::icon_only(icons::COPY, "Copy public key")
                            .ghost()
                            .small();
                        if ui.add(copy).clicked() {
                            ui.ctx().copy_text(key.public_line.clone());
                            action = Some(KeyAction::Copied("public key".into()));
                        }
                    });
                });
        });
    });

    // Next steps.
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(
            RichText::new("NEXT STEPS")
                .font(fonts::semibold(11.0))
                .color(p.dim),
        );
        ui.add_space(6.0);
        let step = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().hline(
                line.x_range(),
                line.center().y,
                Stroke::new(1.0, p.separator),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| add(ui));
            ui.add_space(8.0);
        };
        let explain = |ui: &mut Ui, text: &str, color: Color32| {
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
        };
        let button_column = |ui: &mut Ui, button: Button, enabled: bool| -> egui::Response {
            let (rect, _) = ui.allocate_exact_size(vec2(170.0, 30.0), Sense::hover());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            child.add_enabled(enabled, button)
        };

        // 1. Use in this host.
        step(ui, &mut |ui| {
            let identity_file = form.identity_file(key);
            let tip = if editing {
                "Adds the lines to the host you are editing"
            } else {
                "Open the host in the editor first"
            };
            let use_it = Button::new("Use in this host").small().tip(tip);
            if button_column(ui, use_it, editing).clicked() {
                action = Some(KeyAction::UseInHost {
                    identity_file: identity_file.clone(),
                });
            }
            explain(
                ui,
                "Adds IdentityFile and IdentitiesOnly yes to the editor. Save the host to \
                 write them.",
                p.dim,
            );
        });

        // 2. Add to the agent.
        let service_stopped = ssh
            .and_then(|s| s.service.as_ref())
            .and_then(|s| s.as_ref().ok())
            .is_some_and(|s| !s.is_running());
        step(ui, &mut |ui| {
            let tip = if service_stopped {
                "The agent can't take keys while its service is stopped"
            } else if key.encrypted {
                "Runs ssh-add in a terminal, where you type the passphrase"
            } else {
                "Runs ssh-add"
            };
            let add = Button::new("Add to agent").small().tip(tip);
            if button_column(ui, add, !service_stopped).clicked() {
                commands.push(Command::AddKeyToAgent {
                    path: key.path.clone(),
                    encrypted: key.encrypted,
                });
            }
            if service_stopped {
                ui.label(
                    RichText::new("The ssh-agent service is not running.")
                        .size(12.0)
                        .color(p.yellow),
                );
                if button::link(ui, "How to start it", 12.0).clicked() {
                    action = Some(KeyAction::OpenSettings);
                }
            } else {
                explain(
                    ui,
                    "Loads the key into ssh-agent, so tunnels can use it without asking.",
                    p.dim,
                );
            }
        });

        // 3. Install on the server.
        let saved = !form.alias.is_empty()
            && snapshot
                .hosts()
                .is_some_and(|h| h.find(&form.alias).is_some());
        step(ui, &mut |ui| {
            let tip = if saved {
                "Opens a terminal that adds the key to the server"
            } else {
                "Save the host first"
            };
            let install = Button::new("Install on server").small().tip(tip);
            let response = button_column(ui, install, saved);
            if targets::clicked(ui.ctx(), Target::KeyInstall, &response) && saved {
                commands.push(Command::InstallKey {
                    alias: form.alias.clone(),
                    public_line: key.public_line.clone(),
                });
            }
            explain(
                ui,
                "Opens a terminal and appends the key to ~/.ssh/authorized_keys. Needs a \
                 POSIX shell; the first login usually asks for a password.",
                p.dim,
            );
        });
    });

    separator(ui);
    let mut done = false;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        done = ui.add(Button::new("Done").primary().dialog()).clicked();
    });
    (done, action)
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
            .with_size([900.0, 900.0])
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
        // Picking an algorithm in the dropdown renames the default file.
        harness.get_by_label("Algorithm").click();
        harness.step();
        harness.step();
        harness.get_by_label("RSA 4096").click();
        harness.step();
        harness.step();
        assert!(path(&mut harness).ends_with("id_rsa_web"));
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
        // The eye shows both fields.
        harness.get_by_label("Show passphrase").click();
        harness.step();
        harness.step();
        assert!(form(&mut harness).shown);
        harness.get_by_label("Hide passphrase");

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

    #[test]
    fn the_rounds_stepper_steps_and_checks_typed_values() {
        let mut harness = harness("web", "Host web\n");
        harness.step();
        harness.get_by_label("More rounds").click();
        harness.step();
        assert_eq!(form(&mut harness).rounds, DEFAULT_KDF_ROUNDS + 1);
        harness.get_by_label("Fewer rounds").click();
        harness.step();
        assert_eq!(form(&mut harness).rounds, DEFAULT_KDF_ROUNDS);
        form(&mut harness).rounds = 0;
        harness.step();
        harness.get_by_label(&format!("KDF rounds must be 1 to {MAX_KDF_ROUNDS}."));
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
        // Done loads the result.
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
        harness.get_by_label("Copy fingerprint");
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
        harness.get_by_label("Key created for web");
        harness.get_by_label("Ed25519");

        harness.get_by_label("Use in this host").click();
        harness.step();
        harness.get_by_label("Copy public key").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            [
                KeyAction::UseInHost {
                    identity_file: "~/.ssh/id_ed25519_web".into()
                },
                KeyAction::Copied("public key".into())
            ]
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
        harness.get_by_label("Done").click();
        harness.step();
        assert!(harness.state().dialog.form.is_none());
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

    #[test]
    fn key_types_read_well() {
        assert_eq!(key_type("ssh-ed25519 AAAA x"), "Ed25519");
        assert_eq!(key_type("ecdsa-sha2-nistp384 AAAA"), "ECDSA P-384");
        assert_eq!(key_type("sk-ssh-ed25519@openssh.com AAAA"), "Ed25519-SK");
    }
}
