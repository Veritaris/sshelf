//! Groups view (SPEC.md §7, §10; boards A-Groups, Group-Edit): a list of
//! groups with their switches and aggregate state, a detail pane (status,
//! the per-tunnel states with failures, shared tunnels), the editor (the
//! tunnels as a multi-select dropdown, the *Start with sshelf* switch) and
//! the delete dialog.

use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Frame, Id, Layout, Margin, Rect, RichText, Role,
    Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};
use sshelf_core::core::{AppData, Command};
use sshelf_core::hints;
use sshelf_core::redact::DOTS;
use sshelf_core::supervisor::{GroupState, Owner, Snapshot, TunnelState};
use sshelf_core::tunnels::{Group, Tunnel};

use crate::theme::{Palette, radius, timing};
use crate::tour::steps;
use crate::tour::targets::{self, Target};
use crate::tunnels_view::{
    capitalized, editor_row, field_errors, mark, owner_names, reconnect_text, state_label,
};
use crate::widgets::button::Button;
use crate::widgets::dropdown::{Choice, Dropdown};
use crate::widgets::field::TextField;
use crate::widgets::switch::{switch, switch_raised};
use crate::widgets::{marks, masked, nav, popup, text};
use crate::{fonts, icons, motion, privacy};

/// Width of the group list.
const LIST_WIDTH: f32 = 340.0;
/// A list row (two lines).
const ROW: f32 = 52.0;
const ROW_GAP: f32 = 2.0;
/// The editor's labels and fields (board Group-Edit).
const LABEL: f32 = 150.0;
const FIELD: f32 = 474.0;
/// A row of the detail's tunnel table.
const TUNNEL_ROW: f32 = 44.0;

#[derive(Default)]
pub struct GroupsView {
    selected: Option<String>,
    editor: Option<Group>,
    /// The group the delete dialog is for.
    confirm_delete: Option<String>,
    /// A tunnel row was clicked: the app shows that tunnel.
    open_tunnel: Option<String>,
}

/// A row's second line: "running · 2/2", "degraded · 1/2"; and its colour.
fn state_line(state: GroupState, total: usize) -> (String, Color32) {
    let p = &Palette::ONE_DARK;
    match state {
        GroupState::Stopped => ("stopped".into(), p.dim),
        GroupState::Starting => ("starting…".into(), p.yellow),
        GroupState::Running => (format!("running · {total}/{total}"), p.green),
        GroupState::Degraded { running, total } => {
            (format!("degraded · {running}/{total}"), p.yellow)
        }
    }
}

/// The detail's status chip: text, tint, colour.
fn state_chip(state: GroupState, total: usize) -> (String, Color32, Color32) {
    let p = &Palette::ONE_DARK;
    match state {
        GroupState::Stopped => ("Stopped".into(), p.surface, p.dim),
        GroupState::Starting => ("Starting…".into(), p.yellow_tint, p.yellow),
        GroupState::Running => (
            format!("Running · {total} of {total}"),
            p.green_tint,
            p.green,
        ),
        GroupState::Degraded { running, total } => (
            format!("Degraded · {running} of {total}"),
            p.yellow_tint,
            p.yellow,
        ),
    }
}

/// "L 127.0.0.1:6443": a tunnel's first forward (and how many more);
/// "L •••••" when `masked`.
fn forwards_short(tunnel: &Tunnel, masked: bool) -> String {
    let Some(first) = tunnel.forwards.first() else {
        return String::new();
    };
    let mut text = if masked {
        format!("{:?} {DOTS}", first.kind)
    } else {
        format!("{:?} {}:{}", first.kind, first.bind, first.listen)
    };
    if tunnel.forwards.len() > 1 {
        text.push_str(&format!(" +{}", tunnel.forwards.len() - 1));
    }
    text
}

impl GroupsView {
    /// The tunnel whose row was clicked, once.
    pub fn take_open_tunnel(&mut self) -> Option<String> {
        self.open_tunnel.take()
    }

    /// The editor is open.
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub fn close_editor(&mut self) {
        self.editor = None;
    }

    /// Opens the editor on `group` (the tour, with its changes).
    pub fn edit(&mut self, group: Group) {
        self.selected = Some(group.id.clone());
        self.editor = Some(group);
    }

