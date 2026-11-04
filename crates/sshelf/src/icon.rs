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
pub fn rgba(status: Status) -> Vec<u8> {
    icon_shape::draw(SIZE, status.color().map(|c| [c.r(), c.g(), c.b()]))
}

/// The window's icon (taskbar button, Alt+Tab): drawn larger than the tray's
/// so the taskbar's 48 px at 150 % scales down, not up.
const WINDOW_SIZE: u32 = 64;

/// The window icon with `status`'s dot, as the tray icon has it.
pub fn egui_icon(status: Status) -> egui::IconData {
    egui::IconData {
        rgba: icon_shape::draw(WINDOW_SIZE, status.color().map(|c| [c.r(), c.g(), c.b()])),
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

    #[test]
    fn large_sizes_have_smooth_edges() {
        let big = icon_shape::draw(256, None);
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
        let idle = rgba(Status::Idle);
        assert_eq!(idle.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(pixel(&idle, 0, 0)[3], 0, "transparent corner");
        assert_eq!(pixel(&idle, 4, 16), rgb(p.accent));
        assert_eq!(pixel(&idle, 12, 9), rgb(p.bg_dark));
        assert_eq!(pixel(&rgba(Status::Running), 25, 25), rgb(p.green));
        assert_eq!(pixel(&rgba(Status::Busy), 25, 25), rgb(p.yellow));
        assert_eq!(pixel(&rgba(Status::Failed), 25, 25), rgb(p.red));
        assert_ne!(pixel(&idle, 25, 25), rgb(p.green));
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
