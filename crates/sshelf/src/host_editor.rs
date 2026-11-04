//! Host editor (SPEC.md §5, §10): a structured form and the raw text of one
//! host block. Both edit the same text, so they never drift apart.
//! Validation (`ssh -G -F`, in core), the problems and the changed lines in
//! a column beside the form (below it in a narrow pane), conflicts with
//! changes made on disk, new hosts with their target file, and the
//! *Rename…* dialog.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::{
    Align, Align2, Color32, FontId, Frame, Id, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2,
    pos2, vec2,
};
use sshconf::diff::{self, DiffOp};
use sshconf::{ConfigFile, FileId, LineKind, edit};
use sshelf_core::config_edit::{Change, Edit, Problem};
use sshelf_core::core::{AppData, Command, ConfigSnapshot, SshInfo};
use sshelf_core::keygen::GeneratedKey;
use sshelf_core::rename;
use sshelf_core::supervisor::{Snapshot, TunnelState};

use crate::key_dialog::{KeyAction, KeyDialog};
use crate::theme::{Palette, radius};
use crate::widgets::button::Button;
use crate::widgets::dropdown::{Choice, Dropdown};
use crate::widgets::field::TextField;
use crate::widgets::{marks, nav, popup};
use crate::{fonts, icons, util};

/// How a form field edits its directive.
#[derive(Clone, Copy)]
enum Field {
    /// One value (quoted when it has spaces), with its placeholder.
    Text(&'static str),
    /// `yes` / `no` / not set.
    YesNo,
    /// Repeatable, one value each.
    List { example: &'static str },
    /// `LocalForward` / `RemoteForward` / `DynamicForward` as separate
    /// fields (SPEC.md §10 *Forwards*).
    Forward(Kind),
}

/// Which forward, for its field labels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Local,
    Remote,
    Dynamic,
}

impl Kind {
    /// Labels of the listen port, the target host and the target port.
    fn labels(self) -> [&'static str; 3] {
        match self {
            Kind::Local => ["Local port", "IP / host on remote machine", "Remote port"],
            Kind::Remote => ["Remote port", "IP / host on this machine", "Local port"],
            Kind::Dynamic => ["Local port", "", ""],
        }
    }
}

/// The structured form (SPEC.md §5), in this order.
const FIELDS: &[(&str, Field)] = &[
    ("HostName", Field::Text("IP or DNS name")),
    ("User", Field::Text("not set")),
    ("Port", Field::Text("22")),
    ("ProxyJump", Field::Text("not set, e.g. bastion")),
    (
        "IdentityFile",
        Field::List {
            example: "~/.ssh/id_ed25519",
        },
    ),
    ("IdentitiesOnly", Field::YesNo),
    ("LocalForward", Field::Forward(Kind::Local)),
    ("RemoteForward", Field::Forward(Kind::Remote)),
    ("DynamicForward", Field::Forward(Kind::Dynamic)),
    ("ServerAliveInterval", Field::Text("not set")),
    ("ForwardAgent", Field::YesNo),
];

/// Form layout: label column, the space after it, the action column.
const LABEL: f32 = 150.0;
const GAP: f32 = 12.0;
const ACTION: f32 = 32.0;
/// The *Problems* / *Changes to write* column, beside the form from this
/// pane width on (SPEC.md §10 *Layout*).
const ASIDE: f32 = 380.0;
const SIDE_BY_SIDE: f32 = 880.0;

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

/// A forward's fields. The bind address is kept as it is (it is edited in
/// *Raw text*).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Forward {
    bind: Option<String>,
    listen: String,
    host: String,
    port: String,
}

