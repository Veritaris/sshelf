//! The dropdown (SPEC.md §10 *Dropdown*): the panel rolls open from the
//! trigger by clipping (200 ms "out", closing 140 ms "in"), upward when there
//! is no room below; 30 px rows (36 px with a second line, as *Save to* has)
//! with the sliding hover highlight; disabled rows with their reason; ↑↓
//! Enter Esc. Single-select shows a Magenta check on the selected row and
//! closes on a pick; multi-select has checkboxes, stays open, a footer
//! ("N selected", *Clear*, *Done*) and shows the selection as chips.

use egui::{
    Align2, Area, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers, Order, Rect, Response,
    Role, Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};

use crate::theme::{Palette, radius, timing};
use crate::widgets::button::Button;
use crate::widgets::{nav, popup, text};
use crate::{fonts, icons, motion};

const ROW: f32 = 30.0;
/// A row with a second line.
const TALL_ROW: f32 = 36.0;
/// Space between the trigger and the panel.
const GAP: f32 = 6.0;
const OPEN: f32 = 0.200;
const CLOSE: f32 = 0.140;
/// The check mark's fade.
const CHECK: f32 = 0.120;
/// The multi-select footer under its line.
const FOOTER: f32 = 41.0;
/// A checkbox (multi-select rows).
const CHECKBOX: f32 = 16.0;
/// A chip in the closed multi-select control.
const CHIP: f32 = 22.0;

#[derive(Debug, Clone, Default)]
struct State {
    open: bool,
    /// The keyboard / pointer highlight (an index into the choices).
    highlight: usize,
    /// The search field's text (searchable dropdowns).
    query: String,
    /// The search field takes focus on the next frame (just opened).
    focus_search: bool,
}

/// Height of the search field's row.
const SEARCH: f32 = 40.0;

/// One option: its value, label, an optional second line, and why it
/// can't be picked (a disabled row shows a lock and the reason instead).
#[derive(Debug, Clone)]
pub struct Choice<T> {
    pub value: T,
    pub label: String,
    pub detail: Option<String>,
    pub disabled: Option<String>,
    /// A Lucide icon before the label (actions such as *Browse…*).
    pub icon: Option<char>,
    /// A dim note at the right end (e.g. "recommended").
    pub note: Option<String>,
    /// Starts a group: a separator and this overline above the row.
    pub group: Option<String>,
}

impl<T> Choice<T> {
    /// A plain row: just its value and label.
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            detail: None,
            disabled: None,
            icon: None,
            note: None,
            group: None,
        }
    }
}

/// Height of a group's separator and overline.
const GROUP: f32 = 30.0;

/// How the closed control looks.
#[derive(Debug, Clone, Copy)]
enum Trigger {
    /// A small raised button (e.g. "Group by tag").
    Button,
    /// An input-like field of this width (form selects).
    Field(f32),
}

/// What happened in the open panel this frame.
#[derive(Default)]
struct Outcome {
    /// A row was picked (single) or toggled (multi).
    picked: Option<usize>,
    /// *Clear* (multi).
    clear: bool,
    /// A typed value was used ([`Dropdown::show_typed`]).
    typed: Option<String>,
}

/// What [`Dropdown::show_typed`] returns: an option, or the text typed into
/// the search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked<T> {
    Choice(T),
    Typed(String),
}

/// The caption under the rows ([`Dropdown::with_caption`]).
const CAPTION: f32 = 30.0;

/// A dropdown over `options`; [`Dropdown::show`] for single-select,
/// [`Dropdown::show_multi`] for multi-select.
pub struct Dropdown<'a, T> {
    id: Id,
    name: &'a str,
    choices: Vec<Choice<T>>,
    /// The closed control's text, e.g. "Group by tag"; for multi-select the
    /// placeholder while nothing is selected.
    trigger: String,
    look: Trigger,
    mono: bool,
    /// Notes in mono (aliases, as the group editor's tunnel hosts).
    mono_notes: bool,
    max_visible: usize,
    /// The panel's least width (it is never narrower than the trigger).
    panel_width: f32,
    /// Searchable: what the options are, for "No hosts match".
    search: Option<&'a str>,
    multi: bool,
    /// A row above the options: still listing them (`true`, with the
    /// pulsing ring), or why they couldn't be listed.
    status: Option<(String, bool)>,
    /// A dim caption under the rows (where the options come from).
    caption: Option<String>,
    /// The search text can be used as a value ([`Dropdown::show_typed`]).
    typed: bool,
}

