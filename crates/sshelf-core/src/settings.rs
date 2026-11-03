//! `settings.toml`: ssh binary override, terminal command template, close
//! behavior, appearance (host list style, reduced motion) and the tray menu.

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
    /// Hosts as a list or as cards (SPEC.md §10 *Visual design*).
    pub host_list_style: HostListStyle,
    /// How the cards view groups hosts.
    pub cards_group_by: CardsGroupBy,
    /// Every animation takes no time; the pulse of busy marks is static.
    pub reduce_motion: bool,
    /// What a right click on the tray icon opens (SPEC.md §10 *Tray*).
    pub tray_menu: TrayMenu,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostListStyle {
    #[default]
    List,
    Cards,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CardsGroupBy {
    #[default]
    Tag,
    /// The config file that defines the host.
    File,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayMenu {
    /// sshelf's own menu, docked to the icon; the system menu when the icon's
    /// position is unknown.
    #[default]
    Sshelf,
    /// Always the native menu of the `tray-icon` crate.
    System,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ssh_path: None,
            terminal_command: DEFAULT_TERMINAL_COMMAND.to_string(),
            close_behavior: CloseBehavior::Ask,
            host_list_style: HostListStyle::List,
            cards_group_by: CardsGroupBy::Tag,
            reduce_motion: false,
            tray_menu: TrayMenu::Sshelf,
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
            host_list_style: HostListStyle::Cards,
            cards_group_by: CardsGroupBy::None,
            reduce_motion: true,
            tray_menu: TrayMenu::System,
        };
        store.save(&settings).unwrap();
        let text = std::fs::read_to_string(store.path::<Settings>()).unwrap();
        for line in [
            "close_behavior = \"stop-and-quit\"",
            "host_list_style = \"cards\"",
            "cards_group_by = \"none\"",
            "reduce_motion = true",
            "tray_menu = \"system\"",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
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

    /// A file written before M9 has none of the appearance or tray keys: it
    /// still loads as version 1, with the defaults the spec names.
    #[test]
    fn files_without_the_m9_keys_load_with_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        std::fs::write(
            store.path::<Settings>(),
            "version = 1\nterminal_command = \"{ssh} {alias}\"\nclose_behavior = \"ask\"\n",
        )
        .unwrap();
        let loaded = store.load::<Settings>();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.data.terminal_command, "{ssh} {alias}");
        assert_eq!(loaded.data.host_list_style, HostListStyle::List);
        assert_eq!(loaded.data.cards_group_by, CardsGroupBy::Tag);
        assert!(!loaded.data.reduce_motion);
        assert_eq!(loaded.data.tray_menu, TrayMenu::Sshelf);
    }
}