fn is_port(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

impl Forward {
    /// `[bind:]port host:hostport` (or `[bind:]port` for `DynamicForward`);
    /// `None` for anything else (a Unix socket, one combined argument),
    /// which stays in *Raw text*.
    fn parse(args: &[String], kind: Kind) -> Option<Self> {
        let (listen, target) = match (kind, args) {
            (Kind::Dynamic, [listen]) => (listen, None),
            (Kind::Local | Kind::Remote, [listen, target]) => (listen, Some(target)),
            _ => return None,
        };
        let (bind, listen) = match listen.rsplit_once(':') {
            Some((bind, port)) => (Some(bind.to_string()), port),
            None => (None, listen.as_str()),
        };
        if !is_port(listen) {
            return None;
        }
        let (host, port) = match target {
            None => (String::new(), String::new()),
            Some(target) => {
                let (host, port) = target.rsplit_once(':')?;
                if host.is_empty() || !is_port(port) {
                    return None;
                }
                let host = host
                    .strip_prefix('[')
                    .and_then(|h| h.strip_suffix(']'))
                    .unwrap_or(host);
                (host.to_string(), port.to_string())
            }
        };
        Some(Self {
            bind,
            listen: listen.to_string(),
            host,
            port,
        })
    }

    fn complete(&self, kind: Kind) -> bool {
        is_port(self.listen.trim())
            && (kind == Kind::Dynamic
                || (!self.host.trim().is_empty() && is_port(self.port.trim())))
    }

    /// The directive's arguments; an IPv6 target gets its brackets back.
    fn args(&self, kind: Kind) -> Vec<String> {
        let listen = match &self.bind {
            Some(bind) => format!("{bind}:{}", self.listen.trim()),
            None => self.listen.trim().to_string(),
        };
        if kind == Kind::Dynamic {
            return vec![listen];
        }
        let host = self.host.trim();
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        vec![listen, format!("{host}:{}", self.port.trim())]
    }
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
    /// Last validation, the text it was for and when.
    checked: Option<(String, Result<Vec<Problem>, String>, SystemTime)>,
    /// What the file holds on disk now, when it changed under the edit.
    conflict: Option<Vec<u8>>,
    /// *Overwrite…* was clicked: the confirmation shows.
    confirm_overwrite: bool,
    /// Form field text while it has focus: (field, row) → text.
    buffers: HashMap<(usize, usize), String>,
    focus: Option<(usize, usize)>,
    /// Forwards being typed that are not complete yet: (field, row) →
    /// fields. Nothing is written until all of them are filled in.
    forwards: HashMap<(usize, usize), Forward>,
    /// A new host: the files it may go to (SPEC.md §4); `None` when editing.
    new: Option<Vec<PathBuf>>,
}

impl Session {
    /// A new host for the main config (or the first writable file).
    fn new_host(snapshot: &ConfigSnapshot) -> Result<Self, String> {
        let targets = snapshot.writable_files();
        let file = targets
            .iter()
            .find(|f| **f == snapshot.path)
            .or(targets.first())
            .cloned()
            .ok_or("no config file can be written")?;
        // The form adds directive lines as they are filled in.
        let text = "Host ".to_string();
        let mut session = Self {
            alias: String::new(),
            file: PathBuf::new(),
            file_id: 0,
            base: Vec::new(),
            lines: 0..0,
            original: String::new(),
            text,
            tab: Tab::Form,
            pending: None,
            checked: None,
            conflict: None,
            confirm_overwrite: false,
            buffers: HashMap::new(),
            focus: None,
            forwards: HashMap::new(),
            new: Some(targets),
        };
        session.target(snapshot, file);
        Ok(session)
    }

    /// Points a new host at `file`, as the snapshot has it.
    fn target(&mut self, snapshot: &ConfigSnapshot, file: PathBuf) {
        let loaded = snapshot
            .set()
            .and_then(|set| set.files.iter().enumerate().find(|(_, f)| f.path == file));
        (self.file_id, self.base) = match loaded {
            Some((id, f)) => (id, f.to_bytes()),
            None => (0, Vec::new()),
        };
        self.file = file;
        self.checked = None;
    }

    /// The alias a new host gets: the first word of its `Host` line.
    fn new_alias(&self) -> String {
        edit::header_aliases(&self.text)
            .and_then(|a| a.into_iter().next())
            .unwrap_or_default()
    }

    /// Why a new host cannot be saved under its name yet.
    fn name_problem(&self, snapshot: &ConfigSnapshot) -> Option<String> {
        self.new.as_ref()?;
        let alias = self.new_alias();
        if alias.is_empty() {
            return Some("Give the host a name.".into());
        }
        let empty = sshconf::HostList::default();
        rename::check_new_alias(&alias, snapshot.hosts().unwrap_or(&empty)).err()
    }

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
            confirm_overwrite: false,
            buffers: HashMap::new(),
            focus: None,
            forwards: HashMap::new(),
            new: None,
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
        let change = if self.new.is_some() {
            Change::Insert {
                text: self.text.clone(),
            }
        } else {
            Change::Replace {
                lines: self.lines.clone(),
                text: self.text.clone(),
            }
        };
        Edit {
            id,
            file: self.file.clone(),
            base: self.base.clone(),
            change,
            alias,
        }
    }

    /// Adds `IdentityFile <file>` (after any existing ones) and
    /// `IdentitiesOnly yes`, so ssh offers just this key (SPEC.md §11).
    fn use_key(&mut self, identity_file: &str) {
        let mut files = edit::values(&self.text, "IdentityFile");
        if !files
            .iter()
            .any(|f| f.first().map(String::as_str) == Some(identity_file))
        {
            files.push(vec![identity_file.to_string()]);
        }
        self.text = edit::set_all(&self.text, "IdentityFile", &files);
        self.text = edit::set(&self.text, "IdentitiesOnly", Some(&["yes".to_string()]));
        // The form must show the new values, not what was typed before.
        self.focus = None;
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

    /// Live problems, plus the last validation's when it is for this text.
    fn all_problems(&self) -> Vec<Problem> {
        let mut shown = self.live_problems();
        if let Some((text, Ok(found), _)) = &self.checked
            && *text == self.text
        {
            for problem in found {
                if !shown.contains(problem) {
                    shown.push(problem.clone());
                }
            }
        }
        shown
    }

    /// The problems on each directive, by lowercase keyword (for the
    /// messages under the form's fields).
    fn problems_by_keyword(&self) -> HashMap<String, Vec<String>> {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let mut by_keyword: HashMap<String, Vec<String>> = HashMap::new();
        for problem in self.all_problems() {
            let Some(raw) = problem.line.and_then(|n| lines.get(n)) else {
                continue;
            };
            let keyword = match LineKind::parse(raw) {
                LineKind::Directive(d) => d.keyword,
                // An invalid line still starts with its keyword.
                _ => raw
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };
            by_keyword
                .entry(keyword.to_ascii_lowercase())
                .or_default()
                .push(problem.message);
        }
        by_keyword
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

    /// Whether `keyword`'s values differ from when editing started.
    fn changed(&self, keyword: &str) -> bool {
        self.new.is_none()
            && edit::values(&self.text, keyword) != edit::values(&self.original, keyword)
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
    /// A new host that was saved, for the host list to select.
    created: Option<String>,
    keys: KeyDialog,
}

impl HostEditor {
    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Alias of the host being edited.
    pub fn alias(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.alias.as_str())
    }

    /// A new host is being written (it has no alias in the list yet).
    pub fn is_new(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.new.is_some())
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

    /// Starts a new host (SPEC.md §4: it goes before the first catch-all block).
    pub fn open_new(&mut self, snapshot: &ConfigSnapshot) {
        match Session::new_host(snapshot) {
            Ok(session) => self.session = Some(session),
            Err(e) => self.status = Some((true, format!("cannot add a host: {e}"))),
        }
    }

    /// The alias of a host that was just added, once.
    pub fn take_created(&mut self) -> Option<String> {
        self.created.take()
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
        if session.new.is_some() {
            // Nothing existing is replaced: the insert point is found again.
            let file = session.file.clone();
            session.target(snapshot, file);
        } else if session.dirty() {
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
            session.checked = Some((text, result, SystemTime::now()));
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
            if session.new.is_some() {
                let alias = session.new_alias();
                self.status = Some((false, format!("Added {alias} to {name}")));
                self.created = Some(alias);
            } else {
                self.status = Some((false, format!("Saved {} in {name}", session.alias)));
            }
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
            session.confirm_overwrite = false;
        }
    }

    /// The editor in place of the detail pane. `pane` is the pane's width
    /// (for the stacking rule); `inset` the fill of inset areas.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &ConfigSnapshot,
        pane: f32,
        inset: Color32,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut close = false;
        let mut reopen = false;
        let mut open_keys = None;
        let id = self.next_id + 1;
        let Some(session) = &mut self.session else {
            return commands;
        };
        let name_problem = session.name_problem(snapshot);

        // The footer first, so the body gets the space above it.
        egui::Panel::bottom("host_editor_footer")
            .show_separator_line(false)
            .frame(Frame::NONE)
            .show(ui, |ui| {
                close = footer(ui, session, name_problem.as_deref(), id, &mut commands);
            });
        if header(ui, session) {
            open_keys = Some(match &session.new {
                Some(_) => session.new_alias(),
                None => session.alias.clone(),
            });
        }
        if let Some(disk) = session.conflict.clone() {
            let (cmds, outcome) = conflict(ui, session, snapshot, &disk, id);
            commands.extend(cmds);
            reopen |= outcome == Resolution::Discard;
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_salt("host_editor")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let side_by_side = pane >= SIDE_BY_SIDE;
                if side_by_side {
                    // Two fixed columns; neither can push the other.
                    let width = ui.available_width();
                    let form_width = (width - ASIDE - 24.0).max(200.0);
                    let top = ui.cursor().min;
                    let column = |ui: &mut Ui, x: f32, w: f32| {
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(egui::Rect::from_min_size(
                                    top + vec2(x, 0.0),
                                    vec2(w, f32::INFINITY),
                                ))
                                .layout(Layout::top_down(Align::Min)),
                        );
                        child.set_width(w);
                        child
                    };
                    let mut form_ui = column(ui, 0.0, form_width);
                    body(&mut form_ui, session, snapshot, inset);
                    let mut aside_ui = column(ui, width - ASIDE, ASIDE);
                    aside(
                        &mut aside_ui,
                        session,
                        snapshot,
                        name_problem.as_deref(),
                        inset,
                    );
                    let height = form_ui
                        .min_rect()
                        .height()
                        .max(aside_ui.min_rect().height());
                    ui.allocate_space(vec2(width, height));
                } else {
                    body(ui, session, snapshot, inset);
                    ui.add_space(16.0);
                    aside(ui, session, snapshot, name_problem.as_deref(), inset);
                }
            });
        if !commands.is_empty() {
            self.next_id = id;
        }
        if reopen && session.new.is_some() {
            self.session = None;
        } else if reopen {
            let alias = session.alias.clone();
            self.session = Session::open(snapshot, &alias).ok();
        } else if close {
            self.session = None;
        }
        if let Some(alias) = open_keys {
            self.open_keys(snapshot, &alias);
        }
        commands
    }

    /// Opens the "Generate key" window for `alias` (also from the host
    /// detail's health banner, without an editor).
    pub fn open_keys(&mut self, snapshot: &ConfigSnapshot, alias: &str) {
        let ssh_dir = snapshot
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        self.keys.open(alias, &ssh_dir);
    }

    pub fn on_key_generated(&mut self, id: u64, result: Result<GeneratedKey, String>) {
        self.keys.on_generated(id, result);
    }

    /// The "Generate key" window, if open; it outlives the editor, so a key
    /// can be installed after the host is saved.
    pub fn show_keys(
        &mut self,
        ctx: &egui::Context,
        snapshot: &ConfigSnapshot,
        ssh: Option<&SshInfo>,
    ) -> Vec<Command> {
        let id = self.next_id + 1;
        let editing = self.session.is_some();
        if let Some(session) = &self.session {
            let alias = match &session.new {
                Some(_) => session.new_alias(),
                None => session.alias.clone(),
            };
            self.keys.follow_alias(&alias);
        }
        let (commands, action) = self.keys.show(ctx, id, snapshot, ssh, editing);
        if !commands.is_empty() {
            self.next_id = id;
        }
        if let (Some(KeyAction::UseInHost { identity_file }), Some(session)) =
            (action, &mut self.session)
        {
            session.use_key(&identity_file);
        }
        commands
    }

    /// The rename dialog, if open (board *Dialog-Rename*): the new alias,
    /// everything else that follows it, and a running tunnel on the host
    /// blocks it up front (SPEC.md §5).
    pub fn show_rename(
        &mut self,
        ctx: &egui::Context,
        snapshot: &ConfigSnapshot,
        data: &AppData,
        states: Option<&Snapshot>,
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
        let also = also_updated(snapshot, data, &dialog.alias);
        let running: Vec<&str> = data
            .tunnels
            .tunnels
            .iter()
            .filter(|t| t.host == dialog.alias)
            .filter(|t| {
                states.is_some_and(|s| {
                    !matches!(
                        s.state(&t.id),
                        TunnelState::Stopped | TunnelState::Failed(_)
                    )
                })
            })
            .map(|t| t.name.as_str())
            .collect();
        let blocked = (!running.is_empty()).then(|| {
            let names = running
                .iter()
                .map(|n| format!("\u{201c}{n}\u{201d}"))
                .collect::<Vec<_>>()
                .join(", ");
            let verb = if running.len() == 1 { "is" } else { "are" };
            format!("{names} {verb} running on this host. Stop it to rename.")
        });
        let mut cancel = false;
        let response = egui::Modal::new(Id::unique("rename_host"))
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
                ui.spacing_mut().item_spacing.y = 14.0;
                ui.label(
                    RichText::new(format!("Rename {}", dialog.alias))
                        .font(fonts::semibold(17.0))
                        .color(p.text_strong),
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let label = ui.label(RichText::new("New alias").size(13.0));
                    let error = check.as_ref().err().filter(|e| !e.is_empty());
                    ui.add(
                        TextField::new(&mut dialog.to)
                            .mono()
                            .dialog(p.bg)
                            .error(error.is_some()),
                    )
                    .labelled_by(label.id);
                    if let Some(e) = error {
                        ui.label(RichText::new(capitalized(e)).size(12.0).color(p.red));
                    }
                });
                Frame::new()
                    .fill(p.bg)
                    .corner_radius(radius::CONTROL)
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.label(
                            RichText::new("Also updated")
                                .font(fonts::semibold(12.0))
                                .color(p.text_strong),
                        );
                        for (what, value) in &also {
                            ui.horizontal(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(100.0, 18.0), Sense::hover());
                                ui.painter().text(
                                    pos2(rect.left(), rect.center().y),
                                    Align2::LEFT_CENTER,
                                    what,
                                    FontId::proportional(13.0),
                                    p.dim,
                                );
                                ui.label(RichText::new(value).size(13.0));
                            });
                        }
                    });
                if let Some(blocked) = &blocked {
                    ui.horizontal_wrapped(|ui| {
                        icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                        ui.label(RichText::new(blocked).size(12.0).color(p.yellow));
                    });
                }
                if let Some(e) = &dialog.error {
                    ui.label(RichText::new(e).size(12.0).color(p.red));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let busy = dialog.pending.is_some();
                    let why_not = blocked.clone().or_else(|| match &check {
                        Err(e) if e.is_empty() => Some("Type a new alias".to_string()),
                        Err(e) => Some(capitalized(e)),
                        Ok(()) => None,
                    });
                    let mut rename = Button::new("Rename").primary().dialog();
                    if busy {
                        rename = rename.busy();
                    }
                    if let Some(why) = &why_not {
                        rename = rename.tip(why);
                    }
                    if ui.add_enabled(why_not.is_none() && !busy, rename).clicked() {
                        dialog.pending = Some(id);
                        dialog.error = None;
                        commands.push(Command::RenameHost {
                            id,
                            alias: dialog.alias.clone(),
                            to: to.clone(),
                        });
                    }
                    cancel = ui.add(Button::new("Cancel").ghost().dialog()).clicked();
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

/// "a host name cannot…" as a sentence.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What a rename of `alias` also changes, as (what, where) rows.
fn also_updated(snapshot: &ConfigSnapshot, data: &AppData, alias: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if let (Some(set), Some(host)) = (snapshot.set(), snapshot.hosts().and_then(|h| h.find(alias)))
    {
        let (short, _) = util::location(set, host.at);
        rows.push(("ssh config".to_string(), format!("{short} · Host line")));
    }
    let (mut hosts, mut tunnels, mut history) = (
        data.hosts.clone(),
        data.tunnels.clone(),
        data.history.clone(),
    );
    let renamed = rename::rename_in(&mut hosts, &mut tunnels, &mut history, alias, "\u{0}");
    if renamed.metadata {
        rows.push(("Notes & tags".into(), "hosts.toml".into()));
    }
    if renamed.tunnels > 0 {
        let names: Vec<&str> = data
            .tunnels
            .tunnels
            .iter()
            .filter(|t| t.host == alias)
            .map(|t| t.name.as_str())
            .collect();
        rows.push(("Tunnels".into(), names.join(", ")));
    }
    match renamed.history {
        0 => {}
        1 => rows.push(("History".into(), "1 launch".into())),
        n => rows.push(("History".into(), format!("{n} launches"))),
    }
    rows
}

/// Width of *Form / Raw text* and *Generate key…* with the gap before
/// them, and the least the title needs beside them.
const HEADER_CONTROLS: f32 = 300.0;
const TITLE_ROOM: f32 = 320.0;

/// One line of `text` cut to `width` (paths in the middle), the full text
/// in a tooltip when it was cut.
fn cut_label(ui: &mut Ui, text: &str, font: FontId, color: Color32, width: f32) {
    let (shown, cut) = crate::widgets::text::elide(ui.painter(), text, &font, width);
    let label = ui.add(
        egui::Label::new(RichText::new(shown).font(font).color(color))
            .selectable(false)
            .sense(Sense::hover()),
    );
    if cut {
        let full = text.to_string();
        popup::tip_below(label, |ui| {
            ui.label(full);
        });
    }
}

/// Title, where it is, *Form* / *Raw text* and *Generate key…*; returns
/// whether *Generate key…* was clicked.
fn header(ui: &mut Ui, session: &mut Session) -> bool {
    let p = &Palette::ONE_DARK;
    let mut generate = false;
    // Beside the title when there is room for both; else on a row of their
    // own below it (the cards' narrow pane).
    let wide = ui.available_width() >= TITLE_ROOM + HEADER_CONTROLS;
    let text_width = if wide {
        ui.available_width() - HEADER_CONTROLS
    } else {
        ui.available_width()
    };
    let title = match &session.new {
        Some(_) => "New host".to_string(),
        None => format!("Editing {}", session.alias),
    };
    let place = session.new.is_none().then(|| {
        format!(
            "{} · lines {}–{}",
            session.file.display(),
            session.lines.start + 1,
            session.lines.end
        )
    });
    let texts = |ui: &mut Ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            cut_label(ui, &title, fonts::semibold(20.0), p.text_strong, text_width);
            if let Some(place) = &place {
                cut_label(ui, place, FontId::monospace(12.0), p.dim, text_width);
            }
        });
    };
    // `right_to_left`: added in reverse, so they read the same either way.
    let mut controls = |ui: &mut Ui, right_to_left: bool| {
        let mut mode = |ui: &mut Ui| {
            let selected = usize::from(session.tab == Tab::Raw);
            let picked = nav::segmented(
                ui,
                Id::unique("host_editor_mode"),
                &["Form", "Raw text"],
                selected,
            );
            match picked {
                Some(0) => session.tab = Tab::Form,
                Some(_) => session.tab = Tab::Raw,
                None => {}
            }
        };
        let key = |ui: &mut Ui| {
            let key = Button::new("Generate key…")
                .icon(icons::KEY_ROUND)
                .small()
                .tip("Create a new key pair for this host");
            ui.add(key).clicked()
        };
        if right_to_left {
            generate = key(ui);
            mode(ui);
        } else {
            mode(ui);
            generate = key(ui);
        }
    };
    if wide {
        ui.horizontal(|ui| {
            texts(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                controls(ui, true)
            });
        });
    } else {
        texts(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| controls(ui, false));
    }
    if session.file_id != 0 {
        ui.label(
            RichText::new(
                "This file is included from your main config. It is validated on its own, so \
                 settings from the files around it are not taken into account.",
            )
            .size(12.0)
            .color(p.dim),
        );
    }
    generate
}

