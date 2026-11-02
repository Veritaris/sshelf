//! UI-independent core of sshelf.
//!
//! Owns the app data model (metadata, tunnels, groups, settings, history),
//! versioned TOML storage and migrations, ssh binary discovery, `ssh -G`
//! resolution, host key checks and the tunnel supervisor. Must never depend on
//! egui; the UI talks to it through commands and events. See SPEC.md §2–§9.

pub mod history;
pub mod metadata;
pub mod orphans;
pub mod paths;
pub mod settings;
pub mod storage;
