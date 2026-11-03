//! Whether the desktop can show a tray icon (SPEC.md §10 "Tray"). Windows
//! and macOS always can. On Linux the icon is a StatusNotifierItem, which
//! only appears when a tray host has registered
//! `org.kde.StatusNotifierWatcher` on the session bus — GNOME without an
//! extension has none, and then *Minimize to tray* is not offered.

/// D-Bus name a tray host owns.
pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// Whether a tray icon would be visible. May block briefly on Linux (a
/// session bus round trip); call it off the UI thread.
pub fn available() -> Result<bool, String> {
    imp::available()
}

#[cfg(target_os = "linux")]
mod imp {
    use zbus::blocking::Connection;
    use zbus::blocking::fdo::DBusProxy;
    use zbus::names::BusName;

    pub fn available() -> Result<bool, String> {
        let connection = Connection::session().map_err(|e| format!("no D-Bus session bus: {e}"))?;
        let proxy = DBusProxy::new(&connection).map_err(|e| e.to_string())?;
        let name = BusName::try_from(super::WATCHER).map_err(|e| e.to_string())?;
        proxy.name_has_owner(name).map_err(|e| e.to_string())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn available() -> Result<bool, String> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn windows_and_macos_always_have_a_tray() {
        assert_eq!(super::available(), Ok(true));
    }

    #[test]
    fn watcher_name_is_a_valid_bus_name() {
        // Well-known names: dot-separated elements, no leading digit.
        assert!(
            super::WATCHER
                .split('.')
                .all(|e| !e.is_empty() && !e.starts_with(|c: char| c.is_ascii_digit()))
        );
    }
}