/// The form or the raw text.
fn body(ui: &mut Ui, session: &mut Session, snapshot: &ConfigSnapshot, inset: Color32) {
    // Fields and selects are inset: lighter than a dark pane (the cards').
    ui.visuals_mut().extreme_bg_color = inset;
    match session.tab {
        Tab::Form => form(ui, session, snapshot),
        Tab::Raw => raw(ui, session, inset),
    }
}

/// A form row: the label (with a yellow dot while the value differs from
/// the saved one), then `add` in the rest. Returns the label's id.
fn form_row<R>(
    ui: &mut Ui,
    (label, named_by_field): (&str, bool),
    changed: bool,
    add: impl FnOnce(&mut Ui, Id) -> R,
) -> R {
    let p = &Palette::ONE_DARK;
    // Explicit columns: in a plain horizontal row the field column would
    // only get the width its first frame used.
    let width = ui.available_width();
    let top = ui.cursor().min;
    let mut label_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(top, vec2(LABEL, 32.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    label_ui.spacing_mut().item_spacing.x = 6.0;
    if changed {
        let (rect, response) = label_ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Image, true, "Changed, not saved yet")
        });
        marks::paint_dot(label_ui.painter(), rect.center(), 6.0, p.yellow);
        popup::tip_below(response, |ui| {
            ui.label("Changed, not saved yet");
        });
    }
    let label_id = if named_by_field {
        // A dropdown carries the name itself; a second node with the same
        // text would only be in the way.
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
    let field_width = (width - LABEL - GAP).max(80.0);
    let mut field_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                top + vec2(LABEL + GAP, 0.0),
                vec2(field_width, f32::INFINITY),
            ))
            .layout(Layout::top_down(Align::Min)),
    );
    field_ui.set_width(field_width);
    field_ui.spacing_mut().item_spacing.y = 4.0;
    let inner = add(&mut field_ui, label_id);
    let height = field_ui.min_rect().height().max(32.0);
    ui.allocate_space(vec2(width, height));
    inner
}