impl<'a, T: Copy + PartialEq> Dropdown<'a, T> {
    /// `name` is the control's accessible name; `trigger` its text.
    pub fn new(
        id: impl std::hash::Hash + std::fmt::Debug,
        name: &'a str,
        options: &[(T, &str)],
        trigger: impl Into<String>,
    ) -> Self {
        let choices = options
            .iter()
            .map(|(value, label)| Choice::new(*value, *label))
            .collect();
        Self::with_choices(id, name, choices, trigger)
    }

    /// Options with second lines or disabled rows.
    pub fn with_choices(
        id: impl std::hash::Hash + std::fmt::Debug,
        name: &'a str,
        choices: Vec<Choice<T>>,
        trigger: impl Into<String>,
    ) -> Self {
        Self {
            id: Id::unique(id),
            name,
            choices,
            trigger: trigger.into(),
            look: Trigger::Button,
            mono: false,
            mono_notes: false,
            max_visible: 10,
            panel_width: 180.0,
            search: None,
            multi: false,
            status: None,
            caption: None,
            typed: false,
        }
    }

    /// The options are still being listed: a row with the pulsing ring and
    /// `text` above them (SPEC.md §10 *kubectl tunnels*).
    pub fn loading(mut self, text: impl Into<String>) -> Self {
        self.status = Some((text.into(), true));
        self
    }

    /// The options couldn't be listed: `reason` in a row above them; typing
    /// still works with [`Dropdown::show_typed`].
    pub fn error(mut self, reason: impl Into<String>) -> Self {
        self.status = Some((reason.into(), false));
        self
    }

    /// A dim caption under the rows (e.g. where the options come from).
    pub fn with_caption(mut self, text: impl Into<String>) -> Self {
        self.caption = Some(text.into());
        self
    }

    /// The trimmed search text, if it can be used as typed: not empty and
    /// no option's label exactly.
    fn usable_query(&self, query: &str) -> Option<String> {
        let query = query.trim();
        (self.typed && !query.is_empty() && !self.choices.iter().any(|c| c.label == query))
            .then(|| query.to_string())
    }

    /// A search field at the top, focused on open; typing filters by
    /// case-insensitive substring of the label or second line. `what` the
    /// options are, for "No <what> match".
    pub fn searchable(mut self, what: &'a str) -> Self {
        self.search = Some(what);
        self
    }

    /// The choices the search leaves (all without a search).
    fn visible(&self, query: &str) -> Vec<usize> {
        let needle = query.trim().to_lowercase();
        self.choices
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                needle.is_empty()
                    || c.label.to_lowercase().contains(&needle)
                    || c.detail
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(&needle))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// A wider panel than the default 180 px (long labels).
    pub fn panel_width(mut self, width: f32) -> Self {
        self.panel_width = width;
        self
    }

    /// An input-like closed control `width` wide.
    pub fn field(mut self, width: f32) -> Self {
        self.look = Trigger::Field(width);
        self
    }

    /// Labels in mono (paths).
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Notes in mono (aliases).
    pub fn mono_notes(mut self) -> Self {
        self.mono_notes = true;
        self
    }

    fn row_height(&self) -> f32 {
        let tall = self
            .choices
            .iter()
            .any(|c| c.detail.is_some() || c.disabled.is_some());
        if tall { TALL_ROW } else { ROW }
    }

    fn label_font(&self) -> FontId {
        if self.mono {
            FontId::monospace(12.0)
        } else {
            FontId::proportional(13.0)
        }
    }

    /// Single-select: shows the closed control and, while open, the panel.
    /// Returns the option picked this frame.
    pub fn show(self, ui: &mut Ui, selected: T) -> Option<T> {
        // None of them may be the current one (an "add" menu): no check.
        let selected: Vec<usize> = self
            .choices
            .iter()
            .position(|c| c.value == selected)
            .into_iter()
            .collect();
        let outcome = self.run(ui, &selected, &[]);
        outcome.picked.map(|i| self.choices[i].value)
    }

