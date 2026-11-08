//! The app icon, drawn in code ([`icon_shape`], also the `.exe` icon): a
//! rounded Magenta square with three shelf bars, plus the [`Status`] dot
//! (tray).

use sshelf_core::supervisor::{GroupState, Snapshot, TunnelState};
use sshelf_core::tunnels::Group;

use crate::icon_shape;
use crate::theme::Palette;

/// The app's one status dot (SPEC.md §10 *Status marks*): green while
/// anything runs, yellow while starting or degraded, red when something
/// failed, none when idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Running,
    Busy,
    Failed,
}

impl Status {
    pub fn of(states: Option<&Snapshot>, groups: &[Group]) -> Self {
        let Some(states) = states else {
            return Status::Idle;
        };
        let tunnels = || states.tunnels.values().map(|t| &t.state);
        if tunnels().any(|s| matches!(s, TunnelState::Failed(_))) {
            Status::Failed
        } else if tunnels().any(|s| *s == TunnelState::Starting)
            || groups.iter().any(|g| {
                matches!(
                    states.group_state(g),
                    GroupState::Starting | GroupState::Degraded { .. }
                )
            })
        {
            Status::Busy
        } else if tunnels().any(|s| *s == TunnelState::Running) {
            Status::Running
        } else {
            Status::Idle
        }
    }

    pub fn color(self) -> Option<egui::Color32> {
        let p = &Palette::ONE_DARK;
        match self {
            Status::Idle => None,
            Status::Running => Some(p.green),
            Status::Busy => Some(p.yellow),
            Status::Failed => Some(p.red),
        }
    }

    /// The app mark's accessible name.
    pub fn describe(self) -> &'static str {
        match self {
            Status::Idle => "sshelf",
            Status::Running => "sshelf, tunnels running",
            Status::Busy => "sshelf, tunnels starting or degraded",
            Status::Failed => "sshelf, a tunnel failed",
        }
    }
}

/// Width and height in pixels.
pub const SIZE: u32 = 32;

/// RGBA pixels, row by row, with `status`'s dot (the shape is shared with
/// the `.exe` icon that `build.rs` writes).
/// `look`: the colours; `ink_dot`: the dot in the body's colour (a macOS
/// template image keeps only the alpha, so a coloured dot would be lost).
pub fn rgba(status: Status, look: icon_shape::Look, ink_dot: bool) -> Vec<u8> {
    let dot = status.color().map(|c| {
        if ink_dot {
            look.body
        } else {
            [c.r(), c.g(), c.b()]
        }
    });
    icon_shape::draw(SIZE, look, dot)
}

/// The window's icon (taskbar button, Alt+Tab): drawn larger than the tray's
/// so the taskbar's 48 px at 150 % scales down, not up.
const WINDOW_SIZE: u32 = 64;

/// The window icon with `status`'s dot, as the tray icon has it.
pub fn egui_icon(status: Status) -> egui::IconData {
    egui::IconData {
        rgba: icon_shape::draw(
            WINDOW_SIZE,
            icon_shape::COLORED,
            status.color().map(|c| [c.r(), c.g(), c.b()]),
        ),
        width: WINDOW_SIZE,
        height: WINDOW_SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: egui::Color32) -> [u8; 4] {
        [c.r(), c.g(), c.b(), 255]
    }

    #[test]
    fn the_shape_uses_the_palette() {
        let p = &Palette::ONE_DARK;
        assert_eq!(rgb(p.accent)[..3], icon_shape::ACCENT);
        assert_eq!(rgb(p.bg_dark)[..3], icon_shape::BG_DARK);
    }

    /// The Linux icon (an SVG for the desktop entry) is the same shape in
    /// the same colours, and the entry matches the window's app id.
    #[test]
    fn the_linux_files_match_the_app() {
        let svg = include_str!("../assets/linux/sshelf.svg");
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        assert!(svg.contains(&hex(icon_shape::ACCENT)));
        assert!(svg.contains(&hex(icon_shape::BG_DARK)));
        let entry = include_str!("../assets/linux/sshelf.desktop");
        let has = |line: &str| entry.lines().any(|l| l.trim_end() == line);
        assert!(has("Icon=sshelf"));
        assert!(has("StartupWMClass=sshelf"));
    }

    #[test]
    fn large_sizes_have_smooth_edges() {
        let big = icon_shape::draw(256, icon_shape::COLORED, None);
        assert_eq!(big.len(), 256 * 256 * 4);
        // Some pixel on the rounded corner is partly covered.
        let partial = (0..256 * 256).any(|i| {
            let a = big[i * 4 + 3];
            a > 0 && a < 255
        });
        assert!(partial);
    }

    fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn icon_has_corners_bars_and_a_status_dot() {
        let p = &Palette::ONE_DARK;
        let colored = |s| rgba(s, icon_shape::COLORED, false);
        let idle = colored(Status::Idle);
        assert_eq!(idle.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(pixel(&idle, 0, 0)[3], 0, "transparent corner");
        assert_eq!(pixel(&idle, 4, 16), rgb(p.accent));
        assert_eq!(pixel(&idle, 12, 9), rgb(p.bg_dark));
        assert_eq!(pixel(&colored(Status::Running), 25, 25), rgb(p.green));
        assert_eq!(pixel(&colored(Status::Busy), 25, 25), rgb(p.yellow));
        assert_eq!(pixel(&colored(Status::Failed), 25, 25), rgb(p.red));
        assert_ne!(pixel(&idle, 25, 25), rgb(p.green));
    }

    /// Monochrome: one colour, the bars and the dot's ring see-through; the
    /// dot coloured, or in the ink for a template image.
    #[test]
    fn monochrome_cuts_out_the_bars() {
        let p = &Palette::ONE_DARK;
        let white = icon_shape::Look {
            body: [255, 255, 255],
            cut: None,
        };
        let mono = rgba(Status::Running, white, false);
        assert_eq!(pixel(&mono, 4, 16), [255, 255, 255, 255]);
        assert_eq!(pixel(&mono, 12, 9)[3], 0, "bar cut out");
        assert_eq!(pixel(&mono, 25, 25), rgb(p.green));
        let template = rgba(Status::Running, white, true);
        assert_eq!(pixel(&template, 25, 25), [255, 255, 255, 255]);
    }

    fn status(state: TunnelState) -> sshelf_core::supervisor::TunnelStatus {
        sshelf_core::supervisor::TunnelStatus {
            state,
            owners: Default::default(),
            log: Default::default(),
            pid: None,
            failure: None,
            reconnect: None,
            running_since: None,
            after: None,
        }
    }

    #[test]
    fn status_prefers_failures_then_starts_then_running() {
        let mut states = Snapshot::default();
        assert_eq!(Status::of(None, &[]), Status::Idle);
        assert_eq!(Status::of(Some(&states), &[]), Status::Idle);
        states
            .tunnels
            .insert("a".into(), status(TunnelState::Running));
        assert_eq!(Status::of(Some(&states), &[]), Status::Running);
        states
            .tunnels
            .insert("b".into(), status(TunnelState::Starting));
        assert_eq!(Status::of(Some(&states), &[]), Status::Busy);
        states
            .tunnels
            .insert("c".into(), status(TunnelState::Failed("x".into())));
        assert_eq!(Status::of(Some(&states), &[]), Status::Failed);
        assert_eq!(Status::Failed.color(), Some(Palette::ONE_DARK.red));
        assert_eq!(Status::Idle.color(), None);
    }
}
