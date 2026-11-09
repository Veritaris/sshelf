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
        // Wayland's app id / X11's WM_CLASS: docks match it to the desktop
        // entry (`assets/linux/sshelf.desktop`) for the icon.
        .with_app_id("sshelf")
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
    /// The streamer chip's menu: *Hide host aliases and tags* flipped.
    HideAliases(bool),
    /// The streamer chip's menu: *Turn off streamer mode*.
    StreamerOff,
}

/// Streamer mode as the drag strip shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Streamer {
    pub hide_aliases: bool,
}

/// The drag strip: `title` on the left, *Minimize* and *Close* on the right.
/// Dragging it moves the window; a double click maximizes or restores. While
/// the tour runs it also shows the Magenta "Tour · sample data" chip, what
/// it promises, and *Exit tour* (SPEC.md §10 *Onboarding tour*). In
/// streamer mode the streamer chip with its menu takes the title's place
/// (SPEC.md §10 *Streamer mode*).
pub fn strip(
    ui: &mut Ui,
    title: &str,
    tour: bool,
    streamer: Option<Streamer>,
) -> Option<StripAction> {
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
        match streamer {
            Some(streamer) => {
                if let Some(chosen) = streamer_chip(ui, streamer) {
                    action = Some(chosen);
                }
            }
            None => {
                ui.add(Label::new(RichText::new(title).size(12.0).color(p.dim)).selectable(false));
            }
        }
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

/// The streamer chip (Blue on its tint, 26 px) and its menu: *Hide host
/// aliases and tags*, then *Turn off streamer mode* with a note.
fn streamer_chip(ui: &mut Ui, streamer: Streamer) -> Option<StripAction> {
    use egui::text::{LayoutJob, TextFormat};
    let p = &Palette::ONE_DARK;
    const HEIGHT: f32 = 26.0;
    const PAD: f32 = 8.0;
    const GAP: f32 = 6.0;
    let mut job = LayoutJob::default();
    let font = egui::FontId::proportional(12.0);
    job.append(
        "Streamer mode",
        0.0,
        TextFormat::simple(font.clone(), p.blue),
    );
    if streamer.hide_aliases {
        job.append(
            " · names hidden",
            0.0,
            TextFormat::simple(font, p.link_hover),
        );
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let width = PAD + 14.0 + GAP + galley.size().x + GAP + 12.0 + PAD;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Streamer mode"));
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, p.blue_tint);
    let mut x = rect.left() + PAD;
    let y = rect.center().y;
    let icon = |x: f32, glyph: char, size: f32| {
        painter.text(
            Pos2::new(x + size / 2.0, y),
            Align2::CENTER_CENTER,
            glyph,
            icons::font(size),
            p.blue,
        );
    };
    icon(x, icons::EYE_OFF, 14.0);
    x += 14.0 + GAP;
    let text_size = galley.size();
    painter.galley(Pos2::new(x, y - text_size.y / 2.0), galley, p.blue);
    x += text_size.x + GAP;
    icon(x, icons::CHEVRON_DOWN, 12.0);
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let mut action = None;
    crate::widgets::popup::click_menu(&response, 340.0, |menu| {
        let mut hide = streamer.hide_aliases;
        if menu.switch("Hide host aliases and tags", &mut hide) {
            action = Some(StripAction::HideAliases(hide));
        }
        menu.separator();
        if menu.item(Some(icons::EYE), "Turn off streamer mode", true) {
            action = Some(StripAction::StreamerOff);
        }
        menu.note("Turning it off shows every value at once. Do it off stream.");
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
        // The app ends the tour and sets streamer mode itself.
        StripAction::ExitTour | StripAction::HideAliases(_) | StripAction::StreamerOff => return,
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
    icons::centered(
        painter,
        rect.center(),
        icon,
        16.0,
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
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
        DwmSetWindowAttribute,
    };
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIcon, DestroyIcon, HICON, ICON_BIG, IsZoomed, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetWindowPos, WM_NCCALCSIZE,
        WM_SETICON,
    };

    const SUBCLASS_ID: usize = 1;

    /// The root window, once prepared (0: none).
    static ROOT: AtomicIsize = AtomicIsize::new(0);
    /// The big icon we last set (0: still winit's), destroyed when replaced.
    static BIG_ICON: AtomicIsize = AtomicIsize::new(0);

    /// Call once from the app's creation, with the root window.
    pub fn prepare(cc: &eframe::CreationContext<'_>) {
        let Ok(RawWindowHandle::Win32(handle)) = cc.window_handle().map(|h| h.as_raw()) else {
            return;
        };
        let hwnd = handle.hwnd.get() as HWND;
        ROOT.store(hwnd as isize, Ordering::Relaxed);
        frameless(hwnd);
    }

    /// Sets the root window's big icon (the taskbar button) to `rgba`
    /// (`size` × `size`, straight alpha). winit's `set_window_icon`, which
    /// `ViewportCommand::Icon` reaches, only replaces the small one.
    pub fn set_taskbar_icon(rgba: &[u8], size: u32) {
        let hwnd = ROOT.load(Ordering::Relaxed) as HWND;
        if hwnd.is_null() {
            return;
        }
        // Top-down BGRA; the AND mask is empty (the alpha channel decides).
        let bgra: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect();
        let mask = vec![0u8; (size.div_ceil(32) * 4 * size) as usize];
        // SAFETY: the buffers hold `size` × `size` pixels in the layouts
        // CreateIcon expects (1 bpp mask, 32 bpp colour); `hwnd` is the live
        // root window; only icons created here are destroyed.
        unsafe {
            let icon = CreateIcon(
                std::ptr::null_mut(),
                size as i32,
                size as i32,
                1,
                32,
                mask.as_ptr(),
                bgra.as_ptr(),
            );
            if icon.is_null() {
                return;
            }
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, icon as LPARAM);
            let old = BIG_ICON.swap(icon as isize, Ordering::Relaxed);
            if old != 0 {
                DestroyIcon(old as HICON);
            }
        }
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

/// macOS: winit ignores window icons, and the Dock and Cmd+Tab show the
/// app's icon. eframe sets that once, on the first frame, from the window
/// icon, so `main.rs` gives it [`dock_icon`] (512 px, not the 64 px window
/// icon); afterwards the app sets it itself with the status dot.
///
/// Images are built from raw pixels (`NSBitmapImageRep`), never from PNG
/// data: on some macOS versions an `NSImage` from PNG loads an arbitrary
/// `libpng.dylib` and crashes (egui#7155).
#[cfg(target_os = "macos")]
pub mod macos {
    use objc2::rc::Retained;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBitmapImageRep, NSDeviceRGBColorSpace,
        NSImage,
    };
    use objc2_foundation::NSSize;

    use crate::icon::Status;
    use crate::icon_shape;

    /// The Dock icon's canvas, in pixels (Cmd+Tab shows it up to 128 pt @2x
    /// and more; 512 stays sharp).
    const CANVAS: u32 = 512;
    /// The shape inside it: Apple's icon grid leaves ~10 % around the body.
    const BODY: u32 = 416;

    thread_local! {
        /// The status the Dock icon shows (the main thread's).
        static SHOWN: std::cell::Cell<Status> = const { std::cell::Cell::new(Status::Idle) };
    }

    /// Sets the Dock icon to the app icon with `status`'s dot. Call on the
    /// main thread (`App::logic` is); elsewhere it does nothing.
    pub fn set_dock_icon(status: Status) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        SHOWN.set(status);
        let Some(image) = ns_image(&dock_rgba(status), CANVAS) else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: on the main thread, with a valid image.
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }

    /// The Dock icon for eframe's first-frame setting (see the module docs).
    pub fn dock_icon(status: Status) -> egui::IconData {
        egui::IconData {
            rgba: dock_rgba(status),
            width: CANVAS,
            height: CANVAS,
        }
    }

    /// Shows or hides the Dock icon: hidden while the window sits in the
    /// menu bar (an "accessory" app: no Dock icon, no app menu), back when
    /// the window shows (the caller then focuses it, which activates the
    /// app). Call on the main thread.
    pub fn set_dock_visible(visible: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
        if visible {
            // A Dock tile made again starts from the bundle's icon.
            set_dock_icon(SHOWN.get());
        }
    }

    /// The icon body centred on the transparent canvas.
    fn dock_rgba(status: Status) -> Vec<u8> {
        let dot = status.color().map(|c| [c.r(), c.g(), c.b()]);
        let body = icon_shape::draw(BODY, icon_shape::COLORED, dot);
        let margin = (CANVAS - BODY) / 2;
        let mut canvas = vec![0u8; (CANVAS * CANVAS * 4) as usize];
        for y in 0..BODY {
            let from = (y * BODY * 4) as usize;
            let to = (((y + margin) * CANVAS + margin) * 4) as usize;
            canvas[to..to + (BODY * 4) as usize]
                .copy_from_slice(&body[from..from + (BODY * 4) as usize]);
        }
        canvas
    }

    /// An `NSImage` of `size` × `size` straight-alpha RGBA pixels. The bitmap
    /// owns its buffer (AppKit allocates it; the pixels are copied in,
    /// premultiplied as it expects).
    fn ns_image(rgba: &[u8], size: u32) -> Option<Retained<NSImage>> {
        let side = size as isize;
        // SAFETY: null planes make AppKit allocate a buffer of
        // `side * 4 * side` bytes for 8-bit RGBA, meshed; the colour space
        // name is AppKit's own constant.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                side,
                side,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                side * 4,
                32,
            )
        }?;
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        let premultiplied = premultiply(rgba);
        // SAFETY: the buffer holds `size * size * 4` bytes (see above), as
        // many as `premultiplied`.
        unsafe {
            std::ptr::copy_nonoverlapping(premultiplied.as_ptr(), data, premultiplied.len());
        }
        let image = NSImage::initWithSize(
            NSImage::alloc(),
            NSSize::new(f64::from(size), f64::from(size)),
        );
        image.addRepresentation(&rep);
        Some(image)
    }

    /// Straight alpha to premultiplied, as `NSBitmapImageRep` stores it.
    fn premultiply(rgba: &[u8]) -> Vec<u8> {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, a]| {
                let scale = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
                [scale(r), scale(g), scale(b), a]
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_dock_icon_is_centred_and_premultiplied() {
            let rgba = dock_rgba(Status::Running);
            assert_eq!(rgba.len(), (CANVAS * CANVAS * 4) as usize);
            // Transparent margin, opaque body.
            assert_eq!(rgba[3], 0);
            let centre = ((CANVAS / 2 * CANVAS + CANVAS / 2) * 4) as usize;
            assert_eq!(rgba[centre + 3], 255);
            assert_eq!(premultiply(&[200, 100, 50, 255]), [200, 100, 50, 255]);
            assert_eq!(premultiply(&[200, 100, 50, 128]), [100, 50, 25, 128]);
            assert_eq!(premultiply(&[200, 100, 50, 0]), [0, 0, 0, 0]);
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
                    actions.extend(strip(ui, "sshelf", false, None));
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

    /// Streamer mode: the chip replaces the title; its menu flips *Hide host
    /// aliases and tags* (staying open) and turns streamer mode off.
    #[test]
    fn the_streamer_chip_and_its_menu() {
        let mut harness = test_util::harness()
            .with_size([800.0, 300.0])
            .build_ui_state(
                |ui, actions: &mut Vec<StripAction>| {
                    let streamer = Streamer {
                        hide_aliases: false,
                    };
                    ui.allocate_ui(egui::vec2(800.0, 44.0), |ui| {
                        actions.extend(strip(ui, "sshelf", false, Some(streamer)));
                    });
                },
                Vec::new(),
            );
        harness.run();
        assert!(harness.query_by_label("sshelf").is_none());
        harness.get_by_label("Streamer mode").click();
        harness.run();
        harness.get_by_label("Hide host aliases and tags").click();
        harness.run();
        harness.get_by_label("Turn off streamer mode").click();
        harness.run();
        assert_eq!(
            harness.state(),
            &[StripAction::HideAliases(true), StripAction::StreamerOff]
        );
        assert!(harness.query_by_label("Turn off streamer mode").is_none());
    }
}
