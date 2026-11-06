//! The sshelf tray menu (SPEC.md §10 *Tray*; boards Tray, Tray-States): a
//! small borderless, always-on-top window docked by the tray icon, with the
//! same widgets as the main window — a status header, a banner while a host
//! key waits, group switches (*Retry* on degraded ones), the active tunnels
//! (*Stop* for a manual start; a failed one opens the window on it), the
//! hosts to connect to, *Show window* and *Quit*. It closes on a click
//! outside it, `Esc` or a click that opens something. On Windows it never
//! takes the foreground, so the taskbar's overflow flyout it may have been
//! opened from stays open (see `native`); elsewhere it closes on focus loss.
//!
//! It must work while the main window is hidden: a right click arrives in
//! `App::logic`, which asks eframe for a full pass of the hidden root
//! (`ViewportCommand::RequestPaintWhileHidden`); from then on the visible
//! flyout keeps the root's passes running, so [`TrayMenu::show`] is called
//! from `App::ui` like every other window.
//!
//! The menu is a deferred viewport: eframe creates its window from the event
//! loop and runs its passes on their own. (An immediate viewport is created
//! inside the root's pass, which a tray click can start where no window can
//! be made; egui then panics.) Its clicks go back to the root through
//! [`Shared`], and [`TrayMenu::poll`] takes them in `App::logic`: the root's
//! full passes can be held back (Windows hands `WM_PAINT` to one window at a
//! time), and the menu must neither show stale state nor lose a click.

use std::sync::{Arc, Mutex};

use egui::{
    Align, Align2, CursorIcon, FontId, Id, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, pos2, vec2,
};
use sshelf_core::supervisor::{GroupState, TunnelState};

use crate::theme::{Palette, radius};
use crate::tray::{IconRect, TrayAction, TrayModel};
use crate::widgets::button::Button;
use crate::widgets::switch::switch;
use crate::widgets::{marks, masked, nav};
use crate::{fonts, icons};

const WIDTH: f32 = 340.0;
/// Padding inside the window.
const PAD: f32 = 12.0;
const HEADER: f32 = 34.0;
/// A section's overline, with the space above it.
const SECTION: f32 = 26.0;
const ROW: f32 = 36.0;
/// The host key banner.
const BANNER: f32 = 48.0;
/// The line and the *Show window* / *Quit* buttons.
const FOOTER: f32 = 46.0;
/// Hosts to connect to (the native menu has more).
const RECENT: usize = 5;
/// Space between the icon and the menu.
const GAP: f32 = 8.0;

#[derive(Default)]
pub struct TrayMenu {
    /// Open, by this icon.
    open: Option<IconRect>,
    /// Shared with the menu's own window, which runs its passes apart from
    /// the root's (a deferred viewport).
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    /// What to show; the root keeps it current.
    model: TrayModel,
    /// Clicked in the menu, for the root to carry out.
    actions: Vec<TrayAction>,
    /// The menu asked to close.
    close: bool,
    /// It had the focus once: losing it closes the menu (not on Windows,
    /// where it never takes the focus).
    #[cfg(not(target_os = "windows"))]
    had_focus: bool,
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("sshelf_tray_menu")
}

/// The menu's height for `model` (every row has a fixed height).
fn height(model: &TrayModel) -> f32 {
    let mut height = PAD * 2.0 + HEADER + ROW + FOOTER;
    if model.host_key_waiting {
        height += BANNER;
    }
    if !model.groups.is_empty() {
        height += SECTION + model.groups.len() as f32 * ROW;
    }
    if !model.tunnels.is_empty() {
        height += SECTION + model.tunnels.len() as f32 * ROW;
    }
    height + SECTION + model.recent.len().clamp(1, RECENT) as f32 * ROW
}