    /// Single-select whose search text can also be used as the value: a row
    /// "Use “…”", and Enter while nothing matches.
    pub fn show_typed(mut self, ui: &mut Ui, selected: T) -> Option<Picked<T>> {
        self.typed = true;
        let selected: Vec<usize> = self
            .choices
            .iter()
            .position(|c| c.value == selected)
            .into_iter()
            .collect();
        let outcome = self.run(ui, &selected, &[]);
        match (outcome.typed, outcome.picked) {
            (Some(text), _) => Some(Picked::Typed(text)),
            (None, Some(i)) => Some(Picked::Choice(self.choices[i].value)),
            (None, None) => None,
        }
    }

    /// Multi-select: rows with checkboxes; toggling keeps the panel open.
    /// The closed control shows `selected` as chips (the rest as "+N").
    /// Returns whether `selected` changed.
    pub fn show_multi(mut self, ui: &mut Ui, selected: &mut Vec<T>) -> bool {
        self.multi = true;
        let indexes: Vec<usize> = selected
            .iter()
            .filter_map(|v| self.choices.iter().position(|c| c.value == *v))
            .collect();
        let labels: Vec<String> = indexes
            .iter()
            .map(|&i| self.choices[i].label.clone())
            .collect();
        let outcome = self.run(ui, &indexes, &labels);
        if outcome.clear && !selected.is_empty() {
            selected.clear();
            return true;
        }
        if let Some(index) = outcome.picked {
            let value = self.choices[index].value;
            if let Some(at) = selected.iter().position(|v| *v == value) {
                selected.remove(at);
            } else {
                selected.push(value);
            }
            return true;
        }
        false
    }

