//! Host detail (SPEC.md §4, §10): the title with the favourite star and
//! where the host is defined, its actions, route and health (a tinted
//! banner with the fix for any trouble), and three tabs: *Overview*
//! (connection values to copy, notes, tags and colour), *Effective config*
//! (`ssh -G`) and the host's tunnels.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Layout, Margin, Rect,
    Response, RichText, Role, Sense, Stroke, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use sshconf::{ConfigSet, HostEntry, HostList};
use sshelf_core::core::{AppData, Command, ConfigSnapshot, RouteHop, SshInfo};
use sshelf_core::health::{Health, HealthSnapshot};
use sshelf_core::hints;
use sshelf_core::metadata::{Color, HostMeta};
use sshelf_core::ssh::{AgentStatus, Resolved};
use sshelf_core::storage::LoadStatus;
use sshelf_core::supervisor::{Snapshot, TunnelState};
use sshelf_core::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Tunnel};

use crate::theme::{Palette, SWATCHES, radius, timing};
use crate::tour::targets::{self, Target};
use crate::tunnels_view::state_label;
use crate::widgets::button::{self, Button};
use crate::widgets::color::{self, Picking};
use crate::widgets::{copy, marks, nav, popup, text};
use crate::{fonts, icons, motion, util};

/// `ssh -G` keys shown first, in this order.
const IMPORTANT: &[&str] = &[
    "hostname",
    "user",
    "port",
    "proxyjump",
    "proxycommand",
    "identityfile",
    "identitiesonly",
    "identityagent",
    "forwardagent",
    "localforward",
    "remoteforward",
    "dynamicforward",
    "serveraliveinterval",
];

/// The connection values of *Overview*: `ssh -G` key and its label. Only
/// the first `identityfile` (ssh lists its defaults after it).
const CONNECTION: &[(&str, &str)] = &[
    ("hostname", "HostName"),
    ("user", "User"),
    ("port", "Port"),
    ("identityfile", "IdentityFile"),
    ("proxyjump", "ProxyJump"),
    ("localforward", "LocalForward"),
    ("remoteforward", "RemoteForward"),
    ("dynamicforward", "DynamicForward"),
];

/// Below this width the detail takes its narrow form (the cards' pane): a
/// 20 px title and the *Overview* cards stacked.
const NARROW: f32 = 600.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Tab {
    #[default]
    Overview,
    Config,
    Tunnels,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Overview, Tab::Config, Tab::Tunnels];
}

/// What the detail shows besides the host itself.
pub struct Shown<'a> {
    pub config: &'a ConfigSnapshot,
    pub data: &'a AppData,
    pub ssh: Option<&'a SshInfo>,
    pub health: Option<&'a HealthSnapshot>,
    pub states: Option<&'a Snapshot>,
    /// Fill of inset areas (notes, the config table): set off from the pane.
    pub inset: Color32,
}

/// An `ssh -G` result of one config generation, and when it came.
type ResolvedAt = (u64, Result<Resolved, String>, SystemTime);

#[derive(Default)]
pub struct HostDetail {
    /// The host the notes draft belongs to.
    for_alias: Option<String>,
    /// Its metadata: as saved, or as last sent while that write is on its
    /// way (so the star or a swatch doesn't flick back for a frame).
    base: HostMeta,
    sent: Option<HostMeta>,
    notes: String,
    /// The "+ tag" field while open, and whether it got focus yet.
    new_tag: Option<(String, bool)>,
    /// The custom colour picker while open, with the colour in it.
    picker: Option<Picking>,
    tab: Tab,
    /// Alias → `ssh -G` result.
    resolved: HashMap<String, ResolvedAt>,
    requested: HashSet<(String, u64)>,
    filter: String,
    /// "New tunnel" was clicked for this host; the app opens the editor.
    new_tunnel: Option<String>,
    action: Option<DetailAction>,
    /// Alias → (config generation, jump hosts on the way).
    routes: HashMap<String, (u64, Result<Vec<RouteHop>, String>)>,
    routes_requested: HashSet<(String, u64)>,
    /// Toast texts ("Copied HostName").
    notices: Vec<String>,
}

/// Actions the hosts view (or the app) carries out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailAction {
    Edit(String),
    Rename(String),
    /// A hop on the route was clicked: show that host.
    Select(String),
    /// *Generate key…* from the health banner.
    GenerateKey(String),
    /// "How to start it" next to a stopped agent: *Settings*.
    OpenSettings,
    /// A tunnel of the host: show it in *Tunnels*.
    OpenTunnel(String),
}

impl HostDetail {
    /// The host "New tunnel" was clicked for, once.
    pub fn take_new_tunnel(&mut self) -> Option<String> {
        self.new_tunnel.take()
    }

    /// Toast texts raised since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Asks for `ssh -G` on `alias` once per config generation, unless the
    /// current result is already here.
    pub fn resolve(&mut self, alias: &str, generation: u64) -> Option<Command> {
        let current = self
            .resolved
            .get(alias)
            .is_some_and(|(g, _, _)| *g == generation);
        (!current && self.requested.insert((alias.to_string(), generation)))
            .then(|| Command::Resolve(alias.to_string()))
    }

    /// `user@host[:port]` from the `ssh -G` result fetched for `alias`.
    pub fn address(&self, alias: &str) -> Option<String> {
        let (_, Ok(resolved), _) = self.resolved.get(alias)? else {
            return None;
        };
        let host = resolved.get("hostname")?;
        let mut address = match resolved.get("user") {
            Some(user) => format!("{user}@{host}"),
            None => host.to_string(),
        };
        if let Some(port) = resolved.get("port").filter(|p| *p != "22") {
            address.push(':');
            address.push_str(port);
        }
        Some(address)
    }

    /// The host's first `identityfile` from `ssh -G`, once resolved (a
    /// tunnel's *Load key into agent*).
    pub fn identity_file(&self, alias: &str) -> Option<String> {
        let (_, Ok(resolved), _) = self.resolved.get(alias)? else {
            return None;
        };
        resolved.get("identityfile").map(str::to_string)
    }

    /// "New tunnel…" from somewhere else (the host list's menu).
    pub fn request_new_tunnel(&mut self, alias: String) {
        self.new_tunnel = Some(alias);
    }

    pub fn take_action(&mut self) -> Option<DetailAction> {
        self.action.take()
    }