/// Where the menu goes, in points: centred on the icon, above it when the
/// icon is in the lower half of the screen (a bottom taskbar), else below,
/// kept on the screen.
fn placement(
    icon: IconRect,
    pixels_per_point: f32,
    screen: egui::Vec2,
    size: egui::Vec2,
) -> egui::Pos2 {
    let ppp = f64::from(pixels_per_point);
    let (x, y) = ((icon.x / ppp) as f32, (icon.y / ppp) as f32);
    let (w, h) = ((icon.width / ppp) as f32, (icon.height / ppp) as f32);
    let left = (x + w / 2.0 - size.x / 2.0).clamp(GAP, (screen.x - size.x - GAP).max(GAP));
    let top = if y + h / 2.0 > screen.y / 2.0 {
        y - size.y - GAP
    } else {
        y + h + GAP
    };
    pos2(left, top.clamp(GAP, (screen.y - size.y - GAP).max(GAP)))
}

impl TrayMenu {
    /// Opens the menu by `icon`; call from `App::logic` (the root may be
    /// hidden, so it asks for a pass anyway).
    pub fn open(&mut self, ctx: &egui::Context, icon: IconRect) {
        self.open = Some(icon);
        {
            let mut shared = lock(&self.shared);
            shared.close = false;
            #[cfg(not(target_os = "windows"))]
            {
                shared.had_focus = false;
            }
            shared.actions.clear();
        }
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::RequestPaintWhileHidden);
        ctx.request_repaint();
    }

    /// Keeps the open menu's model current and returns what was clicked in
    /// it since the last call; call from `App::logic`, which runs even when
    /// the root gets no full pass (hidden, or its redraws held back), so the
    /// menu never shows stale state or loses a click.
    pub fn poll(&mut self, ctx: &egui::Context, model: &TrayModel) -> Vec<TrayAction> {
        if self.open.is_none() {
            return Vec::new();
        }
        let (actions, close) = {
            let mut shared = lock(&self.shared);
            if shared.model != *model {
                shared.model = model.clone();
                ctx.request_repaint_of(viewport_id());
            }
            (std::mem::take(&mut shared.actions), shared.close)
        };
        if close {
            self.open = None;
            #[cfg(target_os = "windows")]
            native::detach();
            // The next full pass doesn't show it, which removes its window
            // (it hid itself already).
            ctx.request_repaint();
        }
        actions
    }

    /// The menu while open. Call from `App::ui` on every pass: its window
    /// lives as long as it is shown each pass.
    pub fn show(&mut self, ctx: &egui::Context) {
        let Some(icon) = self.open else {
            return;
        };
        let model = lock(&self.shared).model.clone();
        let size = vec2(WIDTH, height(&model));
        let (ppp, screen) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.native_pixels_per_point.unwrap_or(1.0),
                viewport.monitor_size.unwrap_or(vec2(1920.0, 1080.0)),
            )
        });
        let builder = ViewportBuilder::default()
            .with_title("sshelf menu")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            // Windows: shown without taking the foreground (see `native`).
            .with_active(!cfg!(target_os = "windows"))
            .with_position(placement(icon, ppp, screen, size))
            .with_inner_size(size);
        let shared = self.shared.clone();
        ctx.show_viewport_deferred(viewport_id(), builder, move |ui, class| {
            let model = lock(&shared).model.clone();
            let (escape, close_requested) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Escape),
                    i.viewport().close_requested(),
                )
            });
            // No native windows (tests): nothing to dock.
            let mut close = class == ViewportClass::EmbeddedWindow;
            // Windows: never focused; the hooks see clicks outside and Esc.
            #[cfg(target_os = "windows")]
            {
                if !close {
                    native::attach(ui.ctx());
                }
                close |= native::take_dismissed();
            }
            // Elsewhere it takes the focus and closes when it loses it.
            #[cfg(not(target_os = "windows"))]
            {
                let focused = ui.input(|i| i.viewport().focused);
                let had_focus = lock(&shared).had_focus;
                if focused != Some(true) && !had_focus {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
                }
                close |= had_focus && focused == Some(false);
                lock(&shared).had_focus |= focused == Some(true);
            }
            close |= escape || close_requested;
            let mut actions = Vec::new();
            if !close {
                contents(ui, &model, &mut actions);
            }
            // Anything that opens or quits closes the menu; switches don't.
            close |= actions.iter().any(|a| {
                matches!(
                    a,
                    TrayAction::Show
                        | TrayAction::Quit
                        | TrayAction::ShowTunnel(_)
                        | TrayAction::Connect(_)
                )
            });
            if close && class != ViewportClass::EmbeddedWindow {
                // Gone at once, before the root drops the window.
                ui.ctx().send_viewport_cmd(ViewportCommand::Visible(false));
            }
            let mut shared = lock(&shared);
            if close || !actions.is_empty() {
                shared.close |= close;
                shared.actions.extend(actions);
                // The root's `logic` carries them out (see `poll`).
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
    }
}