    /// The control and its panel; `chips` the multi-select's selection.
    fn run(&self, ui: &mut Ui, selected: &[usize], chips: &[String]) -> Outcome {
        let p = &Palette::ONE_DARK;
        let ctx = ui.ctx().clone();
        let mut state = ctx
            .data(|d| d.get_temp::<State>(self.id))
            .unwrap_or_default();
        let trigger = self.trigger(ui, state.open, chips);
        let mut outcome = Outcome::default();
        let mut close = false;

        let keyboard_open = trigger.has_focus()
            && ctx.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::Space)
                    || i.consume_key(Modifiers::NONE, Key::ArrowDown)
            });
        if (trigger.clicked() || keyboard_open) && !state.open {
            state.open = true;
            state.highlight = selected.first().copied().unwrap_or(0);
            state.query.clear();
            state.focus_search = self.search.is_some();
        } else if trigger.clicked() {
            state.open = false;
        }
        let visible = self.visible(&state.query);
        if state.open {
            // ↑↓ move through what the search leaves.
            let at = visible.iter().position(|&i| i == state.highlight);
            let pickable = visible.contains(&state.highlight)
                && self
                    .choices
                    .get(state.highlight)
                    .is_some_and(|c| c.disabled.is_none());
            ctx.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    let next = at.map_or(0, |at| (at + 1).min(visible.len().saturating_sub(1)));
                    state.highlight = visible.get(next).copied().unwrap_or(state.highlight);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    let next = at.map_or(0, |at| at.saturating_sub(1));
                    state.highlight = visible.get(next).copied().unwrap_or(state.highlight);
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) {
                    // A match is picked; with nothing matching, the text.
                    if pickable {
                        outcome.picked = Some(state.highlight);
                    } else if let Some(text) = self.usable_query(&state.query) {
                        outcome.typed = Some(text);
                    }
                }
                // Space toggles in a multi-select (with a search, it types).
                if self.multi
                    && self.search.is_none()
                    && i.consume_key(Modifiers::NONE, Key::Space)
                    && pickable
                {
                    outcome.picked = Some(state.highlight);
                }
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    state.open = false;
                }
            });
        }

        let shown = motion::fade(
            &ctx,
            self.id.with("roll"),
            state.open,
            if state.open { OPEN } else { CLOSE },
            if state.open {
                motion::ease_out
            } else {
                motion::ease_in
            },
        );
        if shown > 0.0 {
            let row = self.row_height();
            let searching = !state.query.trim().is_empty();
            // At least one row: "No … match".
            let rows = visible.len().clamp(1, self.max_visible);
            let groups = if searching {
                0
            } else {
                self.choices.iter().filter(|c| c.group.is_some()).count()
            };
            let search = if self.search.is_some() { SEARCH } else { 0.0 };
            let mut footer = if self.multi { FOOTER } else { 0.0 };
            if self.caption.is_some() {
                footer += CAPTION;
            }
            // The status row and the "Use “…”" row.
            let extra = usize::from(self.status.is_some())
                + usize::from(self.usable_query(&state.query).is_some());
            let rows = if visible.is_empty() && extra > 0 {
                extra
            } else {
                rows + extra
            };
            let height = rows as f32 * row + groups as f32 * GROUP + search + footer + 8.0;
            let width = trigger.rect.width().max(self.panel_width);
            let below = trigger.rect.bottom() + GAP;
            let room_below = ctx.content_rect().bottom() - below;
            let upward = room_below < height && trigger.rect.top() - GAP > height;
            let top = if upward {
                trigger.rect.top() - GAP - height
            } else {
                below
            };
            let panel = Rect::from_min_size(pos2(trigger.rect.left(), top), vec2(width, height));
            // Revealed from the trigger's side by clipping.
            let revealed = height * shown;
            let clip = if upward {
                Rect::from_min_max(pos2(panel.left(), panel.bottom() - revealed), panel.max)
            } else {
                Rect::from_min_max(panel.min, pos2(panel.right(), panel.top() + revealed))
            };
            let area = Area::new(self.id.with("panel"))
                .order(Order::Foreground)
                .fixed_pos(panel.min)
                .fade_in(false)
                .show(&ctx, |ui| {
                    ui.set_clip_rect(clip.expand(1.0));
                    Frame::new()
                        .fill(p.bg_dark)
                        .stroke(Stroke::new(1.0, p.selection))
                        .corner_radius(radius::CONTROL)
                        .inner_margin(Margin::symmetric(4, 4))
                        .show(ui, |ui| {
                            ui.set_width(width - 8.0);
                            if self.search.is_some() {
                                self.search_field(ui, &mut state);
                            }
                            let rows_height = height - 8.0 - search - footer;
                            if let Some(index) = self.rows(
                                ui,
                                &mut state,
                                (selected, &visible),
                                (rows_height, &mut outcome.typed),
                            ) {
                                outcome.picked = Some(index);
                            }
                            if let Some(caption) = &self.caption {
                                self.caption(ui, caption);
                            }
                            if self.multi {
                                let shown_count = self.search.map(|_| visible.len());
                                let (clear, done) = self.footer(ui, selected.len(), shown_count);
                                outcome.clear = clear;
                                close |= done;
                            }
                        });
                });
            // A click anywhere else closes it.
            let outside = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.interact_pos().is_some_and(|pos| {
                        !area.response.rect.contains(pos) && !trigger.rect.contains(pos)
                    })
            });
            if outside {
                state.open = false;
            }
        }
        if close || ((outcome.picked.is_some() || outcome.typed.is_some()) && !self.multi) {
            state.open = false;
        }
        ctx.data_mut(|d| d.insert_temp(self.id, state));
        outcome
    }

    /// The caption under the rows, below a line.
    fn caption(&self, ui: &mut Ui, caption: &str) {
        let p = &Palette::ONE_DARK;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), CAPTION), Sense::hover());
        ui.painter().hline(
            (rect.left() - 4.0)..=(rect.right() + 4.0),
            rect.top() + 4.0,
            Stroke::new(1.0, p.selection),
        );
        let font = FontId::proportional(11.0);
        let (shown, _) = text::elide(ui.painter(), caption, &font, rect.width() - 20.0);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(rect.left() + 10.0, rect.top() + 5.0),
                    rect.max,
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.add(
            egui::Label::new(egui::RichText::new(shown).font(font).color(p.dim)).selectable(false),
        );
    }

    /// "N selected · M shown", *Clear* and *Done*; returns whether each was
    /// clicked.
    fn footer(&self, ui: &mut Ui, selected: usize, shown: Option<usize>) -> (bool, bool) {
        let p = &Palette::ONE_DARK;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), FOOTER), Sense::hover());
        ui.painter().hline(
            (rect.left() - 4.0)..=(rect.right() + 4.0),
            rect.top() + 4.0,
            Stroke::new(1.0, p.selection),
        );
        let inner = Rect::from_min_max(pos2(rect.left() + 8.0, rect.top() + 5.0), rect.max);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        child.spacing_mut().item_spacing.x = 4.0;
        let done = child.add(Button::new("Done").primary().small()).clicked();
        let clear = child.add(Button::new("Clear").ghost().small()).clicked();
        let mut count = format!("{selected} selected");
        if let Some(shown) = shown {
            count.push_str(&format!(" · {shown} shown"));
        }
        child.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(count).size(12.0).color(p.dim))
                    .selectable(false),
            );
        });
        (clear, done)
    }

    /// The search field at the top of the panel, with a clear button while
    /// it has text; the highlight moves to the first match as it changes.
    fn search_field(&self, ui: &mut Ui, state: &mut State) {
        let p = &Palette::ONE_DARK;
        ui.visuals_mut().extreme_bg_color = p.bg;
        let response = ui.add(
            crate::widgets::field::TextField::new(&mut state.query)
                .hint("Search")
                .inset_right(24.0),
        );
        let field = response.rect;
        if std::mem::take(&mut state.focus_search) {
            response.request_focus();
        }
        if response.changed()
            && let Some(first) = self.visible(&state.query).first()
        {
            state.highlight = *first;
        }
        if !state.query.is_empty() {
            let center = pos2(field.right() + 18.0, field.center().y);
            let clear = ui.interact(
                Rect::from_center_size(center, vec2(20.0, 20.0)),
                self.id.with("clear"),
                Sense::click(),
            );
            clear.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Clear search"));
            ui.painter().text(
                center,
                Align2::CENTER_CENTER,
                icons::X,
                icons::font(12.0),
                if clear.hovered() {
                    p.text_strong
                } else {
                    p.dim
                },
            );
            if clear.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                state.query.clear();
                state.focus_search = true;
            }
        }
        ui.add_space(SEARCH - 32.0 - ui.spacing().item_spacing.y);
    }

    /// The closed control, with a chevron that turns while the panel is
    /// open; a multi-select shows `chips` (or the placeholder).
    fn trigger(&self, ui: &mut Ui, open: bool, chips: &[String]) -> Response {
        let p = &Palette::ONE_DARK;
        let (font, size) = match self.look {
            Trigger::Button => (FontId::proportional(12.0), None),
            Trigger::Field(width) => (self.label_font(), Some(vec2(width, 32.0))),
        };
        let galley = ui
            .painter()
            .layout_no_wrap(self.trigger.clone(), font, p.text);
        let size = size.unwrap_or(vec2(galley.size().x + 10.0 + 6.0 + 14.0 + 8.0, 28.0));
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let enabled = response.enabled();
        let name = self.name;
        let value = if self.multi {
            chips.join(", ")
        } else {
            self.trigger.clone()
        };
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::ComboBox, enabled, name);
            info.current_text_value = Some(value.clone());
            info
        });
        let hover = motion::fade(
            ui.ctx(),
            response.id.with("hover"),
            response.hovered(),
            timing::HOVER,
            motion::ease_out,
        );
        let painter = ui.painter();
        match self.look {
            Trigger::Button => {
                painter.rect_filled(
                    rect,
                    radius::CONTROL,
                    p.surface.lerp_to_gamma(p.selection, hover),
                );
            }
            Trigger::Field(_) => {
                // Inset like a text field (see `field::TextField`).
                painter.rect_filled(rect, radius::CONTROL, ui.visuals().extreme_bg_color);
                let outline = if open {
                    p.accent
                } else {
                    p.selection.lerp_to_gamma(p.border, hover)
                };
                painter.rect_stroke(
                    rect,
                    radius::CONTROL,
                    Stroke::new(1.0, outline),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if self.multi && !chips.is_empty() {
            self.chips(ui, rect, chips);
        } else {
            let left = match self.look {
                Trigger::Button => 10.0,
                Trigger::Field(_) => 12.0,
            };
            let color = if self.multi { p.dim } else { p.text };
            ui.painter().galley(
                pos2(rect.left() + left, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
        }
        let turn = motion::fade(
            ui.ctx(),
            response.id.with("chevron"),
            open,
            CLOSE,
            motion::ease_in_out,
        );
        let chevron = if turn > 0.5 {
            icons::CHEVRON_UP
        } else {
            icons::CHEVRON_DOWN
        };
        ui.painter().text(
            pos2(rect.right() - 8.0 - 7.0, rect.center().y),
            Align2::CENTER_CENTER,
            chevron,
            icons::font(14.0),
            p.text,
        );
        response.on_hover_cursor(CursorIcon::PointingHand)
    }

    /// The selection as chips in the closed control, as many as fit (only
    /// the first one is ever cut); the rest collapse into a Magenta "+N"
    /// whose tooltip shows them. A chip that wasn't there last frame
    /// scales in.
    fn chips(&self, ui: &mut Ui, rect: Rect, labels: &[String]) {
        let p = &Palette::ONE_DARK;
        let ctx = ui.ctx().clone();
        let font = FontId::proportional(12.0);
        let plus_font = fonts::semibold(12.0);
        let painter = ui.painter().clone();
        let known_id = self.id.with("chips_known");
        let known: Option<Vec<String>> = ctx.data(|d| d.get_temp(known_id));
        let right = rect.right() - 30.0;
        let mut x = rect.left() + 6.0;
        let plus_width = |n: usize| {
            painter
                .layout_no_wrap(format!("+{n}"), plus_font.clone(), p.bg_dark)
                .size()
                .x
                + 16.0
        };
        let mut shown = 0;
        for (index, label) in labels.iter().enumerate() {
            let rest = labels.len() - index - 1;
            let reserve = if rest > 0 {
                plus_width(rest) + 4.0
            } else {
                0.0
            };
            let room = right - x - reserve;
            let (text, cut) = text::elide(&painter, label, &font, (room - 16.0).max(0.0));
            if shown > 0 && (cut || room < 40.0) {
                break;
            }
            let galley = painter.layout_no_wrap(text, font.clone(), p.text_strong);
            let chip = Rect::from_min_size(
                pos2(x, rect.center().y - CHIP / 2.0),
                vec2(galley.size().x + 16.0, CHIP),
            );
            let id = self.id.with(("chip", label));
            let new = known.as_ref().is_some_and(|k| !k.contains(label));
            if new {
                motion::fade(&ctx, id, false, 0.0, motion::ease_out);
            }
            let grow = motion::fade(&ctx, id, true, timing::CHIP_IN, motion::ease_out);
            let drawn = Rect::from_center_size(chip.center(), chip.size() * (0.6 + 0.4 * grow));
            painter.rect_filled(drawn, radius::CONTROL, p.selection.gamma_multiply(grow));
            painter.galley(
                pos2(chip.left() + 8.0, chip.center().y - galley.size().y / 2.0),
                galley,
                p.text_strong.gamma_multiply(grow),
            );
            x = chip.right() + 4.0;
            shown += 1;
        }
        ctx.data_mut(|d| d.insert_temp(known_id, labels.to_vec()));
        let hidden = &labels[shown..];
        if hidden.is_empty() {
            return;
        }
        let galley = painter.layout_no_wrap(format!("+{}", hidden.len()), plus_font, p.bg_dark);
        let chip = Rect::from_min_size(
            pos2(x, rect.center().y - CHIP / 2.0),
            vec2(galley.size().x + 16.0, CHIP),
        );
        painter.rect_filled(chip, radius::CONTROL, p.accent);
        painter.galley(
            pos2(chip.left() + 8.0, chip.center().y - galley.size().y / 2.0),
            galley,
            p.bg_dark,
        );
        let response = ui.interact(chip, self.id.with("more"), Sense::hover());
        let hidden = hidden.to_vec();
        popup::tip_below_after(response, timing::CHIP_TIP, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for label in &hidden {
                    crate::widgets::marks::chip(ui, label, p.selection, p.text_strong);
                }
            });
        });
    }

    /// The option rows; returns the one clicked (`typed` gets the search
    /// text when its "Use “…”" row is).
    fn rows(
        &self,
        ui: &mut Ui,
        state: &mut State,
        (selected, visible): (&[usize], &[usize]),
        (max_height, typed): (f32, &mut Option<String>),
    ) -> Option<usize> {
        let p = &Palette::ONE_DARK;
        let row = self.row_height();
        let mut picked = None;
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("scroll"))
            .max_height(max_height)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let slot = ui.painter().add(Shape::Noop);
                let mut highlighted = None;
                let searching = !state.query.trim().is_empty();
                if let Some((text, loading)) = &self.status {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row), Sense::hover());
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect.shrink2(vec2(10.0, 0.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    child.spacing_mut().item_spacing.x = 8.0;
                    let color = if *loading {
                        crate::widgets::marks::pulsing_ring(&mut child, 10.0, p.yellow);
                        p.dim
                    } else {
                        child.label(
                            egui::RichText::new(icons::TRIANGLE_ALERT.to_string())
                                .font(icons::font(12.0))
                                .color(p.yellow),
                        );
                        p.yellow
                    };
                    let font = FontId::proportional(12.0);
                    let (shown, _) =
                        text::elide(child.painter(), text, &font, child.available_width());
                    child.add(
                        egui::Label::new(egui::RichText::new(shown).font(font).color(color))
                            .selectable(false),
                    );
                }
                let usable = self.usable_query(&state.query);
                if visible.is_empty() && self.status.is_none() && usable.is_none() {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row), Sense::hover());
                    let what = self.search.unwrap_or("options");
                    ui.painter().text(
                        pos2(rect.left() + 10.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        format!("No {what} match"),
                        FontId::proportional(13.0),
                        p.dim,
                    );
                }
                for &index in visible {
                    let choice = &self.choices[index];
                    if let Some(group) = choice.group.as_ref().filter(|_| !searching) {
                        let (rect, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), GROUP), Sense::hover());
                        if index > 0 {
                            ui.painter().hline(
                                (rect.left() + 4.0)..=(rect.right() - 4.0),
                                rect.top() + 4.0,
                                Stroke::new(1.0, p.selection),
                            );
                        }
                        ui.painter().text(
                            pos2(rect.left() + 10.0, rect.bottom() - 9.0),
                            Align2::LEFT_CENTER,
                            group.to_uppercase(),
                            crate::fonts::semibold(11.0),
                            p.dim,
                        );
                    }
                    let enabled = choice.disabled.is_none();
                    let sense = if enabled {
                        Sense::click()
                    } else {
                        Sense::hover()
                    };
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row), sense);
                    let is_selected = selected.contains(&index);
                    let role = if self.multi {
                        Role::CheckBox
                    } else {
                        Role::ListBoxOption
                    };
                    response.widget_info(|| {
                        WidgetInfo::selected(role, enabled, is_selected, &choice.label)
                    });
                    if response.hovered() && enabled {
                        state.highlight = index;
                    }
                    if response.clicked() && enabled {
                        picked = Some(index);
                    }
                    if index == state.highlight && enabled {
                        highlighted = Some((index, rect));
                    }
                    let strong = index == state.highlight || is_selected;
                    let color = match (enabled, strong) {
                        (false, _) => p.disabled_text,
                        (true, true) => p.text_strong,
                        (true, false) => p.text,
                    };
                    let second = choice.disabled.as_ref().or(choice.detail.as_ref());
                    let label_y = if second.is_some() {
                        rect.top() + 10.0
                    } else {
                        rect.center().y
                    };
                    let mut left = rect.left() + 10.0;
                    let ticked = motion::fade(
                        ui.ctx(),
                        self.id.with(("check", index)),
                        is_selected,
                        if self.multi { timing::TICK } else { CHECK },
                        motion::ease_in_out,
                    );
                    if self.multi {
                        let center = pos2(left + CHECKBOX / 2.0, rect.center().y);
                        checkbox(ui, center, ticked);
                        left += CHECKBOX + 10.0;
                    }
                    if let Some(icon) = choice.icon {
                        ui.painter().text(
                            pos2(left + 7.0, label_y),
                            Align2::CENTER_CENTER,
                            icon,
                            icons::font(14.0),
                            color,
                        );
                        left += 14.0 + 8.0;
                    }
                    // Text stops before the check mark's column (and the
                    // note, if any).
                    let mut right = rect.right() - 10.0;
                    if !self.multi {
                        right -= 14.0 + 8.0;
                    }
                    if let Some(note) = &choice.note {
                        let font = if self.mono_notes {
                            FontId::monospace(11.0)
                        } else {
                            FontId::proportional(11.0)
                        };
                        let (note, _) =
                            text::elide(ui.painter(), note, &font, (right - left) * 0.45);
                        let galley = ui.painter().layout_no_wrap(note, font, p.dim);
                        right -= galley.size().x;
                        ui.painter().galley(
                            pos2(right, label_y - galley.size().y / 2.0),
                            galley,
                            p.dim,
                        );
                        right -= 8.0;
                    }
                    let room = (right - left).max(0.0);
                    let font = self.label_font();
                    let (label, _) = text::elide(ui.painter(), &choice.label, &font, room);
                    ui.painter()
                        .text(pos2(left, label_y), Align2::LEFT_CENTER, label, font, color);
                    if let Some(second) = second {
                        let mut x = left;
                        let y = rect.bottom() - 10.0;
                        if !enabled {
                            ui.painter().text(
                                pos2(x + 6.0, y),
                                Align2::CENTER_CENTER,
                                icons::LOCK,
                                icons::font(11.0),
                                p.disabled_text,
                            );
                            x += 16.0;
                        }
                        let font = FontId::proportional(11.0);
                        let room = (room - (x - left)).max(0.0);
                        let (second, _) = text::elide(ui.painter(), second, &font, room);
                        ui.painter().text(
                            pos2(x, y),
                            Align2::LEFT_CENTER,
                            second,
                            font,
                            if enabled { p.dim } else { p.disabled_text },
                        );
                    }
                    if !self.multi && ticked > 0.0 {
                        ui.painter().text(
                            pos2(rect.right() - 10.0 - 7.0, rect.center().y),
                            Align2::CENTER_CENTER,
                            icons::CHECK,
                            icons::font(14.0),
                            p.accent.gamma_multiply(ticked),
                        );
                    }
                    if enabled {
                        response.on_hover_cursor(CursorIcon::PointingHand);
                    } else if response.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
                    }
                }
                // The search text as a value of its own.
                if let Some(text) = usable {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row), Sense::click());
                    let label = format!("Use \u{201c}{text}\u{201d}");
                    response.widget_info(|| {
                        WidgetInfo::selected(Role::ListBoxOption, true, false, &label)
                    });
                    if response.hovered() {
                        highlighted = Some((usize::MAX, rect));
                    }
                    ui.painter().text(
                        pos2(rect.left() + 10.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        label,
                        self.label_font(),
                        p.text,
                    );
                    if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        *typed = Some(text);
                    }
                }
                nav::hover_highlight(
                    ui,
                    self.id.with("highlight"),
                    slot,
                    highlighted,
                    p.selection,
                );
            });
        picked
    }
}