    /// Stands in for typing into the notes field.
    #[cfg(test)]
    pub fn type_notes(&mut self, notes: &str) {
        self.notes = notes.to_string();
    }

    pub fn on_route(
        &mut self,
        alias: String,
        generation: u64,
        result: Result<Vec<RouteHop>, String>,
    ) {
        let newer = self
            .routes
            .get(&alias)
            .is_none_or(|(known, _)| generation >= *known);
        if newer {
            self.routes.insert(alias, (generation, result));
        }
    }

    pub fn on_resolved(
        &mut self,
        alias: String,
        generation: u64,
        result: Result<Resolved, String>,
    ) {
        let newer = self
            .resolved
            .get(&alias)
            .is_none_or(|(known, _, _)| generation >= *known);
        if newer {
            self.resolved
                .insert(alias, (generation, result, SystemTime::now()));
        }
    }

    pub fn show(&mut self, ui: &mut Ui, host: &HostEntry, shown: &Shown) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some(set) = shown.config.set() else {
            return commands;
        };
        let generation = shown.config.generation;
        let alias = host.primary().to_string();
        self.sync(
            &alias,
            shown.data.hosts.get(host).cloned().unwrap_or_default(),
        );
        commands.extend(self.resolve(&alias, generation));
        // The route needs `ssh -G` per hop: only for hosts that jump.
        let resolved = self
            .resolved
            .get(&alias)
            .filter(|(g, _, _)| *g == generation)
            .and_then(|(_, r, _)| r.as_ref().ok());
        let jumps = resolved
            .and_then(|r| r.get("proxyjump"))
            .is_some_and(|j| !j.eq_ignore_ascii_case("none"));
        if jumps && self.routes_requested.insert((alias.clone(), generation)) {
            commands.push(Command::ResolveRoute(alias.clone()));
        }
        let proxy_command = resolved
            .and_then(|r| r.get("proxycommand"))
            .filter(|c| !c.eq_ignore_ascii_case("none"))
            .map(str::to_string);
        let identity_file = resolved
            .and_then(|r| r.get("identityfile"))
            .map(str::to_string);
        let route = jumps.then(|| {
            self.routes
                .get(&alias)
                .filter(|(g, _)| *g == generation)
                .map(|(_, r)| r.clone())
        });
        let read_only = shown
            .config
            .read_only(host.at.file)
            .map(|r| format!("Read-only: {r}"));
        let narrow = ui.available_width() < NARROW;

        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        self.header(ui, host, set, shown, narrow, &mut commands);
        ui.add_space(8.0);
        self.actions(ui, host, shown, read_only.as_deref(), &mut commands);
        if let Some(select) = route_line(ui, &alias, route, proxy_command, shown.config.hosts()) {
            self.action = Some(DetailAction::Select(select));
        }
        let fix = health(
            ui,
            &alias,
            shown,
            (identity_file.as_deref(), read_only.as_deref()),
            &mut commands,
        );
        if let Some(fix) = fix {
            self.action = Some(fix);
        }
        ui.add_space(8.0);

