//! Host editor (SPEC.md §5): a structured form and the raw text of one host
//! block. Both edit the same text, so they never drift apart. Validation
//! (`ssh -G -F`, in core), a preview of the changed lines, conflicts with
//! changes made on disk, and the *Rename…* dialog.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

use egui::RichText;
use sshconf::diff::{self, DiffOp};
use sshconf::{FileId, LineKind, edit};
use sshelf_core::config_edit::{Change, Edit, Problem};
use sshelf_core::core::{AppData, Command, ConfigSnapshot};
use sshelf_core::rename;

use crate::theme::Palette;

/// How a form field edits its directive.
#[derive(Clone, Copy)]
enum Field {
    /// One value (quoted when it has spaces).
    Text,
    /// `yes` / `no` / not set.
    YesNo,
    /// Repeatable; `split` for `port host:port` style values.
    List { split: bool, example: &'static str },
}

/// The structured form (SPEC.md §5), in this order.
const FIELDS: &[(&str, Field)] = &[
    ("HostName", Field::Text),
    ("User", Field::Text),
    ("Port", Field::Text),
    ("ProxyJump", Field::Text),
    (
        "IdentityFile",
        Field::List {
            split: false,
            example: "~/.ssh/id_ed25519",
        },
    ),
    ("IdentitiesOnly", Field::YesNo),
    (
        "LocalForward",
        Field::List {
            split: true,
            example: "8080 localhost:80",
        },
    ),
    (
        "RemoteForward",
        Field::List {
            split: true,
            example: "8080 localhost:80",
        },
    ),
    (
        "DynamicForward",
        Field::List {
            split: false,
            example: "1080",
        },
    ),
    ("ServerAliveInterval", Field::Text),
    ("ForwardAgent", Field::YesNo),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Form,
    Raw,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Check,
    Save,
}

/// One open edit of one host block.
struct Session {
    /// Primary alias of the host being edited.
    alias: String,
    file: PathBuf,
    file_id: FileId,
    /// The file's bytes when editing started.
    base: Vec<u8>,
    /// The block's lines in `base`.
    lines: Range<usize>,
    original: String,
    text: String,
    tab: Tab,
    /// Request in flight: id, kind, and the text it was sent for.
    pending: Option<(u64, Pending, String)>,
    /// Last validation and the text it was for.
    checked: Option<(String, Result<Vec<Problem>, String>)>,
    /// What the file holds on disk now, when it changed under the edit.
    conflict: Option<Vec<u8>>,
    overwrite_confirmed: bool,
    /// Form field text while it has focus: (field, row) → text.
    buffers: HashMap<(usize, usize), String>,
    focus: Option<(usize, usize)>,
}

impl Session {
    /// Opens `alias`'s own block as the snapshot has it.
    fn open(snapshot: &ConfigSnapshot, alias: &str) -> Result<Self, String> {
        let set = snapshot.set().ok_or("the ssh config is not loaded")?;
        let host = snapshot
            .hosts()
            .and_then(|h| h.find(alias))
            .ok_or_else(|| format!("no host `{alias}`"))?;
        if let Some(reason) = snapshot.read_only(host.at.file) {
            return Err(reason.to_string());
        }
        let file = &set.files[host.at.file];
        let block = file
            .block_at(host.at.line)
            .ok_or("the host's block was not found")?;
        let text = file.text(block.lines.clone());
        Ok(Self {
            alias: host.primary().to_string(),
            file: file.path.clone(),
            file_id: host.at.file,
            base: file.to_bytes(),
            lines: block.lines,
            original: text.clone(),
            text,
            tab: Tab::Form,
            pending: None,
            checked: None,
            conflict: None,
            overwrite_confirmed: false,
            buffers: HashMap::new(),
            focus: None,
        })
    }

    fn dirty(&self) -> bool {
        self.text != self.original
    }

