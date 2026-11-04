//! The frameless window (SPEC.md §10 *Visual design*): no OS title bar or
//! border. The app paints its own background (16 px corners where the
//! platform can show them), a 44 px drag strip with *Minimize* and *Close*,
//! and resize handles along every edge. On Windows, [`native`] undoes the
//! frame strip egui-winit leaves on frameless windows.

use egui::{
    Align, Align2, Color32, CursorIcon, Id, Label, Layout, PointerButton, Pos2, Rect,
    ResizeDirection, Response, RichText, Role, Sense, Ui, Vec2, ViewportBuilder, ViewportCommand,
    WidgetInfo,
};

use crate::icons;
use crate::motion;
use crate::theme::{Palette, radius, timing};
use crate::widgets::marks;

/// Height of the drag strip at the top of the content area.
pub const STRIP_HEIGHT: f32 = 44.0;
/// Width of the resize handles along the edges (corners take twice that).
const RESIZE_BORDER: f32 = 6.0;
const MIN_SIZE: [f32; 2] = [960.0, 600.0];
const DEFAULT_SIZE: [f32; 2] = [1280.0, 800.0];

/// Whether this platform shows the rounded, transparent corners. Linux only
/// on Wayland: on X11 they need a compositor, which can't be told without an
/// X11 dependency, so corners stay square there.
fn rounded() -> bool {
    if cfg!(target_os = "linux") {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    } else {
        true
    }
}

/// The root viewport: frameless, transparent where corners are rounded.
pub fn viewport(builder: ViewportBuilder) -> ViewportBuilder {
    builder
        .with_decorations(false)
        .with_transparent(rounded())
        // macOS: a system shadow on a transparent window ghosts; flat anyway.
        .with_has_shadow(false)
        .with_inner_size(DEFAULT_SIZE)
        .with_min_inner_size(MIN_SIZE)
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)
    })
}

/// The window's corner radius right now: 16 px, or 0 when maximized or
/// where the platform can't show rounded corners. Panels along the window's
/// edge round their outer corners with it.
pub fn corner(ctx: &egui::Context) -> u8 {
    if rounded() && !maximized(ctx) {
        radius::LARGE
    } else {
        0
    }
}

/// Paints the window background behind everything.
pub fn background(ctx: &egui::Context) {
    ctx.layer_painter(egui::LayerId::background()).rect_filled(
        ctx.content_rect(),
        corner(ctx),
        Palette::ONE_DARK.bg,
    );
}

/// What the drag strip asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripAction {
    StartDrag,
    ToggleMaximize,
    Minimize,
    Close,
    /// *Exit tour*.
    ExitTour,
}

/// The drag strip: `title` on the left, *Minimize* and *Close* on the right.
/// Dragging it moves the window; a double click maximizes or restores. While
/// the tour runs it also shows the Magenta "Tour · sample data" chip, what
/// it promises, and *Exit tour* (SPEC.md §10 *Onboarding tour*).
pub fn strip(ui: &mut Ui, title: &str, tour: bool) -> Option<StripAction> {
    let p = &Palette::ONE_DARK;
    // Registered first, so the buttons on top of it get their own clicks.
    let drag = ui.interact(
        ui.max_rect(),
        ui.scope_id().with("drag_strip"),
        Sense::click_and_drag(),
    );
    let mut action = if drag.double_clicked() {
        Some(StripAction::ToggleMaximize)
    } else if drag.drag_started_by(PointerButton::Primary) {
        Some(StripAction::StartDrag)
    } else {
        None
    };
    ui.horizontal_centered(|ui| {
        ui.add(Label::new(RichText::new(title).size(12.0).color(p.dim)).selectable(false));
        if tour {
            ui.add_space(8.0);
            marks::chip(ui, "Tour · sample data", p.accent, p.bg_dark);
            ui.add(
                Label::new(
                    RichText::new("nothing here touches your files")
                        .size(12.0)
                        .color(p.dim),
                )
                .selectable(false),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let close = window_button(ui, icons::X, "Close", p.red);
            if crate::tour::targets::clicked(
                ui.ctx(),
                crate::tour::targets::Target::WindowClose,
                &close,
            ) {
                action = Some(StripAction::Close);
            }
            if window_button(ui, icons::MINUS, "Minimize", p.yellow).clicked() {
                action = Some(StripAction::Minimize);
            }
            if tour {
                ui.add_space(8.0);
                if ui
                    .add(crate::widgets::button::Button::new("Exit tour").small())
                    .clicked()
                {
                    action = Some(StripAction::ExitTour);
                }
            }
        });
    });
    action
}