        let tunnels: Vec<&Tunnel> = shown
            .data
            .tunnels
            .tunnels
            .iter()
            .filter(|t| host.aliases.contains(&t.host))
            .collect();
        let tunnels_label = format!("Tunnels · {}", tunnels.len());
        let labels = ["Overview", "Effective config", tunnels_label.as_str()];
        let selected = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let tabs_top = ui.cursor().top();
        if let Some(index) = nav::tabs(ui, Id::unique("host_detail_tabs"), &labels, selected) {
            self.tab = Tab::ALL[index];
        }
        // The *Effective config* tab, for the tour (its label's place).
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().left() + 100.0, tabs_top),
            egui::vec2(130.0, 32.0),
        );
        targets::register(ui.ctx(), Target::EffectiveConfigTab, tab_rect);
        if targets::take_click(ui.ctx(), Target::EffectiveConfigTab) {
            self.tab = Tab::Config;
        }
        ui.add_space(8.0);
        let response = egui::ScrollArea::vertical()
            .id_salt("host_detail")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Overview => self.overview(ui, &alias, shown, narrow, &mut commands),
                Tab::Config => self.effective(ui, &alias, generation, shown.inset, &mut commands),
                Tab::Tunnels => self.tunnels(ui, &alias, &tunnels, shown.states, &mut commands),
            });
        targets::register(
            ui.ctx(),
            Target::DetailTabs,
            response.inner_rect.with_min_y(tabs_top),
        );
        commands
    }

    /// The *Effective config* tab is open (the tour's step 5).
    pub fn on_effective_config(&self) -> bool {
        self.tab == Tab::Config
    }

    /// Back to *Overview* (the tour entering a step).
    pub fn show_overview(&mut self) {
        self.tab = Tab::Overview;
    }

    /// Takes the saved metadata: a new host resets the notes draft; notes
    /// being edited stay.
    fn sync(&mut self, alias: &str, meta: HostMeta) {
        let other_host = self.for_alias.as_deref() != Some(alias);
        if other_host {
            self.for_alias = Some(alias.to_string());
            self.sent = None;
            self.notes = meta.notes.clone();
            self.new_tag = None;
            self.picker = None;
            self.filter.clear();
        }
        if self.sent.as_ref() == Some(&meta) {
            self.sent = None;
        }
        let meta = self.sent.clone().unwrap_or(meta);
        if !other_host && meta.notes != self.base.notes && self.notes.trim_end() == self.base.notes
        {
            self.notes = meta.notes.clone();
        }
        self.base = meta;
    }

    /// Saves `meta` at once (star, colour, tags, notes).
    fn save_meta(&mut self, alias: &str, meta: HostMeta, commands: &mut Vec<Command>) {
        commands.push(Command::SetMetadata {
            alias: alias.to_string(),
            meta: meta.clone(),
        });
        self.sent = Some(meta.clone());
        self.base = meta;
    }

    /// Title, favourite star and where it is defined; the address below.
    fn header(
        &mut self,
        ui: &mut Ui,
        host: &HostEntry,
        set: &ConfigSet,
        shown: &Shown,
        narrow: bool,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let alias = host.primary().to_string();
        let meta_read_only = meta_read_only(shown.data);
        let mut toggled = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let font = fonts::semibold(if narrow { 20.0 } else { 22.0 });
            // Room left for the star and the location chip.
            let room = (ui.available_width() - 130.0).max(40.0);
            let (title, cut) = text::elide(ui.painter(), &alias, &font, room);
            let title = ui.add(
                egui::Label::new(RichText::new(title).font(font).color(p.text_strong))
                    .selectable(false)
                    .sense(Sense::hover()),
            );
            if cut {
                let full = alias.clone();
                popup::tip_below(title, |ui| {
                    ui.label(full);
                });
            }
            toggled = star(ui, self.base.favorite, meta_read_only.as_deref()).clicked();
            let (short, full) = util::location(set, host.at);
            location_chip(ui, &short, &full);
            if host.aliases.len() > 1 {
                ui.label(RichText::new(host.aliases[1..].join(" ")).color(p.dim));
            }
        });
        if toggled {
            let meta = HostMeta {
                favorite: !self.base.favorite,
                ..self.base.clone()
            };
            self.save_meta(&alias, meta, commands);
        }
        if let Some(address) = self.address(&alias) {
            ui.label(
                RichText::new(address)
                    .font(FontId::monospace(13.0))
                    .color(p.dim),
            );
        }
        let small = |text: String, color: Color32| RichText::new(text).size(12.0).color(color);
        for &at in &host.also_in {
            let (short, full) = util::location(set, at);
            hint(
                ui,
                small(format!("Also configured at {short}"), p.dim),
                &full,
            );
        }
        if host.conditional {
            ui.label(small(
                "Included inside a Host/Match block: it only applies when that block matches too."
                    .into(),
                p.yellow,
            ));
        }
        if host.has_wildcards {
            ui.label(small(
                "The defining block also has wildcard patterns.".into(),
                p.dim,
            ));
        }
    }

    /// Connect, New tunnel, Edit, Rename… and when it was last launched.
    fn actions(
        &mut self,
        ui: &mut Ui,
        host: &HostEntry,
        shown: &Shown,
        read_only: Option<&str>,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let alias = host.primary();
        ui.horizontal_wrapped(|ui| {
            let tools = tools_error(shown.ssh);
            let tip = tools
                .as_deref()
                .unwrap_or("Open a terminal running ssh for this host");
            let connect = Button::new("Connect")
                .icon(icons::SQUARE_TERMINAL)
                .primary()
                .tip(tip);
            let response = ui.add_enabled(tools.is_none(), connect);
            if targets::clicked(ui.ctx(), Target::Connect, &response) {
                commands.push(Command::Connect(alias.to_string()));
            }
            let new_tunnel = Button::new("New tunnel")
                .icon(icons::PLUS)
                .tip("Create a tunnel through this host");
            if ui.add(new_tunnel).clicked() {
                self.new_tunnel = Some(alias.to_string());
            }
            let edit = Button::new("Edit")
                .tip(read_only.unwrap_or("Edit this host's block in the config"));
            let response = ui.add_enabled(read_only.is_none(), edit);
            if targets::clicked(ui.ctx(), Target::Edit, &response) {
                self.action = Some(DetailAction::Edit(alias.to_string()));
            }
            let rename = Button::new("Rename…")
                .tip(read_only.unwrap_or("Rename this host in the config and in sshelf's data"));
            if ui.add_enabled(read_only.is_none(), rename).clicked() {
                self.action = Some(DetailAction::Rename(alias.to_string()));
            }
            let last = match shown.data.history.last_launch(host) {
                Some(time) => format!("Last launched from sshelf {}", util::ago(time)),
                None => "Never launched from sshelf".to_string(),
            };
            ui.add_space(4.0);
            hint(
                ui,
                RichText::new(last).size(12.0).color(p.dim),
                "Only sessions started from sshelf can be tracked.",
            );
        });
        if let Some(why) = read_only {
            ui.label(RichText::new(why).size(12.0).color(p.yellow));
        }
    }

    /// *Connection* and *Notes & tags*: side by side, stacked when narrow.
    fn overview(
        &mut self,
        ui: &mut Ui,
        alias: &str,
        shown: &Shown,
        narrow: bool,
        commands: &mut Vec<Command>,
    ) {
        if narrow {
            self.connection(ui, alias, narrow);
            self.notes_and_tags(ui, alias, shown, commands);
        } else {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.columns(2, |columns| {
                self.connection(&mut columns[0], alias, narrow);
                self.notes_and_tags(&mut columns[1], alias, shown, commands);
            });
        }
    }

    /// The main connection values from `ssh -G`, each copyable.
    fn connection(&mut self, ui: &mut Ui, alias: &str, narrow: bool) {
        let p = &Palette::ONE_DARK;
        card(ui, "Connection", |ui| match self.resolved.get(alias) {
            None => {
                ui.spinner();
            }
            Some((_, Err(_), _)) => {
                ui.label(
                    RichText::new("ssh -G failed: see Effective config")
                        .size(13.0)
                        .color(p.red),
                );
            }
            Some((_, Ok(resolved), _)) => {
                ui.spacing_mut().item_spacing.y = 2.0;
                let key_width = if narrow { 110.0 } else { 120.0 };
                for (key, label) in CONNECTION {
                    let values = resolved.options.iter().filter(|(k, _)| k == key);
                    let first_only = *key == "identityfile";
                    for (_, value) in values.take(if first_only { 1 } else { usize::MAX }) {
                        if *key == "proxyjump" && value.eq_ignore_ascii_case("none") {
                            continue;
                        }
                        let row = copy::Row {
                            name: label,
                            key: (label, key_width, p.dim),
                            key_font: FontId::proportional(13.0),
                            value,
                            value_color: p.text,
                            height: 28.0,
                            fill: None,
                            hover_fill: p.separator,
                            hover_outline: false,
                            corners: CornerRadius::same(radius::CONTROL),
                        };
                        if copy::row(ui, row) {
                            self.notices.push(format!("Copied {label}"));
                        }
                    }
                }
            }
        });
    }

    /// Tags (chips, "+ tag"), the colour swatches and the notes.
    fn notes_and_tags(
        &mut self,
        ui: &mut Ui,
        alias: &str,
        shown: &Shown,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let read_only = meta_read_only(shown.data);
        card(ui, "Notes & tags", |ui| {
            if let Some(reason) = &read_only {
                ui.label(RichText::new(reason).size(12.0).color(p.yellow));
            }
            ui.add_enabled_ui(read_only.is_none(), |ui| {
                self.tags(ui, alias, commands);
                self.swatches(ui, alias, commands);
                self.notes_field(ui, alias, shown.inset, commands);
            });
        });
    }

    fn tags(&mut self, ui: &mut Ui, alias: &str, commands: &mut Vec<Command>) {
        let mut remove = None;
        let mut add = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (index, tag) in self.base.tags.iter().enumerate() {
                if tag_chip(ui, tag) {
                    remove = Some(index);
                }
            }
            match &mut self.new_tag {
                Some((typed, focused)) => {
                    let edit = ui.add(
                        egui::TextEdit::singleline(typed)
                            .hint_text("tag")
                            .desired_width(120.0)
                            .margin(Margin::symmetric(8, 2)),
                    );
                    if !*focused {
                        edit.request_focus();
                        *focused = true;
                    } else if edit.lost_focus() {
                        let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        add = Some((!escaped).then(|| typed.clone()));
                    }
                }
                None => {
                    if add_tag_chip(ui).clicked() {
                        self.new_tag = Some((String::new(), false));
                    }
                }
            }
        });
        if let Some(typed) = add {
            self.new_tag = None;
            let tags = merge_tags(&self.base.tags, typed.as_deref().unwrap_or_default());
            if tags != self.base.tags {
                let meta = HostMeta {
                    tags,
                    ..self.base.clone()
                };
                self.save_meta(alias, meta, commands);
            }
        }
        if let Some(index) = remove {
            let mut meta = self.base.clone();
            meta.tags.remove(index);
            self.save_meta(alias, meta, commands);
        }
    }

    /// *none*, the five host colours, the stored colour as a sixth, selected
    /// swatch when it is none of them, and "+" for any other colour
    /// (SPEC.md §10).
    fn swatches(&mut self, ui: &mut Ui, alias: &str, commands: &mut Vec<Command>) {
        let current = self
            .base
            .color
            .map(|Color([r, g, b])| Color32::from_rgb(r, g, b));
        let custom = current.filter(|c| SWATCHES.iter().all(|(_, s)| s != c));
        let mut picked = None;
        let mut plus = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if swatch(ui, None, "No colour", current.is_none()).clicked() {
                picked = Some(None);
            }
            for (name, color) in SWATCHES {
                if swatch(ui, Some(color), name, current == Some(color)).clicked() {
                    picked = Some(Some(color));
                }
            }
            if let Some(color) = custom {
                let [r, g, b, _] = color.to_array();
                let name = format!("Custom #{r:02x}{g:02x}{b:02x}");
                swatch(ui, Some(color), &name, true);
            }
            let response = plus_swatch(ui);
            if response.clicked() {
                self.picker = match self.picker {
                    Some(_) => None,
                    None => Some(Picking::new(current.unwrap_or(SWATCHES[0].1))),
                };
            }
            plus = Some(response);
        });
        if let (Some(picking), Some(plus)) = (&mut self.picker, plus) {
            match color_picker(ui, &plus, picking) {
                Picker::Open => {}
                Picker::Use(color) => {
                    self.picker = None;
                    picked = Some(Some(color));
                }
                Picker::Cancel => self.picker = None,
            }
        }
        if let Some(color) = picked.filter(|c| *c != current) {
            let meta = HostMeta {
                color: color.map(|c| Color([c.r(), c.g(), c.b()])),
                ..self.base.clone()
            };
            self.save_meta(alias, meta, commands);
        }
    }

    /// The notes; *Save notes* / *Revert* while they differ from the saved.
    fn notes_field(
        &mut self,
        ui: &mut Ui,
        alias: &str,
        inset: Color32,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        ui.add(
            egui::TextEdit::multiline(&mut self.notes)
                .hint_text("Notes")
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .background_color(inset)
                .margin(Margin::symmetric(12, 10)),
        );
        if self.notes.trim_end() == self.base.notes {
            return;
        }
        ui.horizontal(|ui| {
            if ui
                .add(Button::new("Save notes").primary().small())
                .clicked()
            {
                let meta = HostMeta {
                    notes: self.notes.trim_end().to_string(),
                    ..self.base.clone()
                };
                self.save_meta(alias, meta, commands);
            }
            if ui.add(Button::new("Revert").small()).clicked() {
                self.notes = self.base.notes.clone();
            }
            ui.label(RichText::new("unsaved notes").size(12.0).color(p.yellow));
        });
    }

    /// The `ssh -G` table: filter, its age and *Refresh*; every value
    /// copyable.
    fn effective(
        &mut self,
        ui: &mut Ui,
        alias: &str,
        generation: u64,
        inset: Color32,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let entry = self.resolved.get(alias);
        let current = entry.is_some_and(|(g, _, _)| *g == generation);
        let mut refresh = false;
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 160.0).clamp(80.0, 260.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter options")
                    .desired_width(width)
                    .margin(Margin::symmetric(12, 6)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh = ui
                    .add(Button::icon_only(icons::REFRESH_CW, "Run ssh -G again").small())
                    .clicked();
                match entry {
                    Some((_, _, at)) if current => {
                        let age = format!("ssh -G · {}", since(ui.ctx(), *at));
                        ui.label(RichText::new(age).size(12.0).color(p.dim));
                    }
                    _ => {
                        ui.spinner();
                    }
                }
            });
        });
        if refresh {
            self.resolved.remove(alias);
            self.requested.retain(|(a, _)| a != alias);
            commands.push(Command::Resolve(alias.to_string()));
            return;
        }
        match self.resolved.get(alias) {
            None => {}
            Some((_, Err(e), _)) => {
                marks::banner(ui, p.red_tint, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        icons::show(ui, icons::CIRCLE_X, icons::INLINE, p.red);
                        ui.label(RichText::new(format!("ssh -G failed: {e}")).color(p.red));
                    });
                });
            }
            Some((_, Ok(resolved), _)) => {
                let needle = self.filter.trim().to_lowercase();
                let wanted = |(k, v): &&(String, String)| {
                    needle.is_empty() || k.contains(&needle) || v.to_lowercase().contains(&needle)
                };
                let important = IMPORTANT
                    .iter()
                    .flat_map(|key| resolved.options.iter().filter(move |(k, _)| k == key));
                let rest = resolved
                    .options
                    .iter()
                    .filter(|(k, _)| !IMPORTANT.contains(&k.as_str()));
                let rows: Vec<&(String, String)> = important.chain(rest).filter(wanted).collect();
                let notices = &mut self.notices;
                Frame::new()
                    .fill(inset)
                    .corner_radius(radius::CONTROL)
                    .inner_margin(Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let key_width = (ui.available_width() * 0.4).min(240.0);
                        table_header(ui, key_width);
                        if rows.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("   No option matches")
                                    .size(13.0)
                                    .color(p.dim),
                            );
                            ui.add_space(6.0);
                        }
                        for (index, (key, value)) in rows.into_iter().enumerate() {
                            let important = IMPORTANT.contains(&key.as_str());
                            let row = copy::Row {
                                name: key,
                                key: (key, key_width, if important { p.accent } else { p.dim }),
                                key_font: FontId::monospace(13.0),
                                value,
                                value_color: if important { p.text } else { p.dim },
                                height: 30.0,
                                fill: (index % 2 == 1).then_some(p.zebra),
                                hover_fill: p.surface,
                                hover_outline: true,
                                corners: CornerRadius::same(radius::ROW),
                            };
                            if copy::row(ui, row) {
                                notices.push(format!("Copied {key}"));
                            }
                        }
                    });
            }
        }
    }

    /// The tunnels through this host: state, forwards, Start / Stop.
    fn tunnels(
        &mut self,
        ui: &mut Ui,
        alias: &str,
        tunnels: &[&Tunnel],
        states: Option<&Snapshot>,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        if tunnels.is_empty() {
            ui.label(
                RichText::new("No tunnels through this host yet.")
                    .size(13.0)
                    .color(p.dim),
            );
        }
        ui.spacing_mut().item_spacing.y = 6.0;
        for tunnel in tunnels {
            let state = states.map_or(TunnelState::Stopped, |s| s.state(&tunnel.id));
            Frame::new()
                .fill(p.surface)
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    row(ui, 28.0, |ui| {
                        match &state {
                            TunnelState::Running => marks::dot(ui, p.green),
                            TunnelState::Failed(_) => marks::dot(ui, p.red),
                            TunnelState::Starting | TunnelState::Stopping => {
                                marks::pulsing_ring(ui, marks::DOT, p.yellow)
                            }
                            TunnelState::Stopped => marks::dot(ui, p.comment),
                        };
                        let name = if tunnel.name.is_empty() {
                            &tunnel.id
                        } else {
                            &tunnel.name
                        };
                        if button::link(ui, name, 14.0).clicked() {
                            self.action = Some(DetailAction::OpenTunnel(tunnel.id.clone()));
                        }
                        if let Some(first) = tunnel.forwards.first() {
                            forward_line(ui, first);
                            if tunnel.forwards.len() > 1 {
                                let more = format!("+{}", tunnel.forwards.len() - 1);
                                ui.label(RichText::new(more).size(12.0).color(p.dim));
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let busy = state == TunnelState::Stopping;
                            let running =
                                matches!(state, TunnelState::Running | TunnelState::Starting);
                            if busy || running {
                                let stop = ui.add_enabled(
                                    !busy,
                                    Button::new("Stop").icon(icons::SQUARE).small(),
                                );
                                if stop.clicked() {
                                    commands.push(Command::StopTunnel(tunnel.id.clone()));
                                }
                            } else if ui
                                .add(Button::new("Start").icon(icons::PLAY).small())
                                .clicked()
                            {
                                commands.push(Command::StartTunnel(tunnel.id.clone()));
                            }
                            let (label, color) = state_label(&state);
                            ui.label(RichText::new(label).size(12.0).color(color));
                        });
                    });
                });
        }
        ui.add_space(4.0);
        if ui
            .add(Button::new("New tunnel").icon(icons::PLUS).small())
            .clicked()
        {
            self.new_tunnel = Some(alias.to_string());
        }
    }
}