    fn edit(&self, id: u64) -> Edit {
        // Validate under the alias the block now names (it may be changed).
        let alias = edit::header_aliases(&self.text)
            .and_then(|a| a.into_iter().next())
            .unwrap_or_else(|| self.alias.clone());
        Edit {
            id,
            file: self.file.clone(),
            base: self.base.clone(),
            change: Change::Replace {
                lines: self.lines.clone(),
                text: self.text.clone(),
            },
            alias,
        }
    }

    /// Invalid lines of the current text (instant, no ssh).
    fn live_problems(&self) -> Vec<Problem> {
        self.text
            .split('\n')
            .enumerate()
            .filter_map(|(i, raw)| match LineKind::parse(raw) {
                LineKind::Invalid(invalid) => Some(Problem {
                    line: Some(i),
                    message: invalid.reason,
                }),
                _ => None,
            })
            .collect()
    }

    /// Text of a form field while it is not being typed in.
    fn field_text(&mut self, key: (usize, usize), derived: String) -> &mut String {
        if self.focus != Some(key) {
            self.buffers.insert(key, derived);
        }
        self.buffers.entry(key).or_default()
    }

    fn track_focus(&mut self, key: (usize, usize), response: &egui::Response) {
        if response.has_focus() {
            self.focus = Some(key);
        } else if self.focus == Some(key) {
            self.focus = None;
        }
    }
}

/// The rename dialog for one alias.
struct RenameDialog {
    alias: String,
    to: String,
    pending: Option<u64>,
    error: Option<String>,
}

#[derive(Default)]
pub struct HostEditor {
    session: Option<Session>,
    rename: Option<RenameDialog>,
    next_id: u64,
    /// For the status bar: (is error, text).
    status: Option<(bool, String)>,
}

impl HostEditor {
    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Alias of the host being edited.
    pub fn alias(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.alias.as_str())
    }

    pub fn is_dirty(&self) -> bool {
        self.session.as_ref().is_some_and(Session::dirty)
    }

    pub fn close(&mut self) {
        self.session = None;
    }

    pub fn open(&mut self, snapshot: &ConfigSnapshot, alias: &str) {
        match Session::open(snapshot, alias) {
            Ok(session) => self.session = Some(session),
            Err(e) => self.status = Some((true, format!("cannot edit {alias}: {e}"))),
        }
    }

    pub fn open_rename(&mut self, alias: &str) {
        self.rename = Some(RenameDialog {
            alias: alias.to_string(),
            to: alias.to_string(),
            pending: None,
            error: None,
        });
    }

    /// The new alias of a rename that is being saved (the host list keeps
    /// it selected when the old one disappears).
    pub fn renaming_to(&self) -> Option<&str> {
        self.rename
            .as_ref()
            .filter(|r| r.pending.is_some())
            .map(|r| r.to.trim())
    }

    pub fn take_status(&mut self) -> Option<(bool, String)> {
        self.status.take()
    }

    /// A reload: follow it when there are no edits, else show the conflict.
    pub fn on_config(&mut self, snapshot: &ConfigSnapshot) {
        let Some(session) = &mut self.session else {
            return;
        };
        if matches!(session.pending, Some((_, Pending::Save, _))) {
            return; // our own save; EditSaved or EditConflict follows
        }
        let now = snapshot
            .set()
            .and_then(|set| set.files.iter().find(|f| f.path == session.file))
            .map(|f| f.to_bytes())
            .unwrap_or_default();
        if now == session.base {
            return;
        }
        if session.dirty() {
            session.conflict = Some(now);
        } else {
            let tab = session.tab;
            self.session = Session::open(snapshot, &session.alias).ok();
            if let Some(session) = &mut self.session {
                session.tab = tab;
            }
        }
    }

    pub fn on_checked(&mut self, id: u64, result: Result<Vec<Problem>, String>) {
        if let Some(dialog) = &mut self.rename
            && dialog.pending == Some(id)
        {
            dialog.pending = None;
            dialog.error = Some(match result {
                Ok(problems) => problems
                    .iter()
                    .map(|p| p.message.clone())
                    .collect::<Vec<_>>()
                    .join("; "),
                Err(e) => e,
            });
            return;
        }
        if let Some(session) = &mut self.session
            && let Some((_, _, text)) = session.pending.take_if(|(p, ..)| *p == id)
        {
            session.checked = Some((text, result));
        }
    }