/// An error under a field.
fn field_error(ui: &mut Ui, errors: Option<&Vec<String>>) {
    if let Some(message) = errors.and_then(|e| e.first()) {
        ui.label(
            RichText::new(capitalized(message))
                .size(12.0)
                .color(Palette::ONE_DARK.red),
        );
    }
}

fn form(ui: &mut Ui, session: &mut Session, snapshot: &ConfigSnapshot) {
    let p = &Palette::ONE_DARK;
    ui.spacing_mut().item_spacing.y = 8.0;
    let errors = session.problems_by_keyword();
    let field_width = |ui: &Ui| (ui.available_width() - ACTION - GAP).max(80.0);
    if let Some(targets) = session.new.clone() {
        // The `Host` line of a new host; existing hosts use Rename….
        let name_problem = session.name_problem(snapshot);
        form_row(ui, ("Alias", false), false, |ui, label| {
            let key = (usize::MAX, 0);
            let derived = session.new_alias();
            let width = field_width(ui);
            let buffer = session.field_text(key, derived);
            let typed = !buffer.trim().is_empty();
            let response = ui
                .add(
                    TextField::new(buffer)
                        .mono()
                        .hint("the name you type after ssh")
                        .width(width)
                        .error(typed && name_problem.is_some()),
                )
                .labelled_by(label);
            let value = buffer.trim().to_string();
            session.track_focus(key, &response);
            if response.changed() {
                session.text = match session.text.split_once('\n') {
                    Some((_, rest)) => format!("Host {value}\n{rest}"),
                    None => format!("Host {value}"),
                };
            }
            if typed && let Some(problem) = &name_problem {
                ui.label(RichText::new(capitalized(problem)).size(12.0).color(p.red));
            }
        });
        form_row(ui, ("Save to", true), false, |ui, label| {
            let _ = label; // the dropdown is named "Save to" itself
            target_picker(ui, session, snapshot, &targets, field_width(ui));
        });
    }
    for (index, &(keyword, field)) in FIELDS.iter().enumerate() {
        let current = edit::values(&session.text, keyword);
        let changed = session.changed(keyword);
        let errors = errors.get(&keyword.to_ascii_lowercase());
        let named_by_field = matches!(field, Field::YesNo);
        form_row(
            ui,
            (keyword, named_by_field),
            changed,
            |ui, label| match field {
                Field::Text(hint) => {
                    let key = (index, 0);
                    let derived = current.first().map(|a| a.join(" ")).unwrap_or_default();
                    let width = field_width(ui);
                    let buffer = session.field_text(key, derived);
                    let response = ui
                        .add(
                            TextField::new(buffer)
                                .mono()
                                .hint(hint)
                                .width(width)
                                .error(errors.is_some()),
                        )
                        .labelled_by(label);
                    let value = buffer.clone();
                    session.track_focus(key, &response);
                    if response.changed() {
                        session.text = edit::set(&session.text, keyword, words(&value).as_deref());
                    }
                    field_error(ui, errors);
                }
                Field::YesNo => {
                    yes_no(ui, session, keyword, &current);
                    field_error(ui, errors);
                }
                Field::List { example } => {
                    list(ui, session, index, keyword, example, &current, label);
                    field_error(ui, errors);
                }
                Field::Forward(kind) => {
                    forwards(ui, session, (index, keyword, kind), &current, label);
                    field_error(ui, errors);
                }
            },
        );
    }
    ui.add_space(4.0);
    for note in [
        "Forwards are edited as separate fields and written back as one line. The bind address \
         stays as it is; change it in Raw text.",
        "Other directives (Compression, SetEnv…) are kept as they are; edit them in Raw text.",
    ] {
        ui.label(RichText::new(note).size(12.0).color(p.dim));
    }
}

/// `yes` / `no` / not set, as a dropdown. Another value (a ForwardAgent
/// socket path) shows as it is until something else is picked.
fn yes_no(ui: &mut Ui, session: &mut Session, keyword: &str, current: &[Vec<String>]) {
    let value = current.first().map(|a| a.join(" "));
    let selected = match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => 0u8,
        Some("yes") => 1,
        Some("no") => 2,
        Some(_) => 3,
    };
    let trigger = value.clone().unwrap_or_else(|| "not set".into());
    let options = [(0u8, "not set"), (1, "yes"), (2, "no")];
    let picked = Dropdown::new(("yes_no", keyword), keyword, &options, trigger)
        .field(160.0)
        .show(ui, selected);
    if let Some(picked) = picked.filter(|p| *p != selected) {
        let args = match picked {
            1 => Some(vec!["yes".to_string()]),
            2 => Some(vec!["no".to_string()]),
            _ => None,
        };
        session.text = edit::set(&session.text, keyword, args.as_deref());
    }
}

/// A repeatable one-value directive (IdentityFile): a field per value with
/// a remove button, and *Add*.
fn list(
    ui: &mut Ui,
    session: &mut Session,
    index: usize,
    keyword: &str,
    example: &str,
    current: &[Vec<String>],
    label: Id,
) {
    let rows: Vec<String> = current.iter().map(|a| a.join(" ")).collect();
    let mut new_rows: Option<Vec<String>> = None;
    let remove_name = format!("Remove {keyword}");
    for (row, derived) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let key = (index, row);
            let width = (ui.available_width() - ACTION - GAP).max(80.0);
            let buffer = session.field_text(key, derived.clone());
            let mut response = ui.add(TextField::new(buffer).mono().width(width));
            if row == 0 {
                response = response.labelled_by(label);
            }
            let value = buffer.clone();
            session.track_focus(key, &response);
            if response.changed() && !value.trim().is_empty() {
                let mut all = rows.clone();
                all[row] = value;
                new_rows = Some(all);
            }
            let remove = Button::icon_only(icons::X, &remove_name).ghost();
            if ui.add(remove).clicked() {
                let mut all = rows.clone();
                all.remove(row);
                new_rows = Some(all);
                session.buffers.retain(|(f, _), _| *f != index);
                session.focus = None;
            }
        });
    }
    let add = Button::new(format!("Add {keyword}"))
        .icon(icons::PLUS)
        .small();
    if ui.add(add).clicked() {
        let mut all = rows.clone();
        all.push(example.to_string());
        new_rows = Some(all);
    }
    if let Some(all) = new_rows {
        let args: Vec<Vec<String>> = all.iter().filter_map(|r| words(r)).collect();
        session.text = edit::set_all(&session.text, keyword, &args);
    }
}

/// Forwards as separate fields (SPEC.md §10 *Forwards*): *L* local port →
/// IP / host on the remote machine : remote port; *R* mirrored; *D* the
/// local port. A row is written once all its fields are filled in; one that
/// isn't `[bind:]port host:port` stays in *Raw text*.
fn forwards(
    ui: &mut Ui,
    session: &mut Session,
    (index, keyword, kind): (usize, &str, Kind),
    current: &[Vec<String>],
    label: Id,
) {
    let p = &Palette::ONE_DARK;
    let parsed: Vec<Option<Forward>> = current.iter().map(|a| Forward::parse(a, kind)).collect();
    // A row being added sits after the written ones.
    let extra = session.forwards.contains_key(&(index, parsed.len()));
    let count = parsed.len() + usize::from(extra);
    let mut write: Option<Vec<Vec<String>>> = None;
    let [listen_label, host_label, port_label] = kind.labels();
    let remove_name = format!("Remove {keyword}");
    for row in 0..count {
        let key = (index, row);
        let written = parsed.get(row).cloned().flatten();
        let unparsed = row < parsed.len() && written.is_none();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let width = (ui.available_width() - ACTION - GAP).max(120.0);
            ui.allocate_ui_with_layout(vec2(width, 32.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(width);
                if unparsed {
                    let text = current[row].join(" ");
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(text)
                                .font(FontId::monospace(13.0))
                                .color(p.dim),
                        );
                        ui.label(RichText::new("edit in Raw text").size(12.0).color(p.dim));
                    });
                    return;
                }
                let mut fields = session
                    .forwards
                    .get(&key)
                    .cloned()
                    .or(written.clone())
                    .unwrap_or_default();
                let before = fields.clone();
                forward_fields(
                    ui,
                    &mut fields,
                    kind,
                    row == 0,
                    [listen_label, host_label, port_label],
                    label,
                );
                if fields != before {
                    if fields.complete(kind) {
                        session.forwards.remove(&key);
                        let mut all: Vec<Vec<String>> = current.to_vec();
                        if row < all.len() {
                            all[row] = fields.args(kind);
                        } else {
                            all.push(fields.args(kind));
                        }
                        write = Some(all);
                    } else {
                        session.forwards.insert(key, fields.clone());
                    }
                }
                if session.forwards.contains_key(&key) {
                    ui.label(
                        RichText::new("Not written until every field is filled in")
                            .size(12.0)
                            .color(p.yellow),
                    );
                }
                if let Some(bind) = &fields.bind {
                    ui.label(
                        RichText::new(format!("Bind address {bind} is kept"))
                            .size(12.0)
                            .color(p.dim),
                    );
                }
            });
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                let remove = Button::icon_only(icons::X, &remove_name).ghost();
                if ui.add(remove).clicked() {
                    if row < parsed.len() {
                        let mut all: Vec<Vec<String>> = current.to_vec();
                        all.remove(row);
                        write = Some(all);
                    }
                    session.forwards.retain(|(f, _), _| *f != index);
                }
            });
        });
    }
    let add = Button::new(format!("Add {keyword}"))
        .icon(icons::PLUS)
        .small();
    if ui.add_enabled(!extra, add).clicked() {
        session
            .forwards
            .insert((index, parsed.len()), Forward::default());
    }
    if let Some(all) = write {
        session.text = edit::set_all(&session.text, keyword, &all);
    }
}