/// Why *Connect* (and the like) can't run, if it can't.
fn tools_error(ssh: Option<&SshInfo>) -> Option<String> {
    match ssh.map(|s| &s.tools) {
        Some(Ok(_)) => None,
        Some(Err(e)) => Some(e.clone()),
        None => Some("still detecting ssh".to_string()),
    }
}

fn meta_read_only(data: &AppData) -> Option<String> {
    match &data.hosts_status {
        LoadStatus::ReadOnly(reason) => Some(format!("hosts.toml is read-only: {reason}")),
        _ => None,
    }
}

/// A label with a tooltip (styled like every other one); in a wrapping
/// row it moves to the next line whole.
fn hint(ui: &mut Ui, text: RichText, tip: &str) {
    let label = ui.add(
        egui::Label::new(text)
            .selectable(false)
            .extend()
            .sense(Sense::hover()),
    );
    let tip = tip.to_string();
    popup::tip_below(label, |ui| {
        ui.label(tip);
    });
}

/// A left-to-right row `height` high with everything centred on it (a
/// plain horizontal row takes its height from the first item).
fn row(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_height(height);
            add(ui);
        },
    );
}

/// A raised card with a 13 px heading.
fn card<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = &Palette::ONE_DARK;
    Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.label(
                RichText::new(title)
                    .font(fonts::semibold(13.0))
                    .color(p.text_strong),
            );
            add(ui)
        })
        .inner
}

