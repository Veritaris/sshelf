//! The icon rail (SPEC.md §10 *Layout*, *Visual design*): the app mark with
//! its status dot, Hosts, Tunnels and Groups; the orphan indicator and
//! Settings at the bottom (SPEC.md §6). Ctrl+1…4 (Cmd on macOS) switch views;
//! the selection slides between items.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Id, Key, Modifiers, Rect, Response, RichText, Role,
    Sense, Ui, Vec2, WidgetInfo, pos2, vec2,
};

use crate::icon::Status;
use crate::theme::{Palette, radius, timing};
use crate::tour::targets::{self, Target};
use crate::widgets::{marks, nav, popup};
use crate::{fonts, icons, motion, window};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Hosts,
    Tunnels,
    Groups,
    Settings,
}

/// Width of the rail.
pub const WIDTH: f32 = 64.0;
const ITEM: f32 = 40.0;
const GAP: f32 = 8.0;
const LOGO: f32 = 32.0;
const ICON: f32 = 16.0;

/// What the rail shows besides the views.
pub struct Rail<'a> {
    /// One description per orphan (SPEC.md §6).
    pub orphans: &'a [String],
    /// Tunnels running now: the Tunnels item's dot and tooltip.
    pub running: usize,
    /// The app mark's dot.
    pub status: Status,
    /// Settings can't be opened, and why (the tour).
    pub settings_off: Option<&'a str>,
    /// A newer sshelf to announce (SPEC.md §10 *Update check*): the Settings
    /// item's Blue dot and a line in its tooltip.
    pub update: Option<&'a str>,
}

struct Item {
    view: View,
    icon: char,
    name: &'static str,
    key: Key,
}

const ITEMS: [Item; 4] = [
    Item {
        view: View::Hosts,
        icon: icons::SERVER,
        name: "Hosts",
        key: Key::Num1,
    },
    Item {
        view: View::Tunnels,
        icon: icons::ARROW_RIGHT_LEFT,
        name: "Tunnels",
        key: Key::Num2,
    },
    Item {
        view: View::Groups,
        icon: icons::LAYERS,
        name: "Groups",
        key: Key::Num3,
    },
    Item {
        view: View::Settings,
        icon: icons::SETTINGS,
        name: "Settings",
        key: Key::Num4,
    },
];

/// "Ctrl+2", or "Cmd+2" on macOS.
fn shortcut(key: Key) -> String {
    let modifier = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    format!("{modifier}+{}", key.symbol_or_name())
}

