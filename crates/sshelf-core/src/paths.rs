//! Where sshelf reads and writes: the app data dir and the user's ssh files.

use std::ffi::OsString;
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
    /// Where kubectl and the kubeconfig are looked for (M13).
    pub kube: KubeEnv,
}

/// What kubectl discovery reads from the environment (SPEC.md §8
/// *kubectl*). Taken from the process only by [`Paths::detect`]; every other
/// `Paths` has none of it, so a test can never find the user's kubectl or
/// kubeconfig unless it puts a fake there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KubeEnv {
    /// `PATH`.
    pub path: OsString,
    /// `KUBECONFIG`, if set.
    pub kubeconfig: Option<OsString>,
    /// Install folders searched after `PATH`, with the name shown for each
    /// (Docker Desktop, Rancher Desktop, Homebrew).
    pub folders: Vec<(PathBuf, String)>,
}

impl KubeEnv {
    /// The process's environment.
    pub fn detect(home: &Path) -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        KubeEnv {
            path: var("PATH").unwrap_or_default(),
            kubeconfig: var("KUBECONFIG"),
            folders: crate::kubectl::install_folders(
                home,
                var("ProgramFiles").as_deref(),
                var("LOCALAPPDATA").as_deref(),
            ),
        }
    }
}

impl Paths {
    /// The platform config dir for sshelf (e.g. `%APPDATA%\sshelf`,
    /// `~/Library/Application Support/sshelf`, `~/.config/sshelf`).
    pub fn detect() -> Option<Self> {
        let dirs = BaseDirs::new()?;
        let mut paths = Self::new(dirs.home_dir(), dirs.config_dir().join("sshelf"));
        paths.kube = KubeEnv::detect(dirs.home_dir());
        Some(paths)
    }

    /// Paths under `home` with an empty [`KubeEnv`].
    pub fn new(home: &Path, app_dir: PathBuf) -> Self {
        let ssh_dir = home.join(".ssh");
        Self {
            app_dir,
            home: home.to_path_buf(),
            ssh_config: ssh_dir.join("config"),
            ssh_dir,
            kube: KubeEnv::default(),
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