/// The favourite star: a toggle whose Lucide outline fades from grey to
/// Light Yellow (Lucide has no filled star), with a background only on
/// hover; its tooltip says what a click does, or why it can't.
fn star(ui: &mut Ui, favorite: bool, read_only: Option<&str>) -> Response {
    let p = &Palette::ONE_DARK;
    let name = if favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    let enabled = read_only.is_none();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), sense);
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, favorite, name));
    let ctx = ui.ctx();
    let hover = motion::fade(
        ctx,
        response.id.with("hover"),
        response.hovered() && enabled,
        timing::HOVER,
        motion::ease_out,
    );
    let on = motion::fade(
        ctx,
        response.id.with("on"),
        favorite,
        timing::STAR,
        motion::ease_out,
    );
    let painter = ui.painter();
    // No background at rest; one fades in on hover.
    if hover > 0.0 {
        painter.rect_filled(rect, radius::CONTROL, p.surface.gamma_multiply(hover));
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icons::STAR,
        icons::font(16.0),
        p.dim.lerp_to_gamma(p.yellow, on),
    );
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response.on_hover_cursor(CursorIcon::NotAllowed)
    };
    let tip = read_only.unwrap_or(name).to_string();
    popup::tip_below(response, |ui| {
        ui.label(tip);
    })
}