    pub fn on_saved(&mut self, id: u64) {
        if let Some(dialog) = self.rename.take_if(|d| d.pending == Some(id)) {
            self.status = Some((
                false,
                format!("Renamed {} to {}", dialog.alias, dialog.to.trim()),
            ));
            return;
        }
        if let Some(session) = self
            .session
            .take_if(|s| s.pending.as_ref().is_some_and(|(p, ..)| *p == id))
        {
            let name = session
                .file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.status = Some((false, format!("Saved {} in {name}", session.alias)));
        }
    }

    pub fn on_conflict(&mut self, id: u64, disk: &[u8]) {
        if let Some(dialog) = &mut self.rename
            && dialog.pending == Some(id)
        {
            dialog.pending = None;
            dialog.error = Some("the file changed on disk; try again".into());
            return;
        }
        if let Some(session) = &mut self.session
            && session.pending.take_if(|(p, ..)| *p == id).is_some()
        {
            session.conflict = Some(disk.to_vec());
            session.overwrite_confirmed = false;
        }
    }

    /// The editor pane.
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &ConfigSnapshot) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut close = false;
        let mut reopen = false;
        let id = self.next_id + 1;
        let Some(session) = &mut self.session else {
            return commands;
        };
        let p = &Palette::ONE_DARK;
        egui::ScrollArea::vertical()
            .id_salt("host_editor")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new(format!("Editing {}", session.alias))
                            .color(p.accent)
                            .strong(),
                    );
                });
                ui.label(RichText::new(session.file.display().to_string()).color(p.dim));
                if session.file_id != 0 {
                    ui.label(
                        RichText::new(
                            "This file is included from your main config. It is validated on \
                             its own, so settings from the files around it are not taken into account.",
                        )
                        .small()
                        .color(p.dim),
                    );
                }
                if let Some(disk) = session.conflict.clone() {
                    let (cmds, outcome) = conflict(ui, session, snapshot, &disk, id);
                    commands.extend(cmds);
                    reopen |= outcome == Resolution::Discard;
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut session.tab, Tab::Form, "Form");
                    ui.selectable_value(&mut session.tab, Tab::Raw, "Raw text");
                });
                ui.add_space(4.0);
                match session.tab {
                    Tab::Form => form(ui, session),
                    Tab::Raw => raw(ui, session),
                }
                ui.separator();
                problems(ui, session);
                changes(ui, session);
                ui.separator();
                close = buttons(ui, session, id, &mut commands);
            });
        if !commands.is_empty() {
            self.next_id = id;
        }
        if reopen {
            let alias = session.alias.clone();
            self.session = Session::open(snapshot, &alias).ok();
        } else if close {
            self.session = None;
        }
        commands
    }

    /// The rename modal, if open.
    pub fn show_rename(
        &mut self,
        ctx: &egui::Context,
        snapshot: &ConfigSnapshot,
        data: &AppData,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let id = self.next_id + 1;
        let Some(dialog) = &mut self.rename else {
            return commands;
        };
        let p = &Palette::ONE_DARK;
        let to = dialog.to.trim().to_string();
        let check = match snapshot.hosts() {
            Some(_) if to == dialog.alias => Err(String::new()),
            Some(hosts) => rename::check_new_alias(&to, hosts),
            None => Err("the ssh config is not loaded".into()),
        };
        let impact = {
            let (mut hosts, mut tunnels, mut history) = (
                data.hosts.clone(),
                data.tunnels.clone(),
                data.history.clone(),
            );
            rename::rename_in(&mut hosts, &mut tunnels, &mut history, &dialog.alias, "x").describe()
        };
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("rename_host")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading(format!("Rename {}", dialog.alias));
            ui.horizontal(|ui| {
                let label = ui.label("New name");
                ui.add(egui::TextEdit::singleline(&mut dialog.to).desired_width(260.0))
                    .labelled_by(label.id);
            });
            match &impact {
                Some(what) => ui.label(format!("Also updated: {what}.")),
                None => ui.label(RichText::new("No sshelf data refers to it.").color(p.dim)),
            };
            if let Err(e) = &check
                && !e.is_empty()
            {
                ui.colored_label(p.yellow, e);
            }
            if let Some(e) = &dialog.error {
                ui.colored_label(p.red, e);
            }
            ui.horizontal(|ui| {
                cancel = ui.button("Cancel").clicked();
                let busy = dialog.pending.is_some();
                if ui
                    .add_enabled(check.is_ok() && !busy, egui::Button::new("Rename"))
                    .clicked()
                {
                    dialog.pending = Some(id);
                    dialog.error = None;
                    commands.push(Command::RenameHost {
                        id,
                        alias: dialog.alias.clone(),
                        to: to.clone(),
                    });
                }
                if busy {
                    ui.spinner();
                }
            });
        });
        if !commands.is_empty() {
            self.next_id = id;
        }
        if cancel || (response.should_close() && dialog.pending.is_none()) {
            self.rename = None;
        }
        commands
    }
}