/// Whether the dropdown made with `id` is open (its options load on open).
pub fn is_open(ctx: &egui::Context, id: impl std::hash::Hash + std::fmt::Debug) -> bool {
    ctx.data(|d| d.get_temp::<State>(Id::unique(id)))
        .is_some_and(|s| s.open)
}

/// A 16 px checkbox at `center`, `ticked` 0 → 1: the outline gives way to a
/// Magenta fill and the tick draws itself in from the left.
fn checkbox(ui: &Ui, center: egui::Pos2, ticked: f32) {
    let p = &Palette::ONE_DARK;
    let rect = Rect::from_center_size(center, egui::Vec2::splat(CHECKBOX));
    let painter = ui.painter();
    if ticked < 1.0 {
        painter.rect_stroke(
            rect,
            radius::CHECKBOX,
            Stroke::new(1.5, p.border.gamma_multiply(1.0 - ticked)),
            egui::StrokeKind::Inside,
        );
    }
    if ticked > 0.0 {
        painter.rect_filled(rect, radius::CHECKBOX, p.accent.gamma_multiply(ticked));
        let mut clip = rect;
        clip.max.x = rect.left() + rect.width() * ticked;
        painter
            .with_clip_rect(clip.intersect(painter.clip_rect()))
            .text(
                center,
                Align2::CENTER_CENTER,
                icons::CHECK,
                icons::font(12.0),
                p.bg_dark,
            );
    }
}