/// "config:6" in mono on a chip; the full path in its tooltip.
fn location_chip(ui: &mut Ui, short: &str, full: &str) {
    let p = &Palette::ONE_DARK;
    let galley = ui
        .painter()
        .layout_no_wrap(short.to_string(), FontId::monospace(11.0), p.text);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(16.0, 6.0), Sense::hover());
    let name = format!("Defined at {short}");
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    ui.painter().rect_filled(rect, radius::CONTROL, p.surface);
    ui.painter()
        .galley(rect.min + vec2(8.0, 3.0), galley, p.text);
    let full = full.to_string();
    popup::tip_below(response, |ui| {
        ui.label(full);
    });
}

/// A tag chip; hovering shows an × to remove it. Returns whether it was
/// removed.
fn tag_chip(ui: &mut Ui, tag: &str) -> bool {
    let p = &Palette::ONE_DARK;
    let galley = ui
        .painter()
        .layout_no_wrap(tag.to_string(), FontId::proportional(12.0), p.text);
    let size = vec2(
        galley.size().x + 8.0 + 4.0 + 12.0 + 6.0,
        galley.size().y + 4.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, tag));
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, p.selection);
    painter.galley(
        pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    let x_rect = Rect::from_center_size(
        pos2(rect.right() - 6.0 - 6.0, rect.center().y),
        Vec2::splat(14.0),
    );
    let remove = ui.interact(x_rect, response.id.with("remove"), Sense::click());
    let name = format!("Remove tag {tag}");
    remove.widget_info(|| WidgetInfo::labeled(Role::Button, remove.enabled(), &name));
    let shown = motion::fade(
        ui.ctx(),
        response.id.with("x"),
        response.contains_pointer() && remove.enabled(),
        timing::HOVER,
        motion::ease_out,
    );
    if shown > 0.0 {
        let color = if remove.hovered() {
            p.text_strong
        } else {
            p.dim
        };
        ui.painter().text(
            x_rect.center(),
            Align2::CENTER_CENTER,
            icons::X,
            icons::font(12.0),
            color.gamma_multiply(shown),
        );
    }
    remove.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// The "+ tag" chip.
fn add_tag_chip(ui: &mut Ui) -> Response {
    let p = &Palette::ONE_DARK;
    let galley = ui
        .painter()
        .layout_no_wrap("tag".to_string(), FontId::proportional(12.0), p.dim);
    let size = vec2(
        galley.size().x + 8.0 + 12.0 + 4.0 + 8.0,
        galley.size().y + 4.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, response.enabled(), "Add tag"));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        radius::CONTROL,
        p.surface.lerp_to_gamma(p.selection, hover),
    );
    painter.text(
        pos2(rect.left() + 8.0 + 6.0, rect.center().y),
        Align2::CENTER_CENTER,
        icons::PLUS,
        icons::font(12.0),
        p.dim,
    );
    painter.galley(
        pos2(
            rect.left() + 8.0 + 12.0 + 4.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        p.dim,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A 16 px colour swatch (`None`: no colour, an empty ring); the selected
/// one has a ring around it, 2 px away.
fn swatch(ui: &mut Ui, color: Option<Color32>, name: &str, selected: bool) -> Response {
    let p = &Palette::ONE_DARK;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(Role::RadioButton, response.enabled(), selected, name)
    });
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered() && !selected,
        timing::HOVER,
        motion::ease_out,
    );
    let painter = ui.painter();
    let center = rect.center();
    match color {
        Some(color) => painter.circle_filled(center, 8.0, color),
        None => painter.circle_stroke(center, 7.5, Stroke::new(1.0, p.border)),
    };
    if selected {
        painter.circle_stroke(center, 10.5, Stroke::new(1.0, p.text_strong));
    } else if hover > 0.0 {
        painter.circle_stroke(
            center,
            10.5,
            Stroke::new(1.0, p.border.gamma_multiply(hover)),
        );
    }
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let name = name.to_string();
    popup::tip_below(response, |ui| {
        ui.label(name);
    })
}

/// The "+" swatch that opens the custom colour picker.
fn plus_swatch(ui: &mut Ui) -> Response {
    let p = &Palette::ONE_DARK;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    response
        .widget_info(|| WidgetInfo::labeled(Role::Button, response.enabled(), "Custom colour…"));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    let painter = ui.painter();
    painter.circle_stroke(
        rect.center(),
        7.5,
        Stroke::new(1.0, p.border.lerp_to_gamma(p.text, hover)),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icons::PLUS,
        icons::font(12.0),
        p.dim.lerp_to_gamma(p.text_strong, hover),
    );
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    popup::tip_below(response, |ui| {
        ui.label("Pick any colour");
    })
}

/// About how tall the custom colour picker is, with its buttons.
const PICKER_HEIGHT: f32 = 280.0;

/// What the custom colour picker did this frame.
enum Picker {
    Open,
    Use(Color32),
    Cancel,
}

/// The custom colour picker below the "+" swatch, then *Use colour* /
/// *Cancel*. Nothing is saved before *Use colour*; Esc or a click
/// elsewhere cancels.
fn color_picker(ui: &Ui, plus: &Response, picking: &mut Picking) -> Picker {
    let p = &Palette::ONE_DARK;
    let ctx = ui.ctx();
    let mut result = None;
    // Below the "+", or above it when there is no room (the swatches sit
    // low in the pane), so it never covers the swatch row.
    let room_below = ctx.content_rect().bottom() - plus.rect.bottom();
    let (pos, pivot) = if room_below > PICKER_HEIGHT {
        (plus.rect.left_bottom() + vec2(0.0, 6.0), Align2::LEFT_TOP)
    } else {
        (plus.rect.left_top() - vec2(0.0, 6.0), Align2::LEFT_BOTTOM)
    };
    let area = egui::Area::new(plus.id.with("picker"))
        .order(egui::Order::Foreground)
        .pivot(pivot)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.bg_dark)
                .stroke(Stroke::new(1.0, p.selection))
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    color::picker(ui, picking);
                    ui.horizontal(|ui| {
                        if ui
                            .add(Button::new("Use colour").primary().small())
                            .clicked()
                        {
                            result = Some(Picker::Use(picking.color()));
                        }
                        if ui.add(Button::new("Cancel").small()).clicked() {
                            result = Some(Picker::Cancel);
                        }
                    });
                });
        });
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| !area.response.rect.contains(pos) && !plus.rect.contains(pos))
    });
    match result {
        Some(result) => result,
        None if escape || outside => Picker::Cancel,
        None => Picker::Open,
    }
}

