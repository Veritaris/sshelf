//! The onboarding tour (SPEC.md §10 *Onboarding tour*, M10): 27 steps on the
//! real screens against a sandbox Core with sample data. This module holds
//! the step machine and its parts; `app::tour` drives it against the app's
//! views and Cores.
//!
//! - [`steps`]: the steps and the glossary, as data.
//! - [`targets`]: where views register the widgets a step points at, and the
//!   clicks *Show me* makes.
//! - [`overlay`]: dimming, spotlight, popover, glint.
//! - [`sandbox`]: the temporary home and app data with the sample bundle.
//! - [`welcome`]: the first-launch screen.

pub mod overlay;
pub mod sandbox;
pub mod steps;
pub mod targets;
pub mod welcome;

use steps::{Action, STEPS, Step};
use targets::Target;

/// Frames a missing target is waited for before the step goes without a
/// spotlight.
pub const LATE_FRAMES: u32 = 30;

/// What the tour saw happen in the sandbox (for steps whose "done" is an
/// event rather than a state).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seen {
    /// Connect ran (as a toast).
    pub connected: bool,
    /// An edit was saved.
    pub saved_edit: bool,
    /// *Minimize to tray* was chosen in the close dialog.
    pub minimized: bool,
    /// *Install on server* was clicked.
    pub install_requested: bool,
}

/// The step machine.
#[derive(Debug)]
pub struct Tour {
    pub step: usize,
    pub seen: Seen,
    /// The step's screen is still to be applied (just entered).
    pub enter: bool,
    /// The step's action was already done when it was entered: no
    /// automatic advance then.
    pub done_on_enter: Option<bool>,
    /// Frames the spotlight's targets have been missing.
    pub missing: u32,
}

impl Tour {
    pub fn new(step: usize) -> Self {
        Self {
            step: step.min(STEPS.len() - 1),
            seen: Seen::default(),
            enter: true,
            done_on_enter: None,
            missing: 0,
        }
    }

    pub fn current(&self) -> &'static Step {
        &STEPS[self.step]
    }

    pub fn is_last(&self) -> bool {
        self.step + 1 == STEPS.len()
    }

    /// Enters step `index` (its screen is applied next frame).
    pub fn go(&mut self, index: usize) {
        self.step = index.min(STEPS.len() - 1);
        self.enter = true;
        self.done_on_enter = None;
        self.missing = 0;
    }

    /// After the step's action changed to `done`: whether to move on by
    /// itself. Quick actions (a click that opens something) advance at once;
    /// long ones (starting, checking) wait for *Next*.
    pub fn advances(&mut self, done: bool) -> bool {
        let Some((action, _)) = self.current().action else {
            return false;
        };
        let was = *self.done_on_enter.get_or_insert(done);
        done && !was && !is_long(action)
    }
}

/// Actions that take time: their steps stay until *Next*.
pub fn is_long(action: Action) -> bool {
    matches!(
        action,
        Action::CheckAll
            | Action::StartDbTunnel
            | Action::StartBackup
            | Action::GenerateKey
            | Action::InstallKey
            | Action::StartGroup
    )
}

/// The widget *Show me* clicks for `action` (and the one that glints).
pub fn click_target(action: Action) -> Target {
    match action {
        Action::Connect => Target::Connect,
        Action::CheckAll => Target::CheckAll,
        Action::EffectiveConfig => Target::EffectiveConfigTab,
        Action::Edit => Target::Edit,
        Action::RawText => Target::RawTextTab,
        Action::SaveEdit => Target::EditorSave,
        Action::OpenTunnels => Target::RailTunnels,
        Action::StartDbTunnel => Target::TunnelAction(steps::TOUR_DB),
        Action::NewTunnel => Target::NewTunnel,
        Action::SaveTunnel => Target::TunnelSave,
        Action::StartBackup => Target::TunnelAction(steps::TOUR_BACKUP),
        Action::OpenKeygen => Target::BannerGenerateKey,
        Action::GenerateKey => Target::KeyGenerate,
        Action::InstallKey => Target::KeyInstall,
        Action::OpenGroups => Target::RailGroups,
        Action::StartGroup => Target::GroupSwitch(steps::TOUR_STAGING_GROUP),
        Action::EditGroup => Target::GroupEdit,
        Action::SaveGroup => Target::GroupSave,
        Action::Close => Target::WindowClose,
        Action::MinimizeToTray => Target::CloseMinimize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_actions_advance_once_and_long_ones_wait() {
        let mut tour = Tour::new(1); // Connect: quick.
        assert!(!tour.advances(false));
        assert!(tour.advances(true));

        let mut tour = Tour::new(9); // Start a tunnel: long.
        assert!(!tour.advances(false));
        assert!(!tour.advances(true));

        // Done already when entered (Back): no jump.
        let mut tour = Tour::new(5);
        assert!(!tour.advances(true));
        assert!(!tour.advances(true));

        // Watch steps never advance by themselves.
        let mut tour = Tour::new(0);
        assert!(!tour.advances(true));
    }
}
