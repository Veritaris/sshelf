//! The dropdown (SPEC.md §10 *Dropdown*), single-select so far: the panel
//! rolls open from the trigger by clipping (200 ms "out", closing 140 ms
//! "in"), upward when there is no room below; 30 px rows (36 px with a
//! second line, as *Save to* has) with the sliding hover highlight; a
//! Magenta check on the selected row; disabled rows with their reason;
//! ↑↓ Enter Esc.

use egui::{
    Align2, Area, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers, Order, Rect, Response,
    Role, Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};

use crate::theme::{Palette, radius, timing};
use crate::widgets::{nav, text};
use crate::{icons, motion};

const ROW: f32 = 30.0;
/// A row with a second line.
const TALL_ROW: f32 = 36.0;
/// Space between the trigger and the panel.
const GAP: f32 = 6.0;
const OPEN: f32 = 0.200;
const CLOSE: f32 = 0.140;
/// The check mark's fade.
const CHECK: f32 = 0.120;

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

/// A single-select dropdown over `options`, showing `selected`.
pub struct Dropdown<'a, T> {
    id: Id,
    name: &'a str,
    choices: Vec<Choice<T>>,
    /// The closed control's text, e.g. "Group by tag".
    trigger: String,
    look: Trigger,
    mono: bool,
    max_visible: usize,
    /// The panel's least width (it is never narrower than the trigger).
    panel_width: f32,
    /// Searchable: what the options are, for "No hosts match".
    search: Option<&'a str>,
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
            max_visible: 10,
            panel_width: 180.0,
            search: None,
        }
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

    /// Shows the closed control and, while open, the panel. Returns the
    /// option picked this frame.
    pub fn show(self, ui: &mut Ui, selected: T) -> Option<T> {
        let p = &Palette::ONE_DARK;
        let ctx = ui.ctx().clone();
        let mut state = ctx
            .data(|d| d.get_temp::<State>(self.id))
            .unwrap_or_default();
        let trigger = self.trigger(ui, state.open);
        // `None`: no option is the current one (an "add" menu), so none
        // gets the check.
        let selected_index = self.choices.iter().position(|c| c.value == selected);
        let mut picked = None;

        let keyboard_open = trigger.has_focus()
            && ctx.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::Space)
                    || i.consume_key(Modifiers::NONE, Key::ArrowDown)
            });
        if (trigger.clicked() || keyboard_open) && !state.open {
            state.open = true;
            state.highlight = selected_index.unwrap_or(0);
            state.query.clear();
            state.focus_search = self.search.is_some();
        } else if trigger.clicked() {
            state.open = false;
        }
        let visible = self.visible(&state.query);
        if state.open {
            // ↑↓ move through what the search leaves.
            let at = visible.iter().position(|&i| i == state.highlight);
            ctx.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    let next = at.map_or(0, |at| (at + 1).min(visible.len().saturating_sub(1)));
                    state.highlight = visible.get(next).copied().unwrap_or(state.highlight);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    let next = at.map_or(0, |at| at.saturating_sub(1));
                    state.highlight = visible.get(next).copied().unwrap_or(state.highlight);
                }
                if i.consume_key(Modifiers::NONE, Key::Enter)
                    && visible.contains(&state.highlight)
                    && self
                        .choices
                        .get(state.highlight)
                        .is_some_and(|c| c.disabled.is_none())
                {
                    picked = Some(state.highlight);
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
            let height = rows as f32 * row + groups as f32 * GROUP + search + 8.0;
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
                            let rows_height = height - 8.0 - search;
                            if let Some(index) =
                                self.rows(ui, &mut state, (selected_index, &visible), rows_height)
                            {
                                picked = Some(index);
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
        if picked.is_some() {
            state.open = false;
        }
        let value = picked.map(|i| self.choices[i].value);
        ctx.data_mut(|d| d.insert_temp(self.id, state));
        value
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
    /// open.
    fn trigger(&self, ui: &mut Ui, open: bool) -> Response {
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
        let (name, value) = (self.name, self.trigger.clone());
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
        let left = match self.look {
            Trigger::Button => 10.0,
            Trigger::Field(_) => 12.0,
        };
        painter.galley(
            pos2(rect.left() + left, rect.center().y - galley.size().y / 2.0),
            galley,
            p.text,
        );
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
        painter.text(
            pos2(rect.right() - 8.0 - 7.0, rect.center().y),
            Align2::CENTER_CENTER,
            chevron,
            icons::font(14.0),
            p.text,
        );
        response.on_hover_cursor(CursorIcon::PointingHand)
    }

    /// The option rows; returns the one clicked.
    fn rows(
        &self,
        ui: &mut Ui,
        state: &mut State,
        (selected, visible): (Option<usize>, &[usize]),
        max_height: f32,
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
                if visible.is_empty() {
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
                    let is_selected = Some(index) == selected;
                    response.widget_info(|| {
                        WidgetInfo::selected(
                            Role::ListBoxOption,
                            enabled,
                            is_selected,
                            &choice.label,
                        )
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
                    let mut right = rect.right() - 10.0 - 14.0 - 8.0;
                    if let Some(note) = &choice.note {
                        let galley = ui.painter().layout_no_wrap(
                            note.clone(),
                            FontId::proportional(11.0),
                            p.dim,
                        );
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
                    let check = motion::fade(
                        ui.ctx(),
                        self.id.with(("check", index)),
                        is_selected,
                        CHECK,
                        motion::ease_out,
                    );
                    if check > 0.0 {
                        ui.painter().text(
                            pos2(rect.right() - 10.0 - 7.0, rect.center().y),
                            Align2::CENTER_CENTER,
                            icons::CHECK,
                            icons::font(14.0),
                            p.accent.gamma_multiply(check),
                        );
                    }
                    if enabled {
                        response.on_hover_cursor(CursorIcon::PointingHand);
                    } else if response.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
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
