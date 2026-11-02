//! Where sshelf reads and writes: the app data dir and the user's ssh files.

use std::path::{Path, PathBuf};

use directories::BaseDirs;
use sshconf::ResolveOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// sshelf's own data (`settings.toml`, `hosts.toml`, `history.toml`).
    pub app_dir: PathBuf,
    pub home: PathBuf,
    /// `~/.ssh`: base for relative `Include` paths.
    pub ssh_dir: PathBuf,
    /// `~/.ssh/config`. Only ever read.
    pub ssh_config: PathBuf,
}

impl Paths {
    /// The platform config dir for sshelf (e.g. `%APPDATA%\sshelf`,
    /// `~/Library/Application Support/sshelf`, `~/.config/sshelf`).
    pub fn detect() -> Option<Self> {
        let dirs = BaseDirs::new()?;
        Some(Self::new(dirs.home_dir(), dirs.config_dir().join("sshelf")))
    }

    pub fn new(home: &Path, app_dir: PathBuf) -> Self {
        let ssh_dir = home.join(".ssh");
        Self {
            app_dir,
            home: home.to_path_buf(),
            ssh_config: ssh_dir.join("config"),
            ssh_dir,
        }
    }

    pub fn resolve_options(&self) -> ResolveOptions {
        ResolveOptions {
            home: self.home.clone(),
            ssh_dir: self.ssh_dir.clone(),
            max_depth: sshconf::MAX_INCLUDE_DEPTH,
        }
    }
}