/// Carries out a [`StripAction`]. *Close* is the same close request as the
/// OS's, so the close guard decides what happens (SPEC.md §10).
pub fn apply(ctx: &egui::Context, action: StripAction) {
    let command = match action {
        StripAction::StartDrag => ViewportCommand::StartDrag,
        StripAction::ToggleMaximize => ViewportCommand::Maximized(!maximized(ctx)),
        StripAction::Minimize => ViewportCommand::Minimized(true),
        StripAction::Close => ViewportCommand::Close,
        // The app ends the tour itself.
        StripAction::ExitTour => return,
    };
    ctx.send_viewport_cmd(command);
}

/// A 32 px window control: transparent, fading to a raised background and
/// `accent` icon on hover; pressed shows the selected background.
fn window_button(ui: &mut Ui, icon: char, name: &str, accent: Color32) -> Response {
    let p = &Palette::ONE_DARK;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let enabled = response.enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name));
    let hover = motion::fade(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        timing::HOVER,
        motion::ease_out,
    );
    let fill = if response.is_pointer_button_down_on() {
        p.selection
    } else {
        p.surface.gamma_multiply(hover)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        icons::font(16.0),
        p.text.lerp_to_gamma(accent, hover),
    );
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(name)
}

/// Resize handles along the edges and corners of `rect`, corners first so
/// they win where they overlap the edges.
fn edges(rect: Rect, border: f32) -> [(ResizeDirection, Rect); 8] {
    let corner = Vec2::splat(border * 2.0);
    let (min, max) = (rect.min, rect.max);
    [
        (ResizeDirection::NorthWest, Rect::from_min_size(min, corner)),
        (
            ResizeDirection::NorthEast,
            Rect::from_min_size(Pos2::new(max.x - corner.x, min.y), corner),
        ),
        (
            ResizeDirection::SouthWest,
            Rect::from_min_size(Pos2::new(min.x, max.y - corner.y), corner),
        ),
        (
            ResizeDirection::SouthEast,
            Rect::from_min_size(max - corner, corner),
        ),
        (
            ResizeDirection::North,
            Rect::from_x_y_ranges(min.x..=max.x, min.y..=min.y + border),
        ),
        (
            ResizeDirection::South,
            Rect::from_x_y_ranges(min.x..=max.x, max.y - border..=max.y),
        ),
        (
            ResizeDirection::West,
            Rect::from_x_y_ranges(min.x..=min.x + border, min.y..=max.y),
        ),
        (
            ResizeDirection::East,
            Rect::from_x_y_ranges(max.x - border..=max.x, min.y..=max.y),
        ),
    ]
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    }
}

/// Resize handles on every edge, since the OS frame is gone. Call last in
/// `App::ui`, so they sit above the panels' widgets.
pub fn resize_handles(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if maximized(&ctx) {
        return;
    }
    // Reversed: egui gives the pointer to the widget added last.
    let handles = edges(ctx.content_rect(), RESIZE_BORDER);
    for (i, (direction, rect)) in handles.into_iter().enumerate().rev() {
        let response = ui.interact(rect, Id::unique(("resize_handle", i)), Sense::drag());
        if response.hovered() || response.dragged() {
            ctx.set_cursor_icon(resize_cursor(direction));
        }
        if response.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }
}