    /// The draft in the editor.
    pub fn draft_mut(&mut self) -> Option<&mut Group> {
        self.editor.as_mut()
    }

    pub fn select(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        self.editor = None;
    }

    fn new_group(&mut self, data: &AppData) {
        self.editor = Some(Group {
            id: data.tunnels.new_group_id(),
            name: String::new(),
            tunnels: Vec::new(),
            start_on_launch: false,
        });
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        data: Option<&AppData>,
        states: Option<&Snapshot>,
    ) -> Vec<Command> {
        let p = &Palette::ONE_DARK;
        let mut commands = Vec::new();
        let Some(data) = data else {
            ui.spinner();
            return commands;
        };
        let empty = Snapshot::default();
        let states = states.unwrap_or(&empty);
        if self
            .selected
            .as_ref()
            .is_some_and(|id| data.tunnels.group(id).is_none())
        {
            self.selected = None;
        }
        egui::Panel::left("group_list")
            .resizable(false)
            .exact_size(LIST_WIDTH)
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin {
                left: 12,
                right: 12,
                top: 0,
                bottom: 16,
            }))
            .show(ui, |ui| {
                let rect = ui.max_rect().expand2(vec2(12.0, 0.0));
                ui.painter()
                    .vline(rect.right(), rect.y_range(), Stroke::new(1.0, p.bg_dark));
                if self.editor.is_some() {
                    ui.multiply_opacity(0.55);
                }
                self.list(ui, data, states, &mut commands);
            });
        egui::CentralPanel::default()
            .frame(Frame::NONE.inner_margin(Margin {
                left: 32,
                right: 32,
                top: 0,
                bottom: 24,
            }))
            .show(ui, |ui| {
                if self.editor.is_some() {
                    self.editor(ui, data, &mut commands);
                } else if let Some(group) = self
                    .selected
                    .as_ref()
                    .and_then(|id| data.tunnels.group(id))
                    .cloned()
                {
                    self.detail(ui, &group, data, states, &mut commands);
                } else {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Select a group, or create one")
                                .size(16.0)
                                .color(p.dim),
                        );
                    });
                }
            });
        self.delete_dialog(ui.ctx(), data, &mut commands);
        commands
    }

    fn list(
        &mut self,
        ui: &mut Ui,
        data: &AppData,
        states: &Snapshot,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let groups = &data.tunnels.groups;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Groups")
                    .font(fonts::semibold(16.0))
                    .color(p.text_strong),
            );
            ui.label(
                RichText::new(groups.len().to_string())
                    .size(13.0)
                    .color(p.dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let no_tunnels = data.tunnels.tunnels.is_empty();
                let tip = if no_tunnels {
                    "Create a tunnel first: a group starts and stops tunnels"
                } else {
                    "New group"
                };
                if ui
                    .add_enabled(
                        !no_tunnels,
                        Button::icon_only(icons::PLUS, "New group").tip(tip),
                    )
                    .clicked()
                {
                    self.new_group(data);
                }
            });
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("groups")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = ROW_GAP;
                if groups.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No groups yet").size(14.0).color(p.dim));
                        ui.label(
                            RichText::new("A group starts and stops several tunnels together.")
                                .size(12.0)
                                .color(p.dim),
                        );
                    });
                    return;
                }
                let selection_slot = ui.painter().add(Shape::Noop);
                let hover_slot = ui.painter().add(Shape::Noop);
                let mut hovered = None;
                let mut selected_rect = None;
                for (index, group) in groups.iter().enumerate() {
                    let state = states.group_state(group);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    let is_selected =
                        self.selected.as_ref() == Some(&group.id) && self.editor.is_none();
                    response.widget_info(|| {
                        WidgetInfo::selected(Role::Button, true, is_selected, &group.name)
                    });
                    if is_selected {
                        selected_rect = Some((index, rect));
                    }
                    if response.hovered() {
                        hovered = Some((index, rect));
                    }
                    if response.clicked() {
                        self.select(&group.id);
                    }
                    let tour_id = steps::static_id(&group.id);
                    if let Some(id) = tour_id {
                        targets::register(ui.ctx(), Target::GroupRow(id), rect);
                    }
                    let mut row = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect.shrink2(vec2(12.0, 0.0)))
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    row.spacing_mut().item_spacing.x = 10.0;
                    let text_width = (row.available_width() - 46.0).max(40.0);
                    row.allocate_ui_with_layout(
                        vec2(text_width, ROW),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.add_space(9.0);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let color = if is_selected { p.text_strong } else { p.text };
                            let (name, _) = text::elide(
                                ui.painter(),
                                &group.name,
                                &FontId::proportional(14.0),
                                text_width,
                            );
                            ui.add(
                                egui::Label::new(RichText::new(name).size(14.0).color(color))
                                    .selectable(false),
                            );
                            let (line, color) = state_line(state, group.tunnels.len());
                            ui.add(
                                egui::Label::new(RichText::new(line).size(11.0).color(color))
                                    .selectable(false),
                            );
                        },
                    );
                    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let active = state != GroupState::Stopped;
                        let mut on = active;
                        let name = format!("Run {}", group.name);
                        let response = if is_selected {
                            switch_raised(ui, &mut on, &name)
                        } else {
                            switch(ui, &mut on, &name)
                        };
                        let tip = if active {
                            "Stop this group (tunnels other owners use stay up)"
                        } else {
                            "Start this group"
                        };
                        let response = popup::tip_below(response, |ui| {
                            ui.label(tip);
                        });
                        let changed = match tour_id {
                            Some(id) => {
                                targets::changed(ui.ctx(), Target::GroupSwitch(id), &response)
                            }
                            None => response.changed(),
                        };
                        if changed {
                            commands.push(if active {
                                Command::StopGroup(group.id.clone())
                            } else {
                                Command::StartGroup(group.id.clone())
                            });
                        }
                    });
                    response.on_hover_cursor(CursorIcon::PointingHand);
                }
                if let Some((index, rect)) = selected_rect {
                    let target = motion::tween_rect(
                        ui.ctx(),
                        Id::unique("group_selection"),
                        index,
                        rect,
                        timing::SLIDE,
                    );
                    ui.painter().set(
                        selection_slot,
                        Shape::rect_filled(target, radius::CONTROL, p.selection),
                    );
                }
                let hovered = hovered.filter(|(i, _)| selected_rect.map(|(s, _)| s) != Some(*i));
                nav::hover_highlight(
                    ui,
                    Id::unique("group_hover"),
                    hover_slot,
                    hovered,
                    p.surface,
                );
            });
    }

    fn detail(
        &mut self,
        ui: &mut Ui,
        group: &Group,
        data: &AppData,
        states: &Snapshot,
        commands: &mut Vec<Command>,
    ) {
        let p = &Palette::ONE_DARK;
        let state = states.group_state(group);
        let active = state != GroupState::Stopped;
        ui.spacing_mut().item_spacing.y = 20.0;

        // Header: name and chips, how a start works; the actions.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new(&group.name)
                            .font(fonts::semibold(22.0))
                            .color(p.text_strong),
                    );
                    if group.start_on_launch {
                        starts_with_sshelf(ui);
                    }
                    let (chip, fill, color) = state_chip(state, group.tunnels.len());
                    marks::chip(ui, &chip, fill, color);
                });
                ui.label(
                    RichText::new(
                        "Starting checks every port first, then starts all tunnels in parallel.",
                    )
                    .size(13.0)
                    .color(p.dim),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // Right to left: start / stop ends up rightmost.
                if active {
                    if ui
                        .add(
                            Button::new("Stop group")
                                .icon(icons::SQUARE)
                                .tip("Tunnels other owners use stay up"),
                        )
                        .clicked()
                    {
                        commands.push(Command::StopGroup(group.id.clone()));
                    }
                } else if ui
                    .add(Button::new("Start group").icon(icons::PLAY).primary())
                    .clicked()
                {
                    commands.push(Command::StartGroup(group.id.clone()));
                }
                let edit_tip = if active {
                    "Stop the group before editing it"
                } else {
                    "Change its name, tunnels or start"
                };
                let response = ui.add_enabled(!active, Button::new("Edit").tip(edit_tip));
                if targets::clicked(ui.ctx(), Target::GroupEdit, &response) && !active {
                    self.editor = Some(group.clone());
                }
                if matches!(state, GroupState::Degraded { .. })
                    && ui
                        .add(Button::new("Retry failed").icon(icons::ROTATE_CW))
                        .clicked()
                {
                    commands.push(Command::StartGroup(group.id.clone()));
                }
            });
        });

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Tunnels in this group")
                        .font(fonts::semibold(13.0))
                        .color(p.text_strong),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let tip = if active {
                        "Stop the group before changing its tunnels"
                    } else {
                        "Pick more tunnels in the group's editor"
                    };
                    if ui
                        .add_enabled(
                            !active,
                            Button::new("Add tunnel").icon(icons::PLUS).small().tip(tip),
                        )
                        .clicked()
                    {
                        self.editor = Some(group.clone());
                    }
                });
            });
            for id in &group.tunnels {
                let Some(tunnel) = data.tunnels.tunnel(id) else {
                    marks::banner(ui, p.yellow_tint, |ui| {
                        icons::label(
                            ui,
                            icons::TRIANGLE_ALERT,
                            p.yellow,
                            RichText::new(format!("Missing tunnel `{id}` (see the orphans)"))
                                .color(p.yellow),
                        );
                    });
                    continue;
                };
                self.tunnel_row(ui, tunnel, states);
            }
        });
        shared_note(ui, group, data, states, active);
    }

    /// One tunnel of the group: mark, name, host, forward, state; a click
    /// shows it in Tunnels. A failure follows on its own strip.
    fn tunnel_row(&mut self, ui: &mut Ui, tunnel: &Tunnel, states: &Snapshot) {
        let p = &Palette::ONE_DARK;
        let state = states.state(&tunnel.id);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), TUNNEL_ROW), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Link, true, &tunnel.name));
        let hover = motion::fade(
            ui.ctx(),
            response.id.with("hover"),
            response.hovered(),
            timing::HOVER,
            motion::ease_out,
        );
        ui.painter().rect_filled(
            rect,
            radius::CONTROL,
            p.surface.lerp_to_gamma(p.selection, hover),
        );
        // Columns: mark 16 | name | host | forward 160 | state 96.
        const GAP: f32 = 12.0;
        let inner = rect.shrink2(vec2(12.0, 0.0));
        let flex = ((inner.width() - 16.0 - 160.0 - 96.0 - 4.0 * GAP) / 2.0).max(40.0);
        let mut mark_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    pos2(inner.left(), rect.top()),
                    vec2(16.0, rect.height()),
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        mark(&mut mark_ui, &state);
        let painter = ui.painter();
        let y = rect.center().y;
        let mut x = inner.left() + 16.0 + GAP;
        let cell = |x: f32, text: &str, font: FontId, color: Color32, width: f32| {
            let (text, _) = text::elide(painter, text, &font, width);
            painter.text(pos2(x, y), Align2::LEFT_CENTER, text, font, color);
        };
        cell(
            x,
            &tunnel.name,
            FontId::proportional(13.0),
            p.text_strong,
            flex,
        );
        x += flex + GAP;
        cell(x, &tunnel.host, FontId::monospace(13.0), p.dim, flex);
        x += flex + GAP;
        // A clickable row: a masked forward shows as dots, not a token.
        cell(
            x,
            &forwards_short(tunnel, privacy::get(ui.ctx()).on),
            FontId::monospace(12.0),
            p.text,
            160.0,
        );
        x += 160.0 + GAP;
        let (label, color) = state_label(&state);
        cell(
            x,
            &capitalized(&label),
            FontId::proportional(12.0),
            color,
            96.0,
        );
        if popup::tip_below(response, |ui| {
            ui.label("Show in Tunnels");
        })
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
        {
            self.open_tunnel = Some(tunnel.id.clone());
        }

        if let TunnelState::Failed(reason) = &state {
            Frame::new()
                .fill(p.surface)
                .corner_radius(radius::CONTROL)
                .inner_margin(Margin {
                    left: 40,
                    right: 12,
                    top: 10,
                    bottom: 10,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let style = |color| masked::Style {
                        font: FontId::proportional(12.0),
                        color,
                        on_card: true,
                    };
                    masked::text(ui, reason, &style(p.red), true);
                    if let Some(text) = reconnect_text(ui.ctx(), states.tunnels.get(&tunnel.id)) {
                        ui.label(RichText::new(text).size(12.0).color(p.yellow));
                    }
                    if let Some(hint) = hints::failure_hint(reason) {
                        masked::text(ui, hint, &style(p.dim), true);
                    }
                });
        }
    }

    fn editor(&mut self, ui: &mut Ui, data: &AppData, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        let Some(draft) = &mut self.editor else {
            return;
        };
        let is_new = data.tunnels.group(&draft.id).is_none();
        let mut problems = Vec::new();
        if draft.name.trim().is_empty() {
            problems.push("the name is empty");
        }
        if draft.tunnels.is_empty() {
            problems.push("choose at least one tunnel");
        }
        let mut close = false;
        let mut save = false;
        let mut delete = false;

        // The footer first, so the form gets the space above it.
        egui::Panel::bottom("group_editor_footer")
            .show_separator_line(false)
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let line = ui.available_rect_before_wrap();
                ui.painter()
                    .hline(line.x_range(), line.top(), Stroke::new(1.0, p.selection));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let why = problems.first().map(|p| capitalized(p));
                    let mut save_button = Button::new("Save").primary();
                    if let Some(why) = &why {
                        save_button = save_button.tip(why);
                    }
                    let response = ui.add_enabled(problems.is_empty(), save_button);
                    save = targets::clicked(ui.ctx(), Target::GroupSave, &response)
                        && problems.is_empty();
                    close = ui.add(Button::new("Cancel").ghost()).clicked();
                    if !problems.is_empty() {
                        let count = match problems.len() {
                            1 => "1 problem".to_string(),
                            n => format!("{n} problems"),
                        };
                        ui.label(RichText::new(count).size(12.0).color(p.red));
                    }
                    if !is_new {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            delete = ui.add(Button::new("Delete group…").ghost()).clicked();
                        });
                    }
                });
                ui.add_space(16.0);
            });

        targets::register(ui.ctx(), Target::GroupEditor, ui.max_rect());
        egui::ScrollArea::vertical()
            .id_salt("group_editor")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                ui.label(
                    RichText::new(if is_new { "New group" } else { "Edit group" })
                        .font(fonts::semibold(20.0))
                        .color(p.text_strong),
                );
                ui.add_space(2.0);
                editor_row(ui, ("Name", false, LABEL), |ui, label| {
                    let empty = draft.name.trim().is_empty();
                    ui.add(
                        TextField::new(&mut draft.name)
                            .hint("e.g. Work on prod")
                            .width(FIELD)
                            .error(empty),
                    )
                    .labelled_by(label);
                    if empty {
                        field_errors(ui, &["The name is empty".to_string()]);
                    }
                });
                editor_row(ui, ("Start with sshelf", true, LABEL), |ui, _| {
                    // As tall as a field, so it lines up with its label.
                    let size = vec2(FIELD, 32.0);
                    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        switch(ui, &mut draft.start_on_launch, "Start when sshelf starts");
                        ui.label(
                            RichText::new(
                                "starts this group when the app opens (also from the tray)",
                            )
                            .size(12.0)
                            .color(p.dim),
                        );
                    });
                });
                editor_row(ui, ("Tunnels", true, LABEL), |ui, _| {
                    let tunnels = &data.tunnels.tunnels;
                    let choices: Vec<Choice<usize>> = tunnels
                        .iter()
                        .enumerate()
                        .map(|(i, t)| Choice {
                            note: Some(t.host.clone()),
                            ..Choice::new(i, &t.name)
                        })
                        .collect();
                    let mut picked: Vec<usize> = draft
                        .tunnels
                        .iter()
                        .filter_map(|id| tunnels.iter().position(|t| &t.id == id))
                        .collect();
                    let changed = Dropdown::with_choices(
                        "group_tunnels",
                        "Tunnels",
                        choices,
                        "choose tunnels",
                    )
                    .field(FIELD)
                    .mono_notes()
                    .show_multi(ui, &mut picked);
                    if changed {
                        // Ids of missing tunnels stay (the orphans view
                        // relinks or removes them).
                        let mut ids: Vec<String> = draft
                            .tunnels
                            .iter()
                            .filter(|id| data.tunnels.tunnel(id).is_none())
                            .cloned()
                            .collect();
                        ids.extend(picked.iter().map(|&i| tunnels[i].id.clone()));
                        draft.tunnels = ids;
                    }
                    if draft.tunnels.is_empty() {
                        field_errors(ui, &["Choose at least one tunnel".to_string()]);
                    } else {
                        ui.label(
                            RichText::new(
                                "A tunnel can be in several groups; it runs while any of them \
                                 (or a manual start) holds it.",
                            )
                            .size(12.0)
                            .color(p.dim),
                        );
                    }
                });
            });

        if save {
            commands.push(Command::SaveGroup(draft.clone()));
            self.selected = Some(draft.id.clone());
            close = true;
        }
        if delete {
            self.confirm_delete = Some(draft.id.clone());
        }
        if close {
            self.editor = None;
        }
    }

    /// "Delete group “x”?"; *Cancel* (grey) has the focus (SPEC.md §10
    /// *Destructive confirmations*).
    fn delete_dialog(&mut self, ctx: &egui::Context, data: &AppData, commands: &mut Vec<Command>) {
        let p = &Palette::ONE_DARK;
        let Some(group) = self
            .confirm_delete
            .as_ref()
            .and_then(|id| data.tunnels.group(id))
            .cloned()
        else {
            self.confirm_delete = None;
            return;
        };
        let mut close = false;
        let response = egui::Modal::new(Id::unique("delete_group"))
            .backdrop_color(p.overlay)
            .frame(
                Frame::new()
                    .fill(p.bg_dark)
                    .stroke(Stroke::new(1.0, p.selection))
                    .corner_radius(radius::LARGE)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(412.0);
                ui.spacing_mut().item_spacing.y = 14.0;
                ui.label(
                    RichText::new(format!("Delete group \u{201c}{}\u{201d}?", group.name))
                        .font(fonts::semibold(17.0))
                        .color(p.text_strong),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Its tunnels are kept; only the group is removed from tunnels.toml.",
                        )
                        .size(13.0),
                    )
                    .wrap(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new("Delete").danger().dialog()).clicked() {
                        commands.push(Command::DeleteGroup(group.id.clone()));
                        self.selected = None;
                        self.editor = None;
                        close = true;
                    }
                    let cancel = ui.add(Button::new("Cancel").dialog());
                    if ui.memory(|m| m.focused().is_none()) {
                        cancel.request_focus();
                    }
                    close |= cancel.clicked();
                });
            });
        if close || response.should_close() {
            self.confirm_delete = None;
        }
    }
}