/// Splits a form value into arguments; `None` when it is empty.
fn words(value: &str, split: bool) -> Option<Vec<String>> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if split {
        Some(value.split_whitespace().map(str::to_string).collect())
    } else {
        Some(vec![value.to_string()])
    }
}

fn form(ui: &mut egui::Ui, session: &mut Session) {
    egui::Grid::new("host_form")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for (index, &(keyword, field)) in FIELDS.iter().enumerate() {
                let label = ui.label(keyword);
                let current = edit::values(&session.text, keyword);
                match field {
                    Field::Text => {
                        let key = (index, 0);
                        let derived = current.first().map(|a| a.join(" ")).unwrap_or_default();
                        let buffer = session.field_text(key, derived);
                        let response = ui
                            .add(egui::TextEdit::singleline(buffer).desired_width(320.0))
                            .labelled_by(label.id);
                        let value = buffer.clone();
                        session.track_focus(key, &response);
                        if response.changed() {
                            session.text =
                                edit::set(&session.text, keyword, words(&value, false).as_deref());
                        }
                    }
                    Field::YesNo => {
                        let value = current
                            .first()
                            .and_then(|a| a.first())
                            .map(|v| v.to_ascii_lowercase());
                        let mut chosen = value.clone();
                        let shown = value.clone().unwrap_or_else(|| "(not set)".into());
                        egui::ComboBox::from_id_salt(keyword)
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut chosen, None, "(not set)");
                                ui.selectable_value(&mut chosen, Some("yes".into()), "yes");
                                ui.selectable_value(&mut chosen, Some("no".into()), "no");
                            })
                            .response
                            .labelled_by(label.id);
                        if chosen != value {
                            let args = chosen.map(|v| vec![v]);
                            session.text = edit::set(&session.text, keyword, args.as_deref());
                        }
                    }
                    Field::List { split, example } => {
                        list(ui, session, index, keyword, split, example, &current);
                    }
                }
                ui.end_row();
            }
        });
}