/// Windows: egui-winit always asks winit for the "undecorated shadow" on
/// frameless windows, which keeps a 1 px frame strip at the top of the
/// window. Windows draws a line in it, and a drop shadow around the window.
/// The window is subclassed so all of it is client area while not
/// maximized (maximized windows are left to winit, which clamps them to the
/// work area), and DWM's own corner rounding and border are turned off.
#[cfg(target_os = "windows")]
pub mod native {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
        DwmSetWindowAttribute,
    };
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsZoomed, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowPos, WM_NCCALCSIZE,
    };

    const SUBCLASS_ID: usize = 1;

    /// Call once from the app's creation, with the root window.
    pub fn prepare(cc: &eframe::CreationContext<'_>) {
        let Ok(RawWindowHandle::Win32(handle)) = cc.window_handle().map(|h| h.as_raw()) else {
            return;
        };
        frameless(handle.hwnd.get() as HWND);
    }

    /// The fix for any frameless window of ours (the root, the tray menu):
    /// no DWM corners or accent-colour border, all of it client area.
    pub fn frameless(hwnd: HWND) {
        // SAFETY: `hwnd` is a live window of this process; the attribute
        // values are 4-byte integers as DWM expects, and the subclass
        // procedure is a plain function that lives as long as the program.
        unsafe {
            // Both are no-ops before Windows 11, which doesn't round anyway.
            set_attribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                DWMWCP_DONOTROUND as u32,
            );
            set_attribute(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
            SetWindowSubclass(hwnd, Some(full_client), SUBCLASS_ID, 0);
            // Recompute the frame with the subclass in place.
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    unsafe fn set_attribute(hwnd: HWND, attribute: i32, value: u32) {
        // SAFETY: see `prepare`.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                (&value as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }
    }

    /// The whole window is client area while it isn't maximized.
    unsafe extern "system" fn full_client(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        // SAFETY: called by Windows for `hwnd`'s messages; everything else
        // goes on to winit's window procedure.
        unsafe {
            if msg == WM_NCCALCSIZE && wparam != 0 && IsZoomed(hwnd) == 0 {
                return 0;
            }
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_util;

    #[test]
    fn edges_cover_the_border_and_corners_win() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0));
        let at = |pos: Pos2| {
            edges(rect, RESIZE_BORDER)
                .into_iter()
                .find(|(_, r)| r.contains(pos))
                .map(|(d, _)| d)
        };
        assert_eq!(at(Pos2::new(1.0, 1.0)), Some(ResizeDirection::NorthWest));
        assert_eq!(
            at(Pos2::new(998.0, 598.0)),
            Some(ResizeDirection::SouthEast)
        );
        assert_eq!(at(Pos2::new(500.0, 2.0)), Some(ResizeDirection::North));
        assert_eq!(at(Pos2::new(997.0, 300.0)), Some(ResizeDirection::East));
        assert_eq!(at(Pos2::new(3.0, 300.0)), Some(ResizeDirection::West));
        assert_eq!(at(Pos2::new(500.0, 597.0)), Some(ResizeDirection::South));
        assert_eq!(at(Pos2::new(500.0, 300.0)), None);
    }

    #[test]
    fn strip_buttons_minimize_and_close() {
        let mut harness = test_util::harness()
            .with_size([800.0, 44.0])
            .build_ui_state(
                |ui, actions: &mut Vec<StripAction>| {
                    actions.extend(strip(ui, "sshelf", false));
                },
                Vec::new(),
            );
        harness.run();
        harness.get_by_label("sshelf");
        harness.get_by_label("Minimize").click();
        harness.run();
        harness.get_by_label("Close").click();
        harness.run();
        assert_eq!(
            harness.state(),
            &[StripAction::Minimize, StripAction::Close]
        );
    }
}