/// Everything inside the window.
fn contents(ui: &mut Ui, model: &TrayModel, actions: &mut Vec<TrayAction>) {
    let rect = ui.ctx().content_rect();
    contents_in(ui, rect, model, actions);
}

/// The menu as a picture at `rect` (the tour's popover): what it shows now,
/// without reacting to clicks.
pub fn picture(ui: &mut Ui, rect: egui::Rect, model: &TrayModel) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.disable();
    contents_in(&mut child, rect, model, &mut Vec::new());
}

/// The menu's size for `model`.
pub fn size(model: &TrayModel) -> egui::Vec2 {
    vec2(WIDTH, height(model))
}

fn contents_in(ui: &mut Ui, rect: egui::Rect, model: &TrayModel, actions: &mut Vec<TrayAction>) {
    let p = &Palette::ONE_DARK;
    ui.painter().rect_filled(rect, radius::LARGE, p.bg_dark);
    ui.painter().rect_stroke(
        rect,
        radius::LARGE,
        Stroke::new(1.0, p.selection),
        StrokeKind::Inside,
    );
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(PAD))
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut ui;
    ui.spacing_mut().item_spacing.y = 0.0;

    // Header: the name and what runs.
    fixed(ui, HEADER, |ui| {
        ui.add_space(6.0);
        ui.label(
            RichText::new(if model.tour {
                "sshelf · Tour"
            } else {
                "sshelf"
            })
            .font(fonts::semibold(14.0))
            .color(p.text_strong),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            if model.failed > 0 {
                ui.label(
                    RichText::new(format!("{} failed", model.failed))
                        .size(12.0)
                        .color(p.red),
                );
            }
            if model.running > 0 || model.failed == 0 {
                let (text, color) = if model.running > 0 {
                    (format!("{} running", model.running), p.green)
                } else {
                    ("nothing running".to_string(), p.dim)
                };
                if model.failed > 0 {
                    ui.label(RichText::new("·").size(12.0).color(p.dim));
                }
                ui.label(RichText::new(text).size(12.0).color(color));
            }
        });
    });

    // Streamer mode, at the top: it can be turned on with the window hidden.
    fixed(ui, ROW, |ui| {
        let rect = ui.max_rect();
        // Painted: the switch carries the name.
        ui.painter().text(
            pos2(rect.left() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            "Streamer mode",
            FontId::proportional(13.0),
            p.text,
        );
        ui.painter().hline(
            rect.x_range().shrink(6.0),
            rect.bottom() - 0.5,
            Stroke::new(1.0, p.selection),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            let mut on = model.streamer;
            if switch(ui, &mut on, "Streamer mode").changed() {
                actions.push(TrayAction::SetStreamer(on));
            }
        });
    });

    if model.host_key_waiting {
        fixed(ui, BANNER, |ui| {
            let banner = ui.max_rect().shrink2(vec2(0.0, 4.0));
            ui.painter()
                .rect_filled(banner, radius::CONTROL, p.blue_tint);
            ui.add_space(10.0);
            icons::show(ui, icons::KEY_ROUND, icons::INLINE, p.blue);
            ui.add_space(6.0);
            ui.label(
                RichText::new("A host key waits for you")
                    .size(12.0)
                    .color(p.blue),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.add(Button::new("Show").small()).clicked() {
                    actions.push(TrayAction::Show);
                }
            });
        });
    }

    if !model.groups.is_empty() {
        section(ui, "Groups");
        for group in &model.groups {
            fixed(ui, ROW, |ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(&group.name).size(13.0).color(p.text));
                match group.state {
                    GroupState::Degraded { running, total } => {
                        ui.label(
                            RichText::new(format!("{running}/{total}"))
                                .size(11.0)
                                .color(p.yellow),
                        );
                    }
                    GroupState::Starting => {
                        ui.label(RichText::new("starting…").size(11.0).color(p.yellow));
                    }
                    _ => {}
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(10.0);
                    let active = group.state != GroupState::Stopped;
                    let mut on = active;
                    if switch(ui, &mut on, &group.name).changed() {
                        actions.push(if active {
                            TrayAction::StopGroup(group.id.clone())
                        } else {
                            TrayAction::StartGroup(group.id.clone())
                        });
                    }
                    if matches!(group.state, GroupState::Degraded { .. })
                        && ui
                            .add(Button::new("Retry").icon(icons::ROTATE_CW).small())
                            .clicked()
                    {
                        actions.push(TrayAction::StartGroup(group.id.clone()));
                    }
                });
            });
        }
    }

    if !model.tunnels.is_empty() {
        section(ui, "Active tunnels");
        let slot = ui.painter().add(egui::Shape::Noop);
        let mut hovered = None;
        for (index, tunnel) in model.tunnels.iter().enumerate() {
            let failed = matches!(tunnel.state, TunnelState::Failed(_));
            let response = row(ui, failed, |ui| {
                ui.add_space(10.0);
                match &tunnel.state {
                    TunnelState::Running => marks::dot(ui, p.green),
                    TunnelState::Failed(_) => marks::dot(ui, p.red),
                    TunnelState::Starting => marks::pulsing_ring(ui, marks::DOT, p.yellow),
                    _ => marks::pulsing_ring(ui, marks::DOT, p.dim),
                };
                ui.add_space(8.0);
                marks::type_mark(ui, tunnel.kind, None);
                ui.add_space(2.0);
                ui.label(RichText::new(&tunnel.name).size(13.0).color(p.text));
                let (note, color) = if failed {
                    let text = if tunnel.reconnecting {
                        "reconnecting"
                    } else {
                        "failed"
                    };
                    (text.to_string(), p.red)
                } else if !tunnel.manual {
                    (format!("used by {}", tunnel.used_by.join(", ")), p.dim)
                } else if model.streamer {
                    // Streamer mode: the port is masked (not peekable here).
                    (
                        tunnel.port.map(|_| ":".to_string()).unwrap_or_default(),
                        p.dim,
                    )
                } else {
                    (
                        tunnel.port.map(|p| format!(":{p}")).unwrap_or_default(),
                        p.dim,
                    )
                };
                ui.label(
                    RichText::new(note)
                        .font(FontId::monospace(11.0))
                        .color(color),
                );
                if model.streamer && tunnel.manual && !failed && tunnel.port.is_some() {
                    ui.add_space(-4.0);
                    masked::dots(ui, false);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    let name = format!("Stop {}", tunnel.name);
                    if tunnel.manual
                        && ui
                            .add(Button::icon_only(icons::SQUARE, &name).small())
                            .clicked()
                    {
                        actions.push(TrayAction::StopTunnel(tunnel.id.clone()));
                    }
                });
            });
            if failed && response.hovered() {
                hovered = Some((index, response.rect));
            }
            if failed && response.clicked() {
                actions.push(TrayAction::ShowTunnel(tunnel.id.clone()));
            }
        }
        let id = Id::unique("tray_tunnels_hover");
        nav::hover_highlight(ui, id, slot, hovered, p.surface);
    }

    section(ui, "Connect");
    if model.recent.is_empty() {
        fixed(ui, ROW, |ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new("No sessions launched from sshelf yet")
                    .size(12.0)
                    .color(p.dim),
            );
        });
    }
    let slot = ui.painter().add(egui::Shape::Noop);
    let mut hovered = None;
    for (index, host) in model.recent.iter().take(RECENT).enumerate() {
        let response = row(ui, true, |ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new(&host.shown)
                    .font(FontId::monospace(12.0))
                    .color(p.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(10.0);
                icons::show(ui, icons::SQUARE_TERMINAL, icons::INLINE, p.dim);
            });
        });
        if response.hovered() {
            hovered = Some((index, response.rect));
        }
        if response.clicked() {
            actions.push(TrayAction::Connect(host.alias.clone()));
        }
    }
    // Slides between hosts (50 ms), as in every list and menu.
    let id = Id::unique("tray_connect_hover");
    nav::hover_highlight(ui, id, slot, hovered, p.surface);

    fixed(ui, FOOTER, |ui| {
        let line = ui.max_rect();
        ui.painter().hline(
            line.x_range(),
            line.top() + 6.0,
            Stroke::new(1.0, p.selection),
        );
        ui.spacing_mut().item_spacing.x = 6.0;
        let quit_width = 64.0;
        let show = Button::new("Show window")
            .small()
            .min_width(ui.available_width() - quit_width - 6.0);
        if ui.add(show).clicked() {
            actions.push(TrayAction::Show);
        }
        if ui
            .add(
                Button::new("Quit")
                    .small()
                    .red_on_hover()
                    .min_width(quit_width),
            )
            .clicked()
        {
            actions.push(TrayAction::Quit);
        }
    });
}