/// Draws the rail over the whole of `ui` (its panel). Returns `true` when
/// the orphan indicator was clicked.
pub fn show(ui: &mut Ui, view: &mut View, rail: &Rail) -> bool {
    let p = &Palette::ONE_DARK;
    for item in &ITEMS {
        let off = item.view == View::Settings && rail.settings_off.is_some();
        if !off
            && ui
                .ctx()
                .input_mut(|i| i.consume_key(Modifiers::COMMAND, item.key))
        {
            *view = item.view;
        }
    }
    let rect = ui.max_rect();
    // The window's top-left corner; the status bar below has the bottom one.
    let corner = CornerRadius {
        nw: window::corner(ui.ctx()),
        ..CornerRadius::ZERO
    };
    ui.painter().rect_filled(rect, corner, p.bg_dark);

    let x = rect.center().x;
    let at = |top: f32| Rect::from_min_size(pos2(x - ITEM / 2.0, top), Vec2::splat(ITEM));
    let logo_top = rect.top() + 16.0;
    let logo_rect = Rect::from_min_size(pos2(x - LOGO / 2.0, logo_top), Vec2::splat(LOGO));
    logo(ui, logo_rect, rail.status);
    targets::register(ui.ctx(), Target::RailLogo, logo_rect.expand(4.0));
    let first = logo_top + LOGO + 12.0 + GAP;
    let settings = at(rect.bottom() - 12.0 - ITEM);
    let rects = [
        at(first),
        at(first + ITEM + GAP),
        at(first + 2.0 * (ITEM + GAP)),
        settings,
    ];
    let selected = ITEMS.iter().position(|i| i.view == *view).unwrap_or(0);
    nav::highlight(
        ui,
        Id::unique("rail_highlight"),
        selected,
        rects[selected],
        p.selection,
        radius::CONTROL,
    );
    // The hover background slides between items (under the icons).
    let hover_slot = ui.painter().add(egui::Shape::Noop);
    let mut hovered = None;
    for (index, (item, rect)) in ITEMS.iter().zip(rects).enumerate() {
        let mut tip = item.name.to_string();
        if item.view == View::Tunnels && rail.running > 0 {
            tip.push_str(&format!(" · {} running", rail.running));
        }
        let update = rail.update.filter(|_| item.view == View::Settings);
        if let Some(version) = update {
            tip.push_str(&format!(" · sshelf {version} is available"));
        }
        let off = (item.view == View::Settings)
            .then_some(rail.settings_off)
            .flatten();
        if let Some(why) = off {
            tip = why.to_string();
        }
        let response = ui
            .add_enabled_ui(off.is_none(), |ui| {
                rail_item(ui, rect, item, &tip, item.view == *view)
            })
            .inner;
        if response.hovered() && item.view != *view {
            hovered = Some((index, rect));
        }
        if item.view == View::Tunnels && rail.running > 0 {
            // The activity dot: 8 px, 6 px in from the top-right corner.
            let center = rect.right_top() + vec2(-10.0, 10.0);
            marks::paint_dot(ui.painter(), center, marks::DOT, p.green);
        }
        if item.view == View::Settings {
            // The same dot in Blue while a newer version waits; it fades.
            let shown = motion::fade(
                ui.ctx(),
                Id::unique("rail_update_dot"),
                update.is_some(),
                timing::SLIDE,
                motion::ease_in_out,
            );
            if shown > 0.0 {
                let center = rect.right_top() + vec2(-10.0, 10.0);
                marks::paint_dot(
                    ui.painter(),
                    center,
                    marks::DOT,
                    p.blue.gamma_multiply(shown),
                );
            }
        }
        let clicked = match item.view {
            View::Tunnels => targets::clicked(ui.ctx(), Target::RailTunnels, &response),
            View::Groups => targets::clicked(ui.ctx(), Target::RailGroups, &response),
            _ => response.clicked(),
        };
        if clicked && off.is_none() {
            *view = item.view;
        }
    }
    nav::hover_highlight(ui, Id::unique("rail_hover"), hover_slot, hovered, p.surface);
    !rail.orphans.is_empty() && orphan_button(ui, at(settings.top() - GAP - ITEM), rail.orphans)
}

/// One view button: an icon that turns Magenta when selected (the
/// backgrounds are the sliding selection and hover highlights).
fn rail_item(ui: &mut Ui, rect: Rect, item: &Item, tip: &str, selected: bool) -> Response {
    let p = &Palette::ONE_DARK;
    let response = ui.interact(rect, Id::unique(("rail", item.name)), Sense::click());
    let enabled = response.enabled();
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, selected, item.name));
    let ctx = ui.ctx();
    let on = motion::fade(
        ctx,
        response.id.with("selected"),
        selected,
        timing::SLIDE,
        motion::ease_in_out,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        item.icon,
        icons::font(ICON),
        p.text.lerp_to_gamma(p.accent, on),
    );
    let hint = shortcut(item.key);
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    popup::tip(response, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(tip);
            ui.label(RichText::new(hint).color(p.dim));
        });
    })
}

/// The yellow orphan indicator with its count; the tooltip lists them.
fn orphan_button(ui: &mut Ui, rect: Rect, orphans: &[String]) -> bool {
    let p = &Palette::ONE_DARK;
    let count = orphans.len();
    let name = format!("{count} orphan{}", if count == 1 { "" } else { "s" });
    let response = ui.interact(rect, Id::unique("rail_orphans"), Sense::click());
    let enabled = response.enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &name));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, p.yellow_tint.gamma_multiply(hover));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icons::TRIANGLE_ALERT,
        icons::font(ICON),
        p.yellow,
    );
    // The count badge, 4 px from the top and 2 px from the right.
    let galley = painter.layout_no_wrap(count.to_string(), fonts::semibold(10.0), p.bg_dark);
    let size = galley.size() + vec2(8.0, 0.0);
    let badge = Rect::from_min_size(pos2(rect.right() - 2.0 - size.x, rect.top() + 4.0), size);
    painter.rect_filled(badge, radius::CONTROL, p.yellow);
    painter.galley(badge.min + vec2(4.0, 0.0), galley, Color32::PLACEHOLDER);
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    popup::tip(response, |ui| {
        ui.label("App data that no longer links to a host:");
        for orphan in orphans {
            ui.label(format!("• {orphan}"));
        }
        ui.label(RichText::new("Click to relink or delete.").color(p.dim));
    })
    .clicked()
}