fn list(
    ui: &mut egui::Ui,
    session: &mut Session,
    index: usize,
    keyword: &str,
    split: bool,
    example: &str,
    current: &[Vec<String>],
) {
    let rows: Vec<String> = current.iter().map(|a| a.join(" ")).collect();
    let mut new_rows: Option<Vec<String>> = None;
    ui.vertical(|ui| {
        for (row, derived) in rows.iter().enumerate() {
            ui.horizontal(|ui| {
                let key = (index, row);
                let buffer = session.field_text(key, derived.clone());
                let response = ui
                    .add(egui::TextEdit::singleline(buffer).desired_width(290.0))
                    .on_hover_text(keyword);
                let value = buffer.clone();
                session.track_focus(key, &response);
                if response.changed() && !value.trim().is_empty() {
                    let mut all = rows.clone();
                    all[row] = value;
                    new_rows = Some(all);
                }
                if ui.small_button("✖").on_hover_text("Remove").clicked() {
                    let mut all = rows.clone();
                    all.remove(row);
                    new_rows = Some(all);
                    session.buffers.retain(|(f, _), _| *f != index);
                    session.focus = None;
                }
            });
        }
        if ui
            .small_button(format!("+ Add {keyword}"))
            .on_hover_text(format!("Adds `{keyword} {example}` to edit"))
            .clicked()
        {
            let mut all = rows.clone();
            all.push(example.to_string());
            new_rows = Some(all);
        }
    });
    if let Some(all) = new_rows {
        let args: Vec<Vec<String>> = all.iter().filter_map(|r| words(r, split)).collect();
        session.text = edit::set_all(&session.text, keyword, &args);
    }
}

fn raw(ui: &mut egui::Ui, session: &mut Session) {
    let label = ui.label(
        RichText::new(
            "The whole block as it is written in the file; the form edits the same text.",
        )
        .small()
        .color(Palette::ONE_DARK.dim),
    );
    ui.add(
        egui::TextEdit::multiline(&mut session.text)
            .code_editor()
            .desired_rows(14)
            .desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
}

/// Live problems, plus the last validation if it is for the current text.
fn problems(ui: &mut egui::Ui, session: &Session) {
    let p = &Palette::ONE_DARK;
    let line_of = |n: usize| session.lines.start + n + 1;
    let mut shown: Vec<Problem> = session.live_problems();
    match &session.checked {
        Some((text, Ok(found))) if *text == session.text => {
            for problem in found {
                if !shown.contains(problem) {
                    shown.push(problem.clone());
                }
            }
            if shown.is_empty() {
                ui.colored_label(p.green, "ssh accepts this config.");
            }
        }
        Some((text, Err(e))) if *text == session.text => {
            ui.colored_label(p.red, e);
        }
        _ => {}
    }
    for problem in &shown {
        let text = match problem.line {
            Some(n) => format!("line {}: {}", line_of(n), problem.message),
            None => problem.message.clone(),
        };
        ui.colored_label(p.red, text);
    }
}

/// The changed lines of the block, as they will be written.
fn changes(ui: &mut egui::Ui, session: &Session) {
    if !session.dirty() {
        return;
    }
    let old: Vec<&str> = split(&session.original);
    let new: Vec<&str> = split(&session.text);
    let changed = diff::diff(&old, &new)
        .iter()
        .filter(|op| op.is_change())
        .count();
    egui::CollapsingHeader::new(format!("Changes ({changed} lines)"))
        .default_open(true)
        .show(ui, |ui| diff_view(ui, &old, &new, session.lines.start + 1));
}

fn split(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

/// `-`/`+` lines with line numbers and two lines of context.
fn diff_view(ui: &mut egui::Ui, old: &[&str], new: &[&str], first_line: usize) {
    let p = &Palette::ONE_DARK;
    let ops = diff::diff(old, new);
    for (k, range) in diff::hunks(&ops, 2).into_iter().enumerate() {
        if k > 0 {
            ui.label(RichText::new("...").monospace().color(p.dim));
        }
        for op in &ops[range] {
            let (sign, number, text, color) = match *op {
                DiffOp::Same { old: o, new: n } => (" ", first_line + n, old[o], p.dim),
                DiffOp::Removed { old: o } => ("-", first_line + o, old[o], p.red),
                DiffOp::Added { new: n } => ("+", first_line + n, new[n], p.green),
            };
            ui.label(
                RichText::new(format!("{sign}{number:>5}  {text}"))
                    .monospace()
                    .color(color),
            );
        }
    }
}

#[derive(PartialEq, Eq)]
enum Resolution {
    None,
    Discard,
}

/// The conflict banner: what changed on disk, and the three ways out.
fn conflict(
    ui: &mut egui::Ui,
    session: &mut Session,
    snapshot: &ConfigSnapshot,
    disk: &[u8],
    id: u64,
) -> (Vec<Command>, Resolution) {
    let p = &Palette::ONE_DARK;
    let mut commands = Vec::new();
    let mut outcome = Resolution::None;
    ui.colored_label(
        p.yellow,
        RichText::new("⚠ The file changed on disk since you started editing.").strong(),
    );
    let base = String::from_utf8_lossy(&session.base).into_owned();
    let now = String::from_utf8_lossy(disk).into_owned();
    let lines = |t: &str| t.lines().map(str::to_string).collect::<Vec<_>>();
    let (base_lines, now_lines) = (lines(&base), lines(&now));
    let old: Vec<&str> = base_lines.iter().map(String::as_str).collect();
    let new: Vec<&str> = now_lines.iter().map(String::as_str).collect();
    egui::CollapsingHeader::new("Changes on disk")
        .default_open(true)
        .show(ui, |ui| diff_view(ui, &old, &new, 1));

    // The snapshot is reloaded right after a conflict; until then, wait.
    let current = snapshot
        .set()
        .and_then(|set| set.files.iter().find(|f| f.path == session.file))
        .map(|f| f.to_bytes())
        .unwrap_or_default();
    let reloaded = current == disk;
    let fresh = Session::open(snapshot, &session.alias).ok();
    let rebase = fresh.filter(|f| reloaded && f.original == session.original);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(reloaded, egui::Button::new("Discard my changes"))
            .on_disabled_hover_text("Reloading…")
            .clicked()
        {
            outcome = Resolution::Discard;
        }
        let apply = ui
            .add_enabled(
                rebase.is_some(),
                egui::Button::new("Apply my changes to the new version"),
            )
            .on_disabled_hover_text("This host's block changed on disk too");
        if apply.clicked()
            && let Some(fresh) = rebase
        {
            session.base = fresh.base;
            session.lines = fresh.lines;
            session.conflict = None;
            session.checked = None;
        }
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut session.overwrite_confirmed,
            "Overwrite the changes on disk with my version",
        );
        if ui
            .add_enabled(
                session.overwrite_confirmed && session.pending.is_none(),
                egui::Button::new(RichText::new("Overwrite").color(p.red)),
            )
            .clicked()
        {
            session.pending = Some((id, Pending::Save, session.text.clone()));
            commands.push(Command::SaveEdit {
                edit: session.edit(id),
                overwrite: true,
            });
            session.conflict = None;
        }
    });
    (commands, outcome)
}