/// The fields of one forward: `listen → host : port` (just the port for
/// `DynamicForward`), with small labels above the first row.
fn forward_fields(
    ui: &mut Ui,
    fields: &mut Forward,
    kind: Kind,
    labelled: bool,
    labels: [&str; 3],
    row_label: Id,
) {
    let p = &Palette::ONE_DARK;
    const PORT: f32 = 96.0;
    let width = ui.available_width();
    let host_width = (width - PORT * 2.0 - 16.0 - 12.0 - 6.0 * 4.0).max(60.0);
    // Cut to their column, so a narrow form never overlaps them.
    let small = |ui: &mut Ui, text: &str, w: f32| {
        let (rect, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());
        let font = FontId::proportional(11.0);
        let (shown, _) = crate::widgets::text::elide(ui.painter(), text, &font, w);
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            shown,
            font,
            p.dim,
        );
    };
    // Too narrow for one line (a list pane with the problems beside the
    // form): the listen port above, the target below.
    if kind != Kind::Dynamic && host_width < 140.0 {
        if labelled {
            small(ui, labels[0], PORT);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let listen = ui.add(
                TextField::new(&mut fields.listen)
                    .mono()
                    .hint(labels[0])
                    .width(PORT),
            );
            if labelled {
                listen.labelled_by(row_label);
            }
            icons::show(ui, icons::ARROW_RIGHT, 14.0, p.dim);
        });
        let host_width = (width - PORT - 12.0 - 12.0).max(60.0);
        if labelled {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                small(ui, labels[1], host_width);
                small(ui, labels[2], PORT);
            });
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add(
                TextField::new(&mut fields.host)
                    .mono()
                    .hint(labels[1])
                    .width(host_width - 12.0),
            );
            let (colon, _) = ui.allocate_exact_size(vec2(6.0, 32.0), Sense::hover());
            ui.painter().text(
                colon.center(),
                Align2::CENTER_CENTER,
                ":",
                FontId::monospace(13.0),
                p.dim,
            );
            ui.add(
                TextField::new(&mut fields.port)
                    .mono()
                    .hint(labels[2])
                    .width(PORT),
            );
        });
        return;
    }
    if labelled {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            small(ui, labels[0], PORT);
            if kind != Kind::Dynamic {
                small(ui, "", 16.0);
                small(ui, labels[1], host_width);
                small(ui, "", 12.0);
                small(ui, labels[2], PORT);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let listen = ui.add(
            TextField::new(&mut fields.listen)
                .mono()
                .hint(labels[0])
                .width(PORT),
        );
        // The first row's port field carries the row's name.
        if labelled {
            listen.labelled_by(row_label);
        }
        if kind == Kind::Dynamic {
            return;
        }
        let (arrow, _) = ui.allocate_exact_size(vec2(16.0, 32.0), Sense::hover());
        ui.painter().text(
            arrow.center(),
            Align2::CENTER_CENTER,
            icons::ARROW_RIGHT,
            icons::font(14.0),
            p.dim,
        );
        ui.add(
            TextField::new(&mut fields.host)
                .mono()
                .hint(labels[1])
                .width(host_width),
        );
        let (colon, _) = ui.allocate_exact_size(vec2(12.0, 32.0), Sense::hover());
        ui.painter().text(
            colon.center(),
            Align2::CENTER_CENTER,
            ":",
            FontId::monospace(13.0),
            p.dim,
        );
        ui.add(
            TextField::new(&mut fields.port)
                .mono()
                .hint(labels[2])
                .width(PORT),
        );
    });
}

/// The whole block as raw text.
fn raw(ui: &mut Ui, session: &mut Session, inset: Color32) {
    let p = &Palette::ONE_DARK;
    let label = ui.label(
        RichText::new(
            "The whole block as it is written in the file; the form edits the same text.",
        )
        .size(12.0)
        .color(p.dim),
    );
    Frame::new()
        .fill(inset)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(8, 8))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut session.text)
                    .code_editor()
                    .frame(Frame::NONE)
                    .text_color(p.text_strong)
                    .desired_rows(14)
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(label.id);
        });
}

/// Which file a new host goes to (SPEC.md §4, §10 *Dropdown*): every loaded
/// file, two lines each; read-only ones are disabled with their reason.
fn target_picker(
    ui: &mut Ui,
    session: &mut Session,
    snapshot: &ConfigSnapshot,
    targets: &[PathBuf],
    width: f32,
) {
    let mut choices: Vec<Choice<usize>> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    match snapshot.set() {
        Some(set) => {
            for (id, file) in set.files.iter().enumerate() {
                let hosts = snapshot
                    .hosts()
                    .map(|h| h.hosts.iter().filter(|e| e.at.file == id).count())
                    .unwrap_or(0);
                let count = match hosts {
                    1 => "1 host".to_string(),
                    n => format!("{n} hosts"),
                };
                let origin = match set.included_from.get(id).copied().flatten() {
                    None => "main config".to_string(),
                    Some(at) => format!("included from {}", util::location(set, at).0),
                };
                choices.push(Choice {
                    value: id,
                    label: file.path.display().to_string(),
                    detail: Some(format!("{origin} · {count}")),
                    disabled: snapshot
                        .read_only(id)
                        .filter(|_| !targets.contains(&file.path))
                        .map(|r| format!("read-only: {r}")),
                });
                paths.push(file.path.clone());
            }
        }
        None => {
            choices.push(Choice {
                value: 0,
                label: session.file.display().to_string(),
                detail: Some("main config · created with this host".into()),
                disabled: None,
            });
            paths.push(session.file.clone());
        }
    }
    let selected = paths.iter().position(|p| *p == session.file).unwrap_or(0);
    let trigger = session.file.display().to_string();
    let picked = Dropdown::with_choices("new_host_file", "Save to", choices, trigger)
        .field(width)
        .mono()
        .show(ui, selected);
    if let Some(picked) = picked
        && let Some(path) = paths.get(picked)
        && *path != session.file
    {
        session.target(snapshot, path.clone());
    }
}

/// Splits a form value into its argument; `None` when it is empty.
fn words(value: &str) -> Option<Vec<String>> {
    let value = value.trim();
    (!value.is_empty()).then(|| vec![value.to_string()])
}

/// The column beside (or below) the form: problems, then the changed lines
/// (where a new host goes; the disk's changes during a conflict).
fn aside(
    ui: &mut Ui,
    session: &Session,
    snapshot: &ConfigSnapshot,
    name_problem: Option<&str>,
    inset: Color32,
) {
    ui.spacing_mut().item_spacing.y = 12.0;
    problems(ui, session, name_problem);
    if let Some(disk) = &session.conflict {
        let base = String::from_utf8_lossy(&session.base).into_owned();
        let now = String::from_utf8_lossy(disk).into_owned();
        let old: Vec<&str> = base.lines().collect();
        let new: Vec<&str> = now.lines().collect();
        let changed = count_changed(&old, &new);
        card(
            ui,
            inset,
            "Disk vs. when you started",
            &lines_count(changed),
            |ui| {
                diff_view(ui, &old, &new, 1);
            },
        );
    } else if session.new.is_some() {
        where_it_goes(ui, session, snapshot, inset);
    } else {
        changes(ui, session, inset);
    }
}

/// A titled card; `note` on the right of the title.
fn card(ui: &mut Ui, fill: Color32, title: &str, note: &str, add: impl FnOnce(&mut Ui)) {
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(fill)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(title)
                        .font(fonts::semibold(13.0))
                        .color(p.text_strong),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(note).size(11.0).color(p.dim));
                });
            });
            add(ui);
        });
}