/// The table's "OPTION / VALUE" header row.
fn table_header(ui: &mut Ui, key_width: f32) {
    let p = &Palette::ONE_DARK;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    for (x, text) in [(8.0, "OPTION"), (8.0 + key_width, "VALUE")] {
        ui.painter().text(
            pos2(rect.left() + x, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            fonts::semibold(11.0),
            p.dim,
        );
    }
}

/// One forward in mono: "L 6443 → 127.0.0.1:6443" with a Lucide arrow.
fn forward_line(ui: &mut Ui, forward: &Forward) {
    let p = &Palette::ONE_DARK;
    let mono = |text: String| {
        RichText::new(text)
            .font(FontId::monospace(12.0))
            .color(p.dim)
    };
    let listen = if forward.bind == DEFAULT_BIND {
        forward.listen.to_string()
    } else {
        format!("{}:{}", forward.bind, forward.listen)
    };
    ui.spacing_mut().item_spacing.x = 4.0;
    match forward.kind {
        ForwardKind::D => {
            ui.label(mono(format!("D {listen} (SOCKS)")));
        }
        kind => {
            ui.label(mono(format!("{kind:?} {listen}")));
            icons::show(ui, icons::ARROW_RIGHT, 12.0, p.dim);
            ui.label(mono(forward.target.clone().unwrap_or_default()));
        }
    }
    ui.spacing_mut().item_spacing.x = 8.0;
}

/// "4 s ago" for the first minute (repainting each second), then
/// "3 min ago" and so on.
fn since(ctx: &egui::Context, at: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(at)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    if secs < 60 {
        ctx.request_repaint_after(Duration::from_secs(1));
        format!("{secs} s ago")
    } else {
        util::ago(at)
    }
}

/// The existing tags plus the comma-separated `typed` ones, trimmed, empty
/// ones and duplicates dropped.
fn merge_tags(existing: &[String], typed: &str) -> Vec<String> {
    let mut tags = existing.to_vec();
    for tag in typed.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

/// The Health line, and for trouble a tinted banner with its fix (SPEC.md
/// §10, M7/M8 design). Returns what a fix asked the view to do.
fn health(
    ui: &mut Ui,
    alias: &str,
    shown: &Shown,
    (identity_file, read_only): (Option<&str>, Option<&str>),
    commands: &mut Vec<Command>,
) -> Option<DetailAction> {
    let p = &Palette::ONE_DARK;
    let checking = shown.health.is_some_and(|h| h.checking.contains(alias));
    let report = shown.health.and_then(|h| h.reports.get(alias));
    let line = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 28.0));
    targets::register(ui.ctx(), Target::HealthLine, line);
    row(ui, 28.0, |ui| {
        ui.label(RichText::new("Health").size(13.0).color(p.dim));
        util::health_mark(ui, shown.health, alias, 8.0);
        // *Check* first, from the right, so it always fits; the text gets
        // what is left and is cut with an ellipsis (full text on hover).
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !checking {
                let check = Button::new("Check").icon(icons::ACTIVITY).small().tip(
                    "Log in once without a password (ssh BatchMode, 3 s connect timeout) \
                     and run `true`",
                );
                if ui.add(check).clicked() {
                    commands.push(Command::CheckHealth(vec![alias.to_string()]));
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let mut job = egui::text::LayoutJob::default();
                let mut add = |text: &str, size: f32, color| {
                    job.append(
                        text,
                        0.0,
                        egui::TextFormat::simple(FontId::proportional(size), color),
                    );
                };
                match report {
                    _ if checking => add("Checking…", 13.0, p.yellow),
                    Some(report) => {
                        add(&health_text(&report.health), 13.0, p.text_strong);
                        let when = format!("   checked {}", util::ago(report.checked_at));
                        add(&when, 12.0, p.dim);
                    }
                    None => add("Not checked yet", 13.0, p.dim),
                }
                ui.add(egui::Label::new(job).truncate());
            });
        });
    });
    let report = report.filter(|_| !checking)?;
    let tools = tools_error(shown.ssh);
    let mut action = None;
    match &report.health {
        Health::Ok { .. } => {}
        Health::AuthFailed => {
            let service_stopped = shown
                .ssh
                .and_then(|s| s.service.as_ref())
                .and_then(|s| s.as_ref().ok())
                .is_some_and(|s| !s.is_running());
            marks::banner(ui, p.yellow_tint, |ui| {
                ui.label(
                    RichText::new(
                        "Tunnels run without a terminal, so ssh can only use keys loaded in \
                         ssh-agent or key files without a passphrase.",
                    )
                    .color(p.text_strong),
                );
                if let Some(note) = agent_note(shown.ssh).filter(|_| !service_stopped) {
                    ui.label(RichText::new(note).color(p.dim));
                }
                let why_not = if service_stopped {
                    Some("The ssh-agent service is not running.".to_string())
                } else if let Some(e) = &tools {
                    Some(e.clone())
                } else if identity_file.is_none() {
                    Some("ssh -G lists no IdentityFile for this host.".to_string())
                } else {
                    None
                };
                ui.horizontal_wrapped(|ui| {
                    let file = identity_file.unwrap_or_default();
                    let tip = why_not
                        .clone()
                        .unwrap_or_else(|| format!("Run ssh-add for {file} in a terminal"));
                    let load = Button::new("Load key into agent").small().tip(&tip);
                    if ui.add_enabled(why_not.is_none(), load).clicked() {
                        commands.push(Command::AddKeyToAgent {
                            path: PathBuf::from(file),
                            encrypted: true,
                        });
                    }
                    if service_stopped {
                        ui.label(
                            RichText::new("The ssh-agent service is not running.").color(p.yellow),
                        );
                        if button::link(ui, "How to start it", 12.0).clicked() {
                            action = Some(DetailAction::OpenSettings);
                        }
                    } else {
                        ui.label(RichText::new("runs ssh-add in a terminal").color(p.dim));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    let generate = Button::new("Generate key…").icon(icons::KEY_ROUND).small();
                    if ui.add(generate).clicked() {
                        action = Some(DetailAction::GenerateKey(alias.to_string()));
                    }
                    ui.label(RichText::new("if this host has no key yet").color(p.dim));
                });
            });
        }
        Health::HostKeyUnknown => {
            marks::banner(ui, p.yellow_tint, |ui| {
                ui.label(
                    RichText::new(
                        "sshelf hasn't seen this server's key yet. Review its fingerprint and \
                         accept it; then tunnels and checks can use this host.",
                    )
                    .color(p.text_strong),
                );
                let tip = tools
                    .as_deref()
                    .unwrap_or("Show the server's key and ask before storing it");
                let review = Button::new("Review host key…")
                    .icon(icons::KEY_ROUND)
                    .small()
                    .tip(tip);
                if ui.add_enabled(tools.is_none(), review).clicked() {
                    commands.push(Command::ReviewHostKey(alias.to_string()));
                }
            });
        }
        Health::HostKeyChanged => {
            marks::banner(ui, p.red_tint, |ui| {
                ui.label(
                    RichText::new(
                        "The server's host key changed since it was saved. Compare both keys; \
                         the stored one is replaced only if you confirm.",
                    )
                    .color(p.text_strong),
                );
                let tip = tools
                    .as_deref()
                    .unwrap_or("Compare the stored key with the one the server presents");
                let review = Button::new("Review changed key…").danger().small().tip(tip);
                if ui.add_enabled(tools.is_none(), review).clicked() {
                    commands.push(Command::ReviewHostKey(alias.to_string()));
                }
            });
        }
        Health::Unreachable(why) => {
            marks::banner(ui, p.red_tint, |ui| {
                ui.label(
                    RichText::new(why)
                        .font(FontId::monospace(12.0))
                        .color(p.red),
                );
                if let Some(hint) = hints::failure_hint(why) {
                    ui.label(RichText::new(hint).color(p.text_strong));
                }
                ui.horizontal_wrapped(|ui| {
                    let edit = Button::new("Edit host")
                        .small()
                        .tip(read_only.unwrap_or("Edit this host's block in the config"));
                    if ui.add_enabled(read_only.is_none(), edit).clicked() {
                        action = Some(DetailAction::Edit(alias.to_string()));
                    }
                    ui.label(RichText::new("wrong HostName or Port?").color(p.dim));
                });
            });
        }
    }
    action
}