/// The "starts with sshelf" chip (Lucide `power`).
fn starts_with_sshelf(ui: &mut Ui) {
    let p = &Palette::ONE_DARK;
    let font = FontId::proportional(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap("starts with sshelf".into(), font, p.text);
    let size = vec2(galley.size().x + 12.0 + 12.0 + 4.0, galley.size().y + 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, p.surface);
    painter.text(
        pos2(rect.left() + 6.0 + 6.0, rect.center().y),
        Align2::CENTER_CENTER,
        icons::POWER,
        icons::font(12.0),
        p.text,
    );
    painter.galley(
        pos2(rect.left() + 6.0 + 12.0 + 4.0, rect.top() + 2.0),
        galley,
        p.text,
    );
    popup::tip_below(response, |ui| {
        ui.label("Starts when sshelf opens; host keys are still checked first");
    });
}

/// What stopping does to tunnels others use: while running, the tunnels
/// other groups share; while stopped, the ones still up for other owners.
fn shared_note(ui: &mut Ui, group: &Group, data: &AppData, states: &Snapshot, active: bool) {
    let p = &Palette::ONE_DARK;
    let owner = Owner::Group(group.id.clone());
    let named = |id: &str| {
        data.tunnels
            .tunnel(id)
            .map(|t| format!("\u{201c}{}\u{201d}", t.name))
    };
    let (title, lines): (&str, Vec<String>) = if active {
        let lines = group
            .tunnels
            .iter()
            .filter_map(|id| {
                let others: Vec<String> = data
                    .tunnels
                    .groups_using(id)
                    .iter()
                    .filter(|g| g.id != group.id)
                    .map(|g| g.name.clone())
                    .collect();
                if others.is_empty() {
                    return None;
                }
                Some(format!(
                    "{} is also used by {}; it stays up while that runs.",
                    named(id)?,
                    others.join(", ")
                ))
            })
            .collect();
        ("Stopping keeps shared tunnels", lines)
    } else {
        let lines = group
            .tunnels
            .iter()
            .filter(|id| {
                matches!(
                    states.state(id),
                    TunnelState::Running | TunnelState::Starting
                )
            })
            .filter_map(|id| {
                let others: Vec<&Owner> = states
                    .tunnels
                    .get(id)?
                    .owners
                    .iter()
                    .filter(|o| **o != owner)
                    .collect();
                Some(format!(
                    "{}: still used by {}",
                    named(id)?,
                    owner_names(&others, &data.tunnels)
                ))
            })
            .collect();
        ("Still running for other owners", lines)
    };
    if lines.is_empty() {
        return;
    }
    Frame::new()
        .fill(p.bg_dark)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(
                RichText::new(title)
                    .font(fonts::semibold(12.0))
                    .color(p.text_strong),
            );
            for line in lines {
                ui.add(egui::Label::new(RichText::new(line).size(12.0).color(p.dim)).wrap());
            }
        });
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};
    use std::path::PathBuf;
    use std::sync::Arc;

    use egui::accesskit;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use sshelf_core::supervisor::TunnelStatus;

    use super::*;
    use crate::test_util;

    fn data() -> Arc<AppData> {
        let mut data = AppData::new(PathBuf::from("app"));
        for (id, name) in [("t-a", "API"), ("t-b", "DB")] {
            data.tunnels.upsert_tunnel(Tunnel {
                id: id.into(),
                name: name.into(),
                host: "h".into(),
                forwards: Vec::new(),
                auto_reconnect: false,
            });
        }
        data.tunnels.upsert_group(Group {
            id: "g-1".into(),
            name: "Work".into(),
            tunnels: vec!["t-a".into(), "t-b".into()],
            start_on_launch: true,
        });
        data.tunnels.upsert_group(Group {
            id: "g-2".into(),
            name: "Other".into(),
            tunnels: vec!["t-b".into()],
            start_on_launch: false,
        });
        Arc::new(data)
    }

    fn status(state: TunnelState, owners: &[&str]) -> TunnelStatus {
        TunnelStatus {
            state,
            owners: owners
                .iter()
                .map(|g| Owner::Group(g.to_string()))
                .collect::<BTreeSet<_>>(),
            log: VecDeque::new(),
            pid: None,
            failure: None,
            reconnect: None,
            running_since: None,
        }
    }

    fn harness_with(
        data: Arc<AppData>,
        states: Snapshot,
        view: GroupsView,
    ) -> Harness<'static, (GroupsView, Vec<Command>)> {
        let states = Arc::new(states);
        test_util::harness()
            .with_size([1100.0, 800.0])
            .build_ui_state(
                move |ui, (view, commands): &mut (GroupsView, Vec<Command>)| {
                    commands.extend(view.show(ui, Some(&data), Some(&states)));
                },
                (view, Vec::new()),
            )
    }

    fn harness(states: Snapshot) -> Harness<'static, (GroupsView, Vec<Command>)> {
        harness_with(data(), states, GroupsView::default())
    }

    fn selected(id: &str) -> GroupsView {
        let mut view = GroupsView::default();
        view.select(id);
        view
    }

    #[test]
    fn the_switch_starts_a_stopped_group() {
        let mut harness = harness(Snapshot::default());
        harness.step();
        harness.get_by_role_and_label(accesskit::Role::Button, "Work");
        assert_eq!(harness.get_all_by_label("stopped").count(), 2);
        harness
            .get_by_role_and_label(accesskit::Role::Switch, "Run Work")
            .click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartGroup("g-1".into())]);
    }

    #[test]
    fn the_switch_stops_a_running_group() {
        let states = Snapshot {
            tunnels: [("t-b".to_string(), status(TunnelState::Running, &["g-2"]))].into(),
        };
        let mut harness = harness(states);
        harness.step();
        harness.get_by_label("running · 1/1");
        harness
            .get_by_role_and_label(accesskit::Role::Switch, "Run Other")
            .click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StopGroup("g-2".into())]);
    }

    #[test]
    fn a_degraded_group_offers_retry_and_shows_failures_and_shared_tunnels() {
        let states = Snapshot {
            tunnels: [
                (
                    "t-a".to_string(),
                    status(TunnelState::Failed("Permission denied".into()), &["g-1"]),
                ),
                (
                    "t-b".to_string(),
                    status(TunnelState::Running, &["g-1", "g-2"]),
                ),
            ]
            .into(),
        };
        let mut harness = harness_with(data(), states, selected("g-1"));
        harness.step();
        harness.get_by_label("degraded · 1/2");
        harness.get_by_label("Permission denied");
        harness.get_by_label("Stopping keeps shared tunnels");
        harness
            .get_by_label("\u{201c}DB\u{201d} is also used by Other; it stays up while that runs.");
        assert!(harness.get_by_label("Edit").accesskit_node().is_disabled());

        harness.get_by_label("Retry failed").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartGroup("g-1".into())]);
    }

    #[test]
    fn a_stopped_group_says_what_still_runs_for_others() {
        let states = Snapshot {
            tunnels: [("t-b".to_string(), status(TunnelState::Running, &["g-2"]))].into(),
        };
        let mut harness = harness_with(data(), states, selected("g-1"));
        harness.step();
        harness.get_by_label("Still running for other owners");
        harness.get_by_label("\u{201c}DB\u{201d}: still used by \"Other\"");
        harness.get_by_label("Start group").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::StartGroup("g-1".into())]);
    }

    #[test]
    fn a_tunnel_row_opens_the_tunnel() {
        let mut harness = harness_with(data(), Snapshot::default(), selected("g-2"));
        harness.step();
        harness
            .get_by_role_and_label(accesskit::Role::Link, "DB")
            .click();
        harness.step();
        assert_eq!(
            harness.state_mut().0.take_open_tunnel().as_deref(),
            Some("t-b")
        );
    }

    #[test]
    fn new_group_is_disabled_without_tunnels() {
        let data = Arc::new(AppData::new(PathBuf::from("app")));
        let mut harness = harness_with(data, Snapshot::default(), GroupsView::default());
        harness.step();
        assert!(
            harness
                .get_by_label("New group")
                .accesskit_node()
                .is_disabled()
        );
        harness.get_by_label("No groups yet");
    }

    #[test]
    fn the_editor_picks_tunnels_from_the_multi_select() {
        let mut harness = harness(Snapshot::default());
        harness.step();
        harness.get_by_label("New group").click();
        harness.step();
        harness.step();
        harness.get_by_label("Choose at least one tunnel");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
        harness.state_mut().0.editor.as_mut().unwrap().name = "New".into();
        harness.step();
        harness.get_by_label("Tunnels").click();
        harness.step();
        harness.step();
        harness.get_by_label("0 selected");
        harness.get_by_label("DB").click();
        harness.step();
        harness.step();
        harness.get_by_label("1 selected");
        harness.get_by_label("API").click();
        harness.step();
        harness.step();
        harness.get_by_label("2 selected");
        harness.get_by_label("API").click();
        harness.step();
        harness.step();
        harness.get_by_label("Done").click();
        harness.step();
        harness
            .get_by_role_and_label(accesskit::Role::Switch, "Start when sshelf starts")
            .click();
        harness.step();
        harness.step();
        harness.get_by_label("Save").click();
        harness.step();
        let commands = &harness.state().1;
        let [Command::SaveGroup(group)] = commands.as_slice() else {
            panic!("{commands:?}");
        };
        assert_eq!(group.name, "New");
        assert_eq!(group.tunnels, ["t-b"]);
        assert!(group.id.starts_with("g-"));
        assert!(group.start_on_launch);
    }

    #[test]
    fn deleting_asks_first_with_cancel_as_the_main_button() {
        let mut view = selected("g-2");
        view.editor = Some(data().tunnels.group("g-2").unwrap().clone());
        let mut harness = harness_with(data(), Snapshot::default(), view);
        harness.step();
        harness.get_by_label("Delete group…").click();
        harness.step();
        harness.step();
        harness.get_by_label("Delete group \u{201c}Other\u{201d}?");
        harness.get_by_label("Delete").click();
        harness.step();
        assert_eq!(harness.state().1, [Command::DeleteGroup("g-2".into())]);
    }
}