/// Problems: invalid lines at once, ssh's after *Check* / *Save*.
fn problems(ui: &mut Ui, session: &Session, name_problem: Option<&str>) {
    let p = &Palette::ONE_DARK;
    let line_of = |n: usize| session.lines.start + n + 1;
    // A typed alias that can't be used is explained under its field.
    let name_problem = name_problem.filter(|_| session.new_alias().is_empty());
    let mut shown = session.all_problems();
    // An empty `Host` line is the name problem, already explained.
    if name_problem.is_some() {
        shown.retain(|p| p.line != Some(0));
    }
    let checked = session
        .checked
        .as_ref()
        .filter(|(text, ..)| *text == session.text);
    let note = match checked {
        Some((_, _, at)) => format!(
            "checked {} with ssh -G -F",
            chrono::DateTime::<chrono::Local>::from(*at).format("%H:%M")
        ),
        None => String::new(),
    };
    card(ui, p.surface, "Problems", &note, |ui| {
        let item = |ui: &mut Ui, icon: char, color: Color32, text: &str| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                icons::show(ui, icon, icons::INLINE, color);
                ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
            });
        };
        if let Some(problem) = name_problem {
            item(ui, icons::TRIANGLE_ALERT, p.yellow, problem);
        }
        if let Some((_, Err(e), _)) = checked {
            item(ui, icons::CIRCLE_X, p.red, e);
        }
        for problem in &shown {
            let text = match problem.line {
                Some(n) if session.new.is_some() => {
                    format!("line {} of the new host: {}", n + 1, problem.message)
                }
                Some(n) => format!("line {}: {}", line_of(n), problem.message),
                None => problem.message.clone(),
            };
            item(ui, icons::CIRCLE_X, p.red, &text);
        }
        if shown.is_empty() && name_problem.is_none() {
            match checked {
                Some((_, Ok(_), _)) => {
                    item(ui, icons::CIRCLE_CHECK, p.green, "ssh accepts this config.");
                }
                Some((_, Err(_), _)) => {}
                None => {
                    ui.label(
                        RichText::new("Check runs ssh -G -F on the edited file; Save does too.")
                            .size(12.0)
                            .color(p.dim),
                    );
                }
            }
        }
    });
}

fn lines_count(n: usize) -> String {
    match n {
        1 => "1 line".into(),
        n => format!("{n} lines"),
    }
}

fn count_changed(old: &[&str], new: &[&str]) -> usize {
    diff::diff(old, new)
        .iter()
        .filter(|op| matches!(op, DiffOp::Added { .. }))
        .count()
        .max(
            diff::diff(old, new)
                .iter()
                .filter(|op| matches!(op, DiffOp::Removed { .. }))
                .count(),
        )
}

/// The changed lines of the block, as they will be written.
fn changes(ui: &mut Ui, session: &Session, inset: Color32) {
    let p = &Palette::ONE_DARK;
    let old: Vec<&str> = split(&session.original);
    let new: Vec<&str> = split(&session.text);
    let changed = count_changed(&old, &new);
    let note = if session.dirty() {
        lines_count(changed)
    } else {
        String::new()
    };
    card(ui, inset, "Changes to write", &note, |ui| {
        if !session.dirty() {
            ui.label(RichText::new("No changes yet.").size(12.0).color(p.dim));
            return;
        }
        diff_view(ui, &old, &new, session.lines.start + 1);
        ui.label(
            RichText::new("Everything else in the file stays byte-for-byte.")
                .size(11.0)
                .color(p.dim),
        );
    });
}

/// A new host: where the block goes in its file, with the lines around it.
fn where_it_goes(ui: &mut Ui, session: &Session, snapshot: &ConfigSnapshot, inset: Color32) {
    let p = &Palette::ONE_DARK;
    let file = snapshot
        .set()
        .and_then(|set| set.files.get(session.file_id))
        .filter(|f| f.path == session.file);
    card(ui, inset, "Where it goes", "", |ui| {
        let Some(file) = file else {
            ui.label(
                RichText::new("The file does not exist yet; it is created with this host.")
                    .size(12.0)
                    .color(p.dim),
            );
            return;
        };
        let (new_file, inserted) = file.insert_block(&session.text);
        let note = if inserted.end < new_file.lines.len() {
            "Before the first `Host *` / `Match all` block, so its defaults don't override this host."
        } else {
            "At the end of the file."
        };
        ui.label(RichText::new(note).size(12.0).color(p.dim));
        let text =
            |f: &ConfigFile| -> Vec<String> { f.lines.iter().map(|l| l.raw.clone()).collect() };
        let (old_lines, new_lines) = (text(file), text(&new_file));
        let old: Vec<&str> = old_lines.iter().map(String::as_str).collect();
        let new: Vec<&str> = new_lines.iter().map(String::as_str).collect();
        diff_view(ui, &old, &new, 1);
    });
}

fn split(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

/// `-`/`+` lines with line numbers and two lines of context, on tinted rows.
fn diff_view(ui: &mut Ui, old: &[&str], new: &[&str], first_line: usize) {
    let p = &Palette::ONE_DARK;
    let ops = diff::diff(old, new);
    ui.spacing_mut().item_spacing.y = 0.0;
    for (k, range) in diff::hunks(&ops, 2).into_iter().enumerate() {
        if k > 0 {
            ui.label(
                RichText::new("…")
                    .font(FontId::monospace(12.0))
                    .color(p.dim),
            );
        }
        for op in &ops[range] {
            let (sign, number, text, color, fill) = match *op {
                DiffOp::Same { old: o, new: n } => (" ", first_line + n, old[o], p.dim, None),
                DiffOp::Removed { old: o } => {
                    ("-", first_line + o, old[o], p.red, Some(p.red_tint))
                }
                DiffOp::Added { new: n } => {
                    ("+", first_line + n, new[n], p.green, Some(p.green_tint))
                }
            };
            let line = format!("{sign}{number:>4}  {text}");
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            if let Some(fill) = fill {
                ui.painter().rect_filled(rect, radius::ROW, fill);
            }
            let galley = ui
                .painter()
                .layout_no_wrap(line, FontId::monospace(12.0), color);
            let clip = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
            clip.galley(
                pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
        }
    }
}

#[derive(PartialEq, Eq)]
enum Resolution {
    None,
    Discard,
}

/// The conflict banner: the file changed on disk; reload, re-apply the
/// edit (when this block itself didn't change), or overwrite after a
/// confirmation.
fn conflict(
    ui: &mut Ui,
    session: &mut Session,
    snapshot: &ConfigSnapshot,
    disk: &[u8],
    id: u64,
) -> (Vec<Command>, Resolution) {
    let p = &Palette::ONE_DARK;
    let mut commands = Vec::new();
    let mut outcome = Resolution::None;
    // The snapshot is reloaded right after a conflict; until then, wait.
    let current = snapshot
        .set()
        .and_then(|set| set.files.iter().find(|f| f.path == session.file))
        .map(|f| f.to_bytes())
        .unwrap_or_default();
    let reloaded = current == disk;
    let fresh = match &session.new {
        // A new host only adds lines: it can always move to the new version.
        Some(_) => {
            let mut fresh = Session::new_host(snapshot).ok();
            if let Some(f) = &mut fresh {
                f.target(snapshot, session.file.clone());
                f.lines = session.lines.clone();
                f.original = session.original.clone();
            }
            fresh
        }
        None => Session::open(snapshot, &session.alias).ok(),
    };
    let rebase = fresh.filter(|f| reloaded && f.original == session.original);
    ui.add_space(8.0);
    marks::banner(ui, p.yellow_tint, |ui| {
        ui.horizontal_wrapped(|ui| {
            icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
            ui.label(
                RichText::new("The file changed on disk")
                    .font(fonts::semibold(13.0))
                    .color(p.yellow),
            );
            ui.label(
                RichText::new("while you were editing. Nothing was written.")
                    .size(13.0)
                    .color(p.dim),
            );
        });
        ui.horizontal_wrapped(|ui| {
            let reload = Button::new("Discard my changes").small().tip(if reloaded {
                "Reload the block from disk"
            } else {
                "Reloading…"
            });
            if ui.add_enabled(reloaded, reload).clicked() {
                outcome = Resolution::Discard;
            }
            let apply = Button::new("Apply my changes to the new version")
                .small()
                .warning()
                .tip(if rebase.is_some() {
                    "Your edit goes onto the file as it is now"
                } else {
                    "This host's block changed on disk too"
                });
            if ui.add_enabled(rebase.is_some(), apply).clicked()
                && let Some(fresh) = rebase
            {
                session.base = fresh.base;
                session.lines = fresh.lines;
                session.conflict = None;
                session.checked = None;
            }
            if !session.confirm_overwrite {
                let overwrite = Button::new("Overwrite…").small().ghost();
                if ui
                    .add_enabled(session.pending.is_none(), overwrite)
                    .clicked()
                {
                    session.confirm_overwrite = true;
                }
            }
        });
        if session.confirm_overwrite {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Write your version over the changes on disk?")
                        .color(p.text_strong),
                );
                // The safe way is the main button (SPEC.md §10).
                if ui.add(Button::new("Keep them").primary().small()).clicked() {
                    session.confirm_overwrite = false;
                }
                if ui.add(Button::new("Overwrite").danger().small()).clicked() {
                    session.pending = Some((id, Pending::Save, session.text.clone()));
                    commands.push(Command::SaveEdit {
                        edit: session.edit(id),
                        overwrite: true,
                    });
                    session.conflict = None;
                    session.confirm_overwrite = false;
                }
            });
        }
    });
    (commands, outcome)
}

