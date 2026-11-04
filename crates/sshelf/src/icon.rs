//! The app icon, drawn in code (no image assets yet): a rounded One Dark
//! square with three shelf bars, plus the [`Status`] dot (tray).

use sshelf_core::supervisor::{GroupState, Snapshot, TunnelState};
use sshelf_core::tunnels::Group;

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

/// RGBA pixels, row by row, with `status`'s dot.
pub fn rgba(status: Status) -> Vec<u8> {
    let p = &Palette::ONE_DARK;
    let size = SIZE as f32;
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut color = [0, 0, 0, 0];
            // Rounded square, 1 px margin, 6 px corner radius.
            if inside_rounded(fx, fy, 1.0, size - 1.0, 6.0) {
                color = rgb(p.accent);
            }
            // Three shelf bars.
            let bar = (8.0..24.0).contains(&fx)
                && [8.5, 14.5, 20.5]
                    .iter()
                    .any(|top| (*top..top + 3.0).contains(&fy));
            if bar {
                color = rgb(p.bg_dark);
            }
            if let Some(dot) = status.color() {
                let d = ((fx - 25.0).powi(2) + (fy - 25.0).powi(2)).sqrt();
                if d <= 6.5 {
                    color = rgb(if d <= 5.0 { dot } else { p.bg_dark });
                }
            }
            pixels.extend_from_slice(&color);
        }
    }
    pixels
}

pub fn egui_icon() -> egui::IconData {
    egui::IconData {
        rgba: rgba(Status::Idle),
        width: SIZE,
        height: SIZE,
    }
}

fn rgb(c: egui::Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), 255]
}

fn inside_rounded(x: f32, y: f32, min: f32, max: f32, radius: f32) -> bool {
    if !(min..max).contains(&x) || !(min..max).contains(&y) {
        return false;
    }
    let cx = x.clamp(min + radius, max - radius);
    let cy = y.clamp(min + radius, max - radius);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

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