/// The Health line's short result.
fn health_text(health: &Health) -> String {
    match health {
        Health::Ok { millis } => format!("Auth OK · login {millis} ms"),
        Health::AuthFailed => "Reachable, but no key was accepted".into(),
        Health::HostKeyUnknown => "Reachable, host key not known yet".into(),
        Health::HostKeyChanged => "Reachable, but the host key changed".into(),
        Health::Unreachable(_) => "Unreachable".into(),
    }
}

/// What is known about the agent, for a key that wasn't accepted.
fn agent_note(ssh: Option<&SshInfo>) -> Option<String> {
    match ssh?.agent.as_ref()? {
        AgentStatus::Keys(0) => Some("The agent is running but has no keys loaded.".into()),
        AgentStatus::Keys(n) => Some(format!(
            "The agent has {n} key(s), but the server accepted none of them: check User and \
             IdentityFile."
        )),
        AgentStatus::Unreachable(_) => Some("ssh-agent is not reachable.".into()),
        AgentStatus::Error(_) => None,
    }
}

/// "Route you › bastion › db-gw › this host" (SPEC.md §12, M8) with Lucide
/// chevrons. Hops that are hosts of the config are links; returns the one
/// clicked. `route` is `None` for hosts without `ProxyJump`, `Some(None)`
/// while it is being worked out.
fn route_line(
    ui: &mut Ui,
    alias: &str,
    route: Option<Option<Result<Vec<RouteHop>, String>>>,
    proxy_command: Option<String>,
    hosts: Option<&HostList>,
) -> Option<String> {
    let p = &Palette::ONE_DARK;
    if route.is_none() && proxy_command.is_none() {
        return None;
    }
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let arrow = |ui: &mut Ui| {
            icons::show(ui, icons::CHEVRON_RIGHT, icons::INLINE, p.dim);
        };
        ui.label(RichText::new("Route").size(13.0).color(p.dim));
        ui.label(RichText::new("you").size(13.0));
        match route {
            Some(Some(Ok(hops))) => {
                for hop in hops {
                    arrow(ui);
                    let tip = format!("{}:{}", hop.hostname, hop.port);
                    match hosts.and_then(|h| h.find(&hop.destination)) {
                        Some(host) => {
                            let link = button::link(ui, &hop.destination, 13.0);
                            if link.clicked() {
                                clicked = Some(host.primary().to_string());
                            }
                            popup::tip_below(link, |ui| {
                                ui.label(tip);
                            });
                        }
                        None => hint(
                            ui,
                            RichText::new(&hop.destination).font(FontId::monospace(13.0)),
                            &tip,
                        ),
                    }
                }
            }
            Some(Some(Err(e))) => {
                arrow(ui);
                ui.label(RichText::new("?").color(p.dim));
                ui.label(RichText::new(e).size(12.0).color(p.yellow));
            }
            Some(None) => {
                arrow(ui);
                ui.spinner();
            }
            None => {}
        }
        if let Some(command) = &proxy_command {
            arrow(ui);
            hint(
                ui,
                RichText::new("ProxyCommand").size(13.0).italics(),
                command,
            );
        }
        arrow(ui);
        ui.label(RichText::new(alias).size(13.0).color(p.text_strong));
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_tags_are_trimmed_split_and_deduplicated() {
        let existing = vec!["prod".to_string()];
        assert_eq!(
            merge_tags(&existing, " web ,, prod,db "),
            ["prod", "web", "db"]
        );
        assert_eq!(merge_tags(&existing, "  "), ["prod"]);
    }

    #[test]
    fn health_lines_are_short() {
        assert_eq!(
            health_text(&Health::Ok { millis: 584 }),
            "Auth OK · login 584 ms"
        );
        assert_eq!(
            health_text(&Health::Unreachable("timed out".into())),
            "Unreachable"
        );
    }
}