/// Save (or *Add host*) / Check / Discard and why Save is unavailable;
/// returns whether the editor closes.
fn footer(
    ui: &mut Ui,
    session: &mut Session,
    name_problem: Option<&str>,
    id: u64,
    commands: &mut Vec<Command>,
) -> bool {
    let p = &Palette::ONE_DARK;
    let mut close = false;
    let line = ui.available_rect_before_wrap();
    ui.painter()
        .hline(line.x_range(), line.top(), Stroke::new(1.0, p.selection));
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let busy = session.pending.as_ref().map(|(_, kind, _)| *kind);
        let problems = session.live_problems().len();
        let blocked = name_problem.is_some() || problems > 0;
        let reason = if name_problem.is_some() {
            Some(if session.new_alias().is_empty() {
                "Give the host a name to add it".to_string()
            } else {
                "Choose another alias".to_string()
            })
        } else if problems > 0 {
            Some(match problems {
                1 => "Fix 1 problem to save".to_string(),
                n => format!("Fix {n} problems to save"),
            })
        } else if session.conflict.is_some() {
            Some("Resolve the file change first".to_string())
        } else {
            None
        };
        let can_save = session.dirty() && busy.is_none() && !blocked && session.conflict.is_none();
        let label = if session.new.is_some() {
            "Add host"
        } else {
            "Save"
        };
        let mut save = Button::new(label).primary().tip(
            reason
                .as_deref()
                .unwrap_or("Checks the edit with ssh, then writes only the changed lines"),
        );
        if busy == Some(Pending::Save) {
            save = save.busy();
        }
        if ui.add_enabled(can_save, save).clicked() {
            session.pending = Some((id, Pending::Save, session.text.clone()));
            commands.push(Command::SaveEdit {
                edit: session.edit(id),
                overwrite: false,
            });
        }
        let mut check =
            Button::new("Check").tip("Runs ssh -G -F on the edited file without saving");
        if busy == Some(Pending::Check) {
            check = check.busy();
        }
        if ui.add_enabled(busy.is_none() && !blocked, check).clicked() {
            session.pending = Some((id, Pending::Check, session.text.clone()));
            commands.push(Command::CheckEdit(session.edit(id)));
        }
        let close_label = match (&session.new, session.dirty()) {
            (Some(_), _) => "Cancel",
            (None, true) => "Discard",
            (None, false) => "Close",
        };
        close = ui
            .add_enabled(busy.is_none(), Button::new(close_label).ghost())
            .clicked();
        match &reason {
            Some(reason) if session.dirty() || session.new.is_some() => {
                ui.label(RichText::new(reason).size(12.0).color(p.red));
            }
            _ if session.dirty() => {
                ui.label(RichText::new("unsaved changes").size(12.0).color(p.dim));
            }
            _ => {}
        }
    });
    ui.add_space(16.0);
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
    use crate::test_util;

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
        states: Option<Snapshot>,
        commands: Vec<Command>,
    }

    fn sized(text: &str, width: f32) -> Harness<'static, State> {
        let snapshot = snapshot(text);
        let mut editor = HostEditor::default();
        editor.open(&snapshot, "web");
        assert!(editor.is_open());
        test_util::harness()
            .with_size([width, 900.0])
            .build_ui_state(
                move |ui, state: &mut State| {
                    let commands =
                        state
                            .editor
                            .show(ui, &state.snapshot, width, Palette::ONE_DARK.bg_dark);
                    state.commands.extend(commands);
                    let commands = state.editor.show_rename(
                        ui.ctx(),
                        &state.snapshot,
                        &state.data,
                        state.states.as_ref(),
                    );
                    state.commands.extend(commands);
                    let commands = state.editor.show_keys(ui.ctx(), &state.snapshot, None);
                    state.commands.extend(commands);
                },
                State {
                    editor,
                    snapshot,
                    data: AppData::new(PathBuf::from("app")),
                    states: None,
                    commands: Vec::new(),
                },
            )
    }

    fn harness(text: &str) -> Harness<'static, State> {
        sized(text, 960.0)
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
        harness.get_by_label("1 line");
        harness.get_by_label("Changed, not saved yet");
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
    fn yes_no_fields_are_dropdowns() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        harness.get_by_label("ForwardAgent").click();
        steps(&mut harness);
        harness.get_by_label("yes").click();
        steps(&mut harness);
        assert!(text(&harness).contains("    ForwardAgent yes"));
    }

    #[test]
    fn invalid_lines_block_save_and_ssh_problems_are_shown() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        harness.state_mut().editor.session.as_mut().unwrap().text =
            "Host web\n    HostName \"open\n".into();
        steps(&mut harness);
        harness.get_by_label("line 2: unterminated quote");
        // Under its field too.
        harness.get_by_label("Unterminated quote");
        harness.get_by_label("Fix 1 problem to save");
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
        harness.get_by_label_contains("with ssh -G -F");
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
        harness.get_by_label("Disk vs. when you started");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
        // Overwriting asks first; the safe answer is the main one.
        harness.get_by_label("Overwrite…").click();
        steps(&mut harness);
        harness.get_by_label("Keep them").click();
        steps(&mut harness);
        assert!(harness.query_by_label("Overwrite").is_none());
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
    fn the_problems_column_sits_beside_the_form_only_in_a_wide_pane() {
        for (width, beside) in [(960.0, true), (860.0, false)] {
            let mut harness = sized(CONFIG, width);
            steps(&mut harness);
            let form = harness.get_by_label("HostName").rect();
            let problems = harness.get_by_label("Problems").rect();
            assert_eq!(
                problems.left() > form.right(),
                beside,
                "{width}: {form:?} / {problems:?}"
            );
            assert_eq!(problems.top() > form.bottom(), !beside, "{width}");
        }
    }

    #[test]
    fn a_narrow_pane_puts_the_header_controls_below_the_title() {
        for (width, beside) in [(960.0, true), (520.0, false)] {
            let mut harness = sized(CONFIG, width);
            steps(&mut harness);
            let title = harness.get_by_label("Editing web").rect();
            let form = harness.get_by_label("Form").rect();
            assert_eq!(form.top() > title.bottom(), !beside, "{width}");
            assert!(form.right() <= width, "{width}: {form:?}");
        }
    }

    #[test]
    fn forwards_are_edited_as_separate_fields() {
        let config = "Host web\n    LocalForward 0.0.0.0:6443 127.0.0.1:6443\n    RemoteForward /tmp/s localhost:80\n";
        let mut harness = harness(config);
        steps(&mut harness);
        // A socket forward stays in Raw text.
        harness.get_by_label("edit in Raw text");
        harness.get_by_label("Bind address 0.0.0.0 is kept");
        // A new forward is only written once complete.
        harness.get_by_label("Add DynamicForward").click();
        steps(&mut harness);
        harness.get_by_label("Not written until every field is filled in");
        assert!(!text(&harness).contains("DynamicForward"));
        harness.get_by_label("DynamicForward").click();
        harness.step();
        harness.get_by_label("DynamicForward").type_text("1080");
        steps(&mut harness);
        assert!(text(&harness).contains("    DynamicForward 1080"));
        assert!(
            harness
                .query_by_label("Not written until every field is filled in")
                .is_none()
        );
    }

    #[test]
    fn forward_values_parse_and_join() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let local = Forward::parse(&args(&["6443", "127.0.0.1:6443"]), Kind::Local).unwrap();
        assert_eq!(
            local,
            Forward {
                bind: None,
                listen: "6443".into(),
                host: "127.0.0.1".into(),
                port: "6443".into(),
            }
        );
        assert_eq!(local.args(Kind::Local), args(&["6443", "127.0.0.1:6443"]));
        // A bind address and an IPv6 target round-trip.
        let v6 = Forward::parse(&args(&["[::1]:8080", "[2001:db8::1]:80"]), Kind::Remote).unwrap();
        assert_eq!(v6.bind.as_deref(), Some("[::1]"));
        assert_eq!(v6.host, "2001:db8::1");
        assert_eq!(
            v6.args(Kind::Remote),
            args(&["[::1]:8080", "[2001:db8::1]:80"])
        );
        let dynamic = Forward::parse(&args(&["localhost:1080"]), Kind::Dynamic).unwrap();
        assert_eq!(dynamic.args(Kind::Dynamic), args(&["localhost:1080"]));
        // Not port + host:port: stays raw.
        assert!(Forward::parse(&args(&["/tmp/s", "localhost:80"]), Kind::Local).is_none());
        assert!(Forward::parse(&args(&["8080:localhost:80"]), Kind::Local).is_none());
        assert!(Forward::parse(&args(&["8080", "localhost"]), Kind::Local).is_none());
        // Incomplete fields are not written.
        let mut typing = local.clone();
        typing.port.clear();
        assert!(!typing.complete(Kind::Local));
    }

    fn new_host_harness(snapshot: Arc<ConfigSnapshot>) -> Harness<'static, State> {
        let mut editor = HostEditor::default();
        editor.open_new(&snapshot);
        assert!(editor.is_new());
        test_util::harness()
            .with_size([960.0, 900.0])
            .build_ui_state(
                |ui, state: &mut State| {
                    let commands =
                        state
                            .editor
                            .show(ui, &state.snapshot, 960.0, Palette::ONE_DARK.bg_dark);
                    state.commands.extend(commands);
                },
                State {
                    editor,
                    snapshot,
                    data: AppData::new(PathBuf::from("app")),
                    states: None,
                    commands: Vec::new(),
                },
            )
    }

    #[test]
    fn a_new_host_needs_a_free_name_and_is_inserted() {
        let snapshot = snapshot(CONFIG);
        let path = snapshot.path.clone();
        let mut harness = new_host_harness(snapshot);
        steps(&mut harness);
        harness.get_by_label("New host");
        harness.get_by_label("Give the host a name.");
        assert!(
            harness
                .get_by_label("Add host")
                .accesskit_node()
                .is_disabled()
        );

        harness.get_by_label("Alias").click();
        harness.step();
        harness.get_by_label("Alias").type_text("web");
        steps(&mut harness);
        harness.get_by_label("`web` is already a host");
        assert!(
            harness
                .get_by_label("Add host")
                .accesskit_node()
                .is_disabled()
        );

        harness.get_by_label("Alias").type_text("2");
        harness.get_by_label("HostName").click();
        harness.step();
        harness.get_by_label("HostName").type_text("web2.example");
        steps(&mut harness);
        assert_eq!(text(&harness), "Host web2\n    HostName web2.example");
        // It goes before `Host *`.
        harness.get_by_label_contains("Before the first `Host *`");
        harness.get_by_label("Add host").click();
        steps(&mut harness);
        let edit = saved_edit(&harness);
        assert_eq!(edit.file, path);
        assert_eq!(edit.alias, "web2");
        assert_eq!(
            edit.change,
            Change::Insert {
                text: "Host web2\n    HostName web2.example".into()
            }
        );
        harness.state_mut().editor.on_saved(edit.id);
        assert_eq!(
            harness.state_mut().editor.take_created().as_deref(),
            Some("web2")
        );
        assert_eq!(
            harness.state_mut().editor.take_status(),
            Some((false, "Added web2 to config".into()))
        );
    }

    #[test]
    fn a_new_host_can_go_to_an_included_file_but_not_a_read_only_one() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        fs::create_dir_all(tmp.path().join("conf.d")).unwrap();
        fs::write(&config, "Include conf.d/*\nHost *\n    User me\n").unwrap();
        fs::write(tmp.path().join("conf.d").join("work"), "Host w\n").unwrap();
        fs::write(tmp.path().join("conf.d").join("locked"), "Host l\n").unwrap();
        let mut opts = ResolveOptions::new(tmp.path());
        opts.ssh_dir = tmp.path().to_path_buf();
        let set = ConfigSet::load(&config, &opts).unwrap();
        assert_eq!(set.files.len(), 3);
        let read_only = set
            .files
            .iter()
            .map(|f| {
                f.path
                    .ends_with("locked")
                    .then(|| "the file is read-only".to_string())
            })
            .collect();
        let hosts = HostList::from_set(&set);
        let snapshot = Arc::new(ConfigSnapshot {
            generation: 1,
            path: config.clone(),
            state: ConfigState::Loaded { set, hosts },
            read_only,
        });
        let targets = snapshot.writable_files();
        assert_eq!(targets.len(), 2);
        assert!(!targets.iter().any(|t| t.ends_with("locked")));

        let mut harness = new_host_harness(Arc::clone(&snapshot));
        steps(&mut harness);
        assert_eq!(
            harness.state().editor.session.as_ref().unwrap().file,
            config
        );
        // Every file is listed; the read-only one can't be picked.
        harness.get_by_label("Save to").click();
        steps(&mut harness);
        let locked = harness.get_by_label_contains("locked");
        assert!(locked.accesskit_node().is_disabled());
        harness.get_by_label_contains("work").click();
        steps(&mut harness);
        let session = harness.state().editor.session.as_ref().unwrap();
        assert!(session.file.ends_with("work"));
        assert_eq!(session.base, b"Host w\n");
        // Globs expand sorted: config, conf.d/locked, conf.d/work.
        assert_eq!(session.file_id, 2);
        harness.get_by_label_contains("validated on its own");
    }

    #[test]
    fn a_missing_config_is_created_by_the_first_host() {
        let snapshot = Arc::new(ConfigSnapshot {
            generation: 1,
            path: PathBuf::from("home/.ssh/config"),
            state: ConfigState::Missing,
            read_only: Vec::new(),
        });
        let mut harness = new_host_harness(snapshot);
        steps(&mut harness);
        harness.get_by_label("The file does not exist yet; it is created with this host.");
        let session = harness.state().editor.session.as_ref().unwrap();
        assert_eq!(session.file, PathBuf::from("home/.ssh/config"));
        assert!(session.base.is_empty());
    }

    #[test]
    fn a_generated_key_can_be_used_in_the_host() {
        let mut harness = harness(CONFIG);
        steps(&mut harness);
        harness.get_by_label("Generate key…").click();
        steps(&mut harness);
        harness.get_by_label("Generate").click();
        steps(&mut harness);
        let Some(Command::GenerateKey { id, request }) = harness.state().commands.last().cloned()
        else {
            panic!("no key requested");
        };
        assert!(request.path.ends_with("id_ed25519_web"), "{request:?}");
        let key = sshelf_core::keygen::GeneratedKey {
            public_path: sshelf_core::keygen::public_path(&request.path),
            path: request.path,
            public_line: "ssh-ed25519 AAAA web (sshelf)".into(),
            fingerprint: "SHA256:x".into(),
            encrypted: false,
        };
        harness.state_mut().editor.on_key_generated(id, Ok(key));
        steps(&mut harness);
        harness.get_by_label("Use in this host").click();
        steps(&mut harness);
        assert_eq!(
            text(&harness),
            "Host web\n    HostName old.example\n    IdentityFile ~/.ssh/id_ed25519_web\n    IdentitiesOnly yes\n"
        );
        // Saving is the editor's usual Save.
        harness.get_by_label("2 lines");
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
        harness.get_by_label("Also updated");
        harness.get_by_label("hosts.toml");
        harness.get_by_label("config:1 · Host line");
        // Unchanged name: nothing to do.
        assert!(
            harness
                .get_by_label("Rename")
                .accesskit_node()
                .is_disabled()
        );
        harness.get_by_label("New alias").click();
        harness.step();
        harness.get_by_label("New alias").type_text("-2");
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

    #[test]
    fn a_running_tunnel_blocks_the_rename_up_front() {
        use std::collections::BTreeMap;

        use sshelf_core::supervisor::TunnelStatus;
        use sshelf_core::tunnels::Tunnel;

        let mut harness = harness("Host web\nHost db\n");
        harness.state_mut().data.tunnels.tunnels.push(Tunnel {
            id: "t-1".into(),
            name: "dev cluster".into(),
            host: "web".into(),
            forwards: Vec::new(),
            auto_reconnect: false,
        });
        let mut tunnels = BTreeMap::new();
        tunnels.insert(
            "t-1".to_string(),
            TunnelStatus {
                state: TunnelState::Running,
                owners: Default::default(),
                log: Default::default(),
                pid: None,
                failure: None,
                reconnect: None,
            },
        );
        harness.state_mut().states = Some(Snapshot { tunnels });
        harness.state_mut().editor.open_rename("web");
        harness.state_mut().editor.rename.as_mut().unwrap().to = "www".into();
        steps(&mut harness);
        harness.get_by_label("dev cluster");
        harness.get_by_label_contains("is running on this host. Stop it to rename.");
        assert!(
            harness
                .get_by_label("Rename")
                .accesskit_node()
                .is_disabled()
        );
    }
}