/// A strip `height` tall across the menu, laid out left to right.
fn fixed(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    add(&mut child);
}

/// A section's overline.
fn section(ui: &mut Ui, title: &str) {
    let p = &Palette::ONE_DARK;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), SECTION), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 10.0, rect.bottom() - 8.0),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        fonts::semibold(11.0),
        p.dim,
    );
}

/// A row; `clickable` ones get the hand cursor (the caller paints the
/// sliding hover background).
fn row(ui: &mut Ui, clickable: bool, add: impl FnOnce(&mut Ui)) -> egui::Response {
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    add(&mut child);
    if clickable {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Windows: the menu never activates, so opening it from the taskbar's
/// overflow flyout leaves the flyout open (it closes when it loses the
/// foreground). Without the focus, a click outside and `Esc` are seen by
/// low-level hooks, which run on a thread of their own with its own message
/// loop (never inside an egui pass). The window also gets the frameless fix
/// (no accent-colour border).
#[cfg(target_os = "windows")]
mod native {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, EnumWindows, GWL_EXSTYLE, GetMessageW, GetWindowLongPtrW, GetWindowRect,
        GetWindowTextW, GetWindowThreadProcessId, HC_ACTION, IsWindowVisible, KBDLLHOOKSTRUCT, MSG,
        MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowLongPtrW,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN,
        WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_XBUTTONDOWN, WS_EX_NOACTIVATE,
    };

    const TITLE: &str = "sshelf menu";
    const VK_ESCAPE: u32 = 0x1B;

    /// The menu's window, once found (0: none).
    static MENU: AtomicIsize = AtomicIsize::new(0);
    /// A click outside or `Esc` since the menu opened.
    static DISMISSED: AtomicBool = AtomicBool::new(false);
    /// Repaints the menu when the hooks see something.
    static CTX: Mutex<Option<egui::Context>> = Mutex::new(None);
    /// The hook thread's id, to stop it.
    static HOOKS: Mutex<Option<u32>> = Mutex::new(None);

    /// Finds the menu's window and prepares it, and starts the hooks; call
    /// on every pass of the menu (it does its work once per window).
    pub fn attach(ctx: &egui::Context) {
        if MENU.load(Ordering::Relaxed) != 0 {
            return;
        }
        let Some(hwnd) = find() else {
            return;
        };
        crate::window::native::frameless(hwnd);
        // SAFETY: `hwnd` is the menu's live window, found in this process.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE as isize);
        }
        MENU.store(hwnd as isize, Ordering::Relaxed);
        DISMISSED.store(false, Ordering::Relaxed);
        *CTX.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx.clone());
        start_hooks();
    }

    /// A click outside the menu or `Esc` happened (once).
    pub fn take_dismissed() -> bool {
        DISMISSED.swap(false, Ordering::Relaxed)
    }

    /// The menu closed: stops the hooks and forgets the window.
    pub fn detach() {
        MENU.store(0, Ordering::Relaxed);
        DISMISSED.store(false, Ordering::Relaxed);
        if let Some(thread) = HOOKS.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // SAFETY: posting to a thread id; it fails harmlessly if gone.
            unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) };
        }
        *CTX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// This process's visible window titled "sshelf menu".
    fn find() -> Option<HWND> {
        let mut found: HWND = std::ptr::null_mut();
        // SAFETY: the callback only reads window properties and writes
        // `found`, which outlives the synchronous `EnumWindows` call.
        unsafe {
            EnumWindows(Some(each), &mut found as *mut HWND as LPARAM);
        }
        (!found.is_null()).then_some(found)
    }

    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> i32 {
        // SAFETY: called by `EnumWindows` with live windows; `found` is the
        // pointer `find` passed.
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != GetCurrentProcessId() || IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            let mut text = [0u16; 32];
            let len = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
            if String::from_utf16_lossy(&text[..len.max(0) as usize]) == TITLE {
                *(found as *mut HWND) = hwnd;
                return 0;
            }
            1
        }
    }

    fn start_hooks() {
        let mut hooks = HOOKS.lock().unwrap_or_else(|e| e.into_inner());
        if hooks.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: plain Win32 calls on this thread; the hook procedures
            // are functions that live as long as the program.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                // Makes the thread's message queue, so WM_QUIT can reach it.
                PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                let module = GetModuleHandleW(std::ptr::null());
                let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
                let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
                let _ = tx.send(GetCurrentThreadId());
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
                UnhookWindowsHookEx(mouse);
                UnhookWindowsHookEx(keys);
            }
        });
        *hooks = rx.recv().ok();
    }

    fn dismiss() {
        DISMISSED.store(true, Ordering::Relaxed);
        if let Some(ctx) = CTX.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            ctx.request_repaint_of(super::viewport_id());
        }
    }

    unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: for HC_ACTION, `lparam` points at an MSLLHOOKSTRUCT;
        // `MENU` holds a window of this process (or 0).
        unsafe {
            let down = matches!(
                wparam as u32,
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
            );
            let menu = MENU.load(Ordering::Relaxed) as HWND;
            if code == HC_ACTION as i32 && down && !menu.is_null() {
                let at = (*(lparam as *const MSLLHOOKSTRUCT)).pt;
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(menu, &mut rect);
                let inside = at.x >= rect.left
                    && at.x < rect.right
                    && at.y >= rect.top
                    && at.y < rect.bottom;
                if !inside {
                    dismiss();
                }
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
    }

    unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: for HC_ACTION, `lparam` points at a KBDLLHOOKSTRUCT.
        unsafe {
            if code == HC_ACTION as i32
                && wparam as u32 == WM_KEYDOWN
                && (*(lparam as *const KBDLLHOOKSTRUCT)).vkCode == VK_ESCAPE
                && MENU.load(Ordering::Relaxed) != 0
            {
                dismiss();
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::tray::{GroupRow, RecentHost, TunnelRow};

    fn model() -> TrayModel {
        TrayModel {
            groups: vec![GroupRow {
                id: "g-1".into(),
                name: "Work".into(),
                state: GroupState::Degraded {
                    running: 1,
                    total: 2,
                },
            }],
            tunnels: vec![TunnelRow {
                id: "t-a".into(),
                name: "API".into(),
                port: Some(6443),
                kind: sshelf_core::tunnels::TunnelType::Kubectl,
                state: TunnelState::Running,
                manual: true,
                reconnecting: false,
                used_by: Vec::new(),
            }],
            recent: vec![RecentHost::new("web"), RecentHost::new("db")],
            host_key_waiting: true,
            ..TrayModel::default()
        }
    }

    #[test]
    fn height_counts_every_row() {
        let model = model();
        let expected = PAD * 2.0
            + HEADER
            + ROW
            + FOOTER
            + BANNER
            + (SECTION + ROW)
            + (SECTION + ROW)
            + (SECTION + 2.0 * ROW);
        assert_eq!(height(&model), expected);
        let empty = TrayModel::default();
        assert_eq!(
            height(&empty),
            PAD * 2.0 + HEADER + ROW + FOOTER + SECTION + ROW
        );
    }

    /// Streamer mode: ports show as dots (no peeking in the tray); the
    /// switch turns it off.
    #[test]
    fn streamer_mode_masks_ports_and_its_switch_turns_it_off() {
        let model = TrayModel {
            streamer: true,
            ..model()
        };
        let mut actions = Vec::new();
        let mut harness = crate::test_util::harness()
            .with_size([WIDTH, height(&model)])
            .build_ui(|ui| contents(ui, &model, &mut actions));
        harness.step();
        let drawn = crate::test_util::drawn_text(&harness);
        assert!(!drawn.contains("6443"), "{drawn}");
        harness.get_by_label(sshelf_core::redact::DOTS);
        harness
            .get_by_role_and_label(egui::accesskit::Role::Switch, "Streamer mode")
            .click();
        harness.step();
        drop(harness);
        assert_eq!(actions, [TrayAction::SetStreamer(false)]);
    }

    #[test]
    fn it_docks_above_a_bottom_taskbar_and_below_a_top_bar() {
        let screen = vec2(1920.0, 1080.0);
        let size = vec2(WIDTH, 300.0);
        let icon = |y| IconRect {
            x: 1800.0,
            y,
            width: 24.0,
            height: 24.0,
        };
        let bottom = placement(icon(1050.0), 1.0, screen, size);
        assert_eq!(bottom.y, 1050.0 - 300.0 - GAP);
        // Kept on the screen at the right edge.
        assert_eq!(bottom.x, 1920.0 - WIDTH - GAP);
        let top = placement(icon(4.0), 1.0, screen, size);
        assert_eq!(top.y, 4.0 + 24.0 + GAP);
        // Physical pixels at 150 %.
        let scaled = placement(icon(1575.0), 1.5, screen, size);
        assert_eq!(scaled.y, 1050.0 - 300.0 - GAP);
    }

    /// `poll` (from `App::logic`) keeps the model current and carries out the
    /// menu's clicks and its close, with no full pass of the root at all.
    #[test]
    fn poll_works_without_the_roots_ui() {
        let ctx = egui::Context::default();
        let icon = IconRect {
            x: 0.0,
            y: 0.0,
            width: 24.0,
            height: 24.0,
        };
        let mut menu = TrayMenu::default();
        assert!(menu.poll(&ctx, &model()).is_empty(), "closed: nothing");
        menu.open(&ctx, icon);
        lock(&menu.shared)
            .actions
            .push(TrayAction::StartGroup("g-1".into()));
        assert_eq!(
            menu.poll(&ctx, &model()),
            [TrayAction::StartGroup("g-1".into())]
        );
        assert_eq!(lock(&menu.shared).model, model());
        lock(&menu.shared).close = true;
        menu.poll(&ctx, &model());
        assert!(menu.open.is_none());
    }

    #[test]
    fn the_rows_carry_their_actions() {
        let model = model();
        let mut actions = Vec::new();
        let mut harness = crate::test_util::harness()
            .with_size([WIDTH, height(&model)])
            .build_ui(|ui| contents(ui, &model, &mut actions));
        harness.step();
        harness
            .get_by_role_and_label(egui::accesskit::Role::Switch, "Work")
            .click();
        harness.step();
        harness.get_by_label("Retry").click();
        harness.step();
        harness.get_by_label("Stop API").click();
        harness.step();
        harness.get_by_label("Quit").click();
        harness.step();
        harness.get_by_label("A host key waits for you");
        drop(harness);
        assert_eq!(
            actions,
            [
                TrayAction::StopGroup("g-1".into()),
                TrayAction::StartGroup("g-1".into()),
                TrayAction::StopTunnel("t-a".into()),
                TrayAction::Quit,
            ]
        );
    }
}