/// Check / Save / Close; returns whether the editor closes.
fn buttons(ui: &mut egui::Ui, session: &mut Session, id: u64, commands: &mut Vec<Command>) -> bool {
    let p = &Palette::ONE_DARK;
    let mut close = false;
    ui.horizontal(|ui| {
        let busy = session.pending.is_some();
        let blocked = !session.live_problems().is_empty();
        let can_save = session.dirty() && !busy && !blocked && session.conflict.is_none();
        if ui
            .add_enabled(can_save, egui::Button::new("Save"))
            .on_hover_text("Checks the edit with ssh, then writes only the changed lines")
            .on_disabled_hover_text(if blocked {
                "Fix the problems first"
            } else {
                "Nothing to save"
            })
            .clicked()
        {
            session.pending = Some((id, Pending::Save, session.text.clone()));
            commands.push(Command::SaveEdit {
                edit: session.edit(id),
                overwrite: false,
            });
        }
        if ui
            .add_enabled(!busy && !blocked, egui::Button::new("Check"))
            .on_hover_text("Runs ssh -G on the edited file without saving")
            .clicked()
        {
            session.pending = Some((id, Pending::Check, session.text.clone()));
            commands.push(Command::CheckEdit(session.edit(id)));
        }
        let close_label = if session.dirty() {
            "Discard changes"
        } else {
            "Close"
        };
        close = ui
            .add_enabled(!busy, egui::Button::new(close_label))
            .clicked();
        if busy {
            ui.spinner();
        }
        if session.dirty() {
            ui.label(RichText::new("unsaved changes").small().color(p.yellow));
        }
    });
    close
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshconf::{ConfigSet, HostList, ResolveOptions};
    use sshelf_core::core::ConfigState;
    use sshelf_core::metadata::{HostMeta, HostsFile};

    use super::*;

    const CONFIG: &str = "Host web\n    HostName old.example\n\nHost *\n    User me\n";

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
            read_only: vec![None],
        })
    }

    struct State {
        editor: HostEditor,
        snapshot: Arc<ConfigSnapshot>,
        data: AppData,
        commands: Vec<Command>,
    }

    fn harness(text: &str) -> Harness<'static, State> {
        let snapshot = snapshot(text);
        let mut editor = HostEditor::default();
        editor.open(&snapshot, "web");
        assert!(editor.is_open());
        Harness::builder().with_size([900.0, 900.0]).build_ui_state(
            |ui, state: &mut State| {
                let commands = state.editor.show(ui, &state.snapshot);
                state.commands.extend(commands);
                let commands = state
                    .editor
                    .show_rename(ui.ctx(), &state.snapshot, &state.data);
                state.commands.extend(commands);
            },
            State {
                editor,
                snapshot,
                data: AppData::new(PathBuf::from("app")),
                commands: Vec::new(),
            },
        )
    }

    fn text(harness: &Harness<'_, State>) -> String {
        harness
            .state()
            .editor
            .session
            .as_ref()
            .unwrap()
            .text
            .clone()
    }

    fn saved_edit(harness: &Harness<'_, State>) -> Edit {
        match harness.state().commands.last() {
            Some(Command::SaveEdit {
                edit,
                overwrite: false,
            }) => edit.clone(),
            other => panic!("expected a save, got {other:?}"),
        }
    }

    fn steps(harness: &mut Harness<'_, State>) {
        for _ in 0..3 {
            harness.step();
        }
    }

    #[test]
    fn the_form_edits_the_block_text_and_save_sends_it() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
        harness.get_by_label("User").click();
        harness.step();
        harness.get_by_label("User").type_text("admin");
        steps(&mut harness);
        assert_eq!(
            text(&harness),
            "Host web\n    HostName old.example\n    User admin\n"
        );
        harness.get_by_label("Changes (1 lines)");
        harness.get_by_label("Save").click();
        steps(&mut harness);
        let edit = saved_edit(&harness);
        assert_eq!(edit.alias, "web");
        assert_eq!(edit.base, CONFIG.as_bytes());
        assert_eq!(
            edit.change,
            Change::Replace {
                lines: 0..3,
                text: "Host web\n    HostName old.example\n    User admin\n".into()
            }
        );
        // The raw text shows the same edit.
        harness.get_by_label("Raw text").click();
        steps(&mut harness);
        let raw = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        assert!(raw.accesskit_node().value().unwrap().contains("User admin"));
    }

    #[test]
    fn invalid_lines_block_save_and_ssh_problems_are_shown() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        harness.state_mut().editor.session.as_mut().unwrap().text =
            "Host web\n    HostName \"open\n".into();
        steps(&mut harness);
        harness.get_by_label("line 2: unterminated quote");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());

        harness.state_mut().editor.session.as_mut().unwrap().text =
            "Host web\n    HostName old.example\n    Bogus 1\n".into();
        steps(&mut harness);
        harness.get_by_label("Save").click();
        steps(&mut harness);
        let id = saved_edit(&harness).id;
        harness.state_mut().editor.on_checked(
            id,
            Ok(vec![Problem {
                line: Some(2),
                message: "Bad configuration option: bogus".into(),
            }]),
        );
        steps(&mut harness);
        harness.get_by_label("line 3: Bad configuration option: bogus");
        // Still open, nothing saved.
        assert!(harness.state().editor.is_open());
        harness.state_mut().editor.on_saved(id + 100);
        assert!(harness.state().editor.is_open());
    }

    #[test]
    fn a_save_closes_the_editor_with_a_status() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        harness.state_mut().editor.session.as_mut().unwrap().text =
            "Host web\n    HostName new.example\n".into();
        steps(&mut harness);
        harness.get_by_label("Save").click();
        steps(&mut harness);
        let id = saved_edit(&harness).id;
        harness.state_mut().editor.on_saved(id);
        assert!(!harness.state().editor.is_open());
        assert_eq!(
            harness.state_mut().editor.take_status(),
            Some((false, "Saved web in config".into()))
        );
    }

    #[test]
    fn changes_on_disk_follow_or_conflict() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        // No edits yet: the editor follows the file.
        let changed = CONFIG.replace("old.example", "other.example");
        harness.state_mut().snapshot = snapshot(&changed);
        let new = Arc::clone(&harness.state().snapshot);
        harness.state_mut().editor.on_config(&new);
        assert!(text(&harness).contains("other.example"));

        // With edits: a conflict with the three choices.
        harness.state_mut().editor.session.as_mut().unwrap().text =
            "Host web\n    HostName other.example\n    Port 2\n".into();
        let changed_again = format!("# a comment\n{changed}");
        harness.state_mut().snapshot = snapshot(&changed_again);
        let newer = Arc::clone(&harness.state().snapshot);
        harness.state_mut().editor.on_config(&newer);
        steps(&mut harness);
        harness.get_by_label("Discard my changes");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
        // The block itself is unchanged on disk, so the edit can move over.
        harness
            .get_by_label("Apply my changes to the new version")
            .click();
        steps(&mut harness);
        assert!(harness.query_by_label("Discard my changes").is_none());
        harness.get_by_label("Save").click();
        steps(&mut harness);
        let edit = saved_edit(&harness);
        assert_eq!(edit.base, changed_again.as_bytes());
        assert!(matches!(edit.change, Change::Replace { ref lines, .. } if *lines == (1..4)));
    }

    #[test]
    fn rename_dialog_checks_the_name_and_sends_the_rename() {
        let mut harness = harness("Host web\nHost db\n");
        harness.state_mut().data.hosts = {
            let mut hosts = HostsFile::default();
            hosts.hosts.insert(
                "web".into(),
                HostMeta {
                    favorite: true,
                    ..HostMeta::default()
                },
            );
            hosts
        };
        harness.state_mut().editor.open_rename("web");
        steps(&mut harness);
        harness.get_by_label("Also updated: metadata.");
        // Unchanged name: nothing to do.
        assert!(
            harness
                .get_by_label("Rename")
                .accesskit_node()
                .is_disabled()
        );
        harness.get_by_label("New name").click();
        harness.step();
        harness.get_by_label("New name").type_text("-2");
        steps(&mut harness);
        assert_eq!(harness.state().editor.rename.as_ref().unwrap().to, "web-2");
        assert!(
            !harness
                .get_by_label("Rename")
                .accesskit_node()
                .is_disabled()
        );
        harness.state_mut().editor.rename.as_mut().unwrap().to = "-x".into();
        steps(&mut harness);
        assert!(
            harness
                .get_by_label("Rename")
                .accesskit_node()
                .is_disabled()
        );
        harness.state_mut().editor.rename.as_mut().unwrap().to = "db".into();
        steps(&mut harness);
        harness.get_by_label("`db` is already a host");
        harness.state_mut().editor.rename.as_mut().unwrap().to = "www".into();
        steps(&mut harness);
        harness.get_by_label("Rename").click();
        steps(&mut harness);
        let Some(Command::RenameHost { id, alias, to }) = harness.state().commands.last().cloned()
        else {
            panic!("no rename sent");
        };
        assert_eq!((alias.as_str(), to.as_str()), ("web", "www"));
        assert_eq!(harness.state().editor.renaming_to(), Some("www"));
        harness.state_mut().editor.on_saved(id);
        assert_eq!(
            harness.state_mut().editor.take_status(),
            Some((false, "Renamed web to www".into()))
        );
    }
}
