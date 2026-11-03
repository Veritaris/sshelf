//! `settings.toml`: ssh binary override and terminal command template.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::storage::Versioned;

/// Opens the detected terminal running ssh for the alias (SPEC.md §10).
pub const DEFAULT_TERMINAL_COMMAND: &str = "{terminal_cmd} {ssh} {alias}";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Overrides the detected ssh binary; companion tools come from its directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_path: Option<PathBuf>,
    /// Command template for "Connect" (see [`crate::terminal`]).
    pub terminal_command: String,
    /// What closing the window does while tunnels run (SPEC.md §10).
    pub close_behavior: CloseBehavior,
}

/// What closing the window does while tunnels run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseBehavior {
    /// Ask every time.
    #[default]
    Ask,
    /// Remembered choice: stop the tunnels and quit without asking.
    StopAndQuit,
    /// Remembered choice: hide the window, tunnels keep running in the tray.
    /// Asks instead when the desktop has no tray.
    MinimizeToTray,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ssh_path: None,
            terminal_command: DEFAULT_TERMINAL_COMMAND.to_string(),
            close_behavior: CloseBehavior::Ask,
        }
    }
}

impl Versioned for Settings {
    const FILE: &'static str = "settings.toml";
    const VERSION: u32 = 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{LoadStatus, Store};

    #[test]
    fn round_trip_and_defaults_for_missing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let settings = Settings {
            ssh_path: Some(PathBuf::from("/opt/ssh/bin/ssh")),
            terminal_command: "alacritty -e {ssh} {alias}".into(),
            close_behavior: CloseBehavior::StopAndQuit,
        };
        store.save(&settings).unwrap();
        let text = std::fs::read_to_string(store.path::<Settings>()).unwrap();
        assert!(
            text.contains("close_behavior = \"stop-and-quit\""),
            "{text}"
        );
        assert_eq!(store.load::<Settings>().data, settings);

        std::fs::write(store.path::<Settings>(), "version = 1\n").unwrap();
        let loaded = store.load::<Settings>();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.data, Settings::default());

        std::fs::write(
            store.path::<Settings>(),
            "version = 1\nclose_behavior = \"minimize-to-tray\"\n",
        )
        .unwrap();
        assert_eq!(
            store.load::<Settings>().data.close_behavior,
            CloseBehavior::MinimizeToTray
        );
    }
}