/// The app mark: a Magenta tile with three bars and the status dot (with a
/// ring in the rail's colour) at its bottom-right corner.
fn logo(ui: &mut Ui, rect: Rect, status: Status) {
    let p = &Palette::ONE_DARK;
    let response = ui.interact(rect, Id::unique("rail_logo"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, status.describe()));
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, p.accent);
    let bar = vec2(rect.width() - 14.0, 3.0);
    for i in 0..3 {
        let top = rect.center().y - 8.5 + i as f32 * 7.0;
        painter.rect_filled(
            Rect::from_min_size(pos2(rect.left() + 7.0, top), bar),
            1,
            p.bg,
        );
    }
    if let Some(color) = status.color() {
        let center = rect.right_bottom() - vec2(2.0, 2.0);
        painter.circle_filled(center, 7.0, p.bg_dark);
        painter.circle_filled(center, 5.0, color);
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_util;

    struct State {
        view: View,
        orphans: Vec<String>,
        running: usize,
        orphan_clicks: usize,
        update: Option<&'static str>,
    }

    fn harness(orphans: Vec<String>, running: usize) -> egui_kittest::Harness<'static, State> {
        test_util::harness()
            .with_size([64.0, 600.0])
            .build_ui_state(
                |ui, state: &mut State| {
                    let rail = Rail {
                        orphans: &state.orphans,
                        running: state.running,
                        status: Status::Running,
                        settings_off: None,
                        update: state.update,
                    };
                    if show(ui, &mut state.view, &rail) {
                        state.orphan_clicks += 1;
                    }
                },
                State {
                    view: View::Hosts,
                    orphans,
                    running,
                    orphan_clicks: 0,
                    update: None,
                },
            )
    }

    /// A newer version: the Settings tooltip says so (the dot is painted
    /// with it), and both go with it.
    #[test]
    fn an_update_adds_to_the_settings_tooltip() {
        let mut harness = harness(Vec::new(), 0);
        harness.state_mut().update = Some("0.4.0");
        harness.run();
        harness.get_by_label("Settings").hover();
        harness.run_steps(40);
        harness.get_by_label_contains("Settings · sshelf 0.4.0 is available");
        harness.state_mut().update = None;
        harness.run_steps(2);
        // The open tooltip now says only "Settings".
        assert!(harness.query_by_label_contains("is available").is_none());
    }

    #[test]
    fn items_switch_views_and_report_the_selection() {
        let mut harness = harness(Vec::new(), 0);
        harness.run();
        harness.get_by_label("sshelf, tunnels running");
        harness.get_by_label("Tunnels").click();
        harness.run();
        assert_eq!(harness.state().view, View::Tunnels);
        harness.get_by_label("Settings").click();
        harness.run();
        assert_eq!(harness.state().view, View::Settings);
        assert!(harness.query_by_label("0 orphans").is_none());
    }

    #[test]
    fn shortcuts_switch_views() {
        let mut harness = harness(Vec::new(), 0);
        harness.run();
        for (key, view) in [
            (Key::Num3, View::Groups),
            (Key::Num2, View::Tunnels),
            (Key::Num4, View::Settings),
            (Key::Num1, View::Hosts),
        ] {
            harness.key_press_modifiers(Modifiers::COMMAND, key);
            harness.run();
            assert_eq!(harness.state().view, view);
        }
    }

    #[test]
    fn the_orphan_indicator_counts_and_clicks() {
        let mut harness = harness(
            vec!["Metadata · old-k3s".into(), "Tunnel · grafana".into()],
            1,
        );
        harness.run();
        harness.get_by_label("2 orphans").click();
        harness.run();
        assert_eq!(harness.state().orphan_clicks, 1);
        assert_eq!(harness.state().view, View::Hosts);
    }
}
