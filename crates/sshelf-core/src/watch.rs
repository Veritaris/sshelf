//! Watches the directories a config set depends on (SPEC.md §4 "External
//! changes") and reports each burst of relevant changes once, after a short
//! quiet period.
//!
//! A directory that does not exist yet is watched through its nearest
//! existing ancestor; events there only count if they lead towards it.
//! Access events are ignored, so reading the config (e.g. while reloading)
//! never triggers another reload.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sshconf::WatchDir;

/// Quiet period that ends a burst of events.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

pub struct ConfigWatcher {
    watcher: RecommendedWatcher,
    /// Currently watched paths and whether recursively.
    watched: BTreeMap<PathBuf, bool>,
    /// Directories that matter, as given and canonicalized.
    targets: Arc<Mutex<Vec<PathBuf>>>,
}

impl ConfigWatcher {
    /// `on_change` runs on a background thread, once per burst of changes.
    pub fn new(on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(tx)?;
        let targets = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&targets);
        thread::Builder::new()
            .name("sshelf-config-watch".into())
            .spawn(move || debounce(rx, shared, on_change))
            .map_err(notify::Error::io)?;
        Ok(Self {
            watcher,
            watched: BTreeMap::new(),
            targets,
        })
    }

    /// Watches exactly `dirs` from now on; returns problems as messages.
    pub fn set_dirs(&mut self, dirs: &[WatchDir]) -> Vec<String> {
        let mut wanted: BTreeMap<PathBuf, bool> = BTreeMap::new();
        let mut targets = Vec::new();
        for dir in dirs {
            targets.push(dir.path.clone());
            let Some(existing) = nearest_existing(&dir.path) else {
                continue;
            };
            // Events may report canonical paths (e.g. `/private/var` on macOS),
            // also for directories that do not exist yet.
            if let (Ok(canonical), Ok(rest)) = (
                fs::canonicalize(&existing),
                dir.path.strip_prefix(&existing),
            ) {
                targets.push(canonical.join(rest));
            }
            let recursive = dir.recursive && existing == dir.path;
            *wanted.entry(existing).or_insert(false) |= recursive;
        }
        *self.targets.lock().unwrap() = targets;

        let mut errors = Vec::new();
        let stale: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|(path, recursive)| wanted.get(*path) != Some(recursive))
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            // The directory may already be gone; nothing to report then.
            let _ = self.watcher.unwatch(&path);
            self.watched.remove(&path);
        }
        for (path, recursive) in wanted {
            if self.watched.contains_key(&path) {
                continue;
            }
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match self.watcher.watch(&path, mode) {
                Ok(()) => {
                    self.watched.insert(path, recursive);
                }
                Err(e) => errors.push(format!("cannot watch {}: {e}", path.display())),
            }
        }
        errors
    }
}

fn nearest_existing(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.is_dir())
        .map(Path::to_path_buf)
}

fn debounce(
    rx: Receiver<notify::Result<Event>>,
    targets: Arc<Mutex<Vec<PathBuf>>>,
    on_change: impl Fn(),
) {
    while let Ok(event) = rx.recv() {
        if !is_relevant(&event, &targets.lock().unwrap()) {
            continue;
        }
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        on_change();
    }
}

/// Watcher errors (e.g. overflowed event queues) count as changes.
fn is_relevant(event: &notify::Result<Event>, targets: &[PathBuf]) -> bool {
    let Ok(event) = event else {
        return true;
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| {
        targets
            .iter()
            .any(|target| path.starts_with(target) || target.starts_with(path))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);
    /// Time for the OS watcher to start delivering events.
    const SETTLE: Duration = Duration::from_millis(300);

    fn watcher() -> (ConfigWatcher, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let watcher = ConfigWatcher::new(move || {
            let _ = tx.send(());
        })
        .unwrap();
        (watcher, rx)
    }

    fn dir(path: PathBuf, recursive: bool) -> WatchDir {
        WatchDir { path, recursive }
    }

    /// Lets the OS watcher start, then drops what it reports from before:
    /// macOS (FSEvents) can deliver the setup's own writes (even the temp
    /// dir's creation) after the watch began.
    fn settle(rx: &Receiver<()>) {
        thread::sleep(SETTLE);
        while rx.recv_timeout(DEBOUNCE * 3).is_ok() {}
    }

    fn expect_change(rx: &Receiver<()>, what: &str) {
        rx.recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("no change reported after {what}"));
        // Swallow follow-up bursts so the next expectation starts clean.
        while rx.recv_timeout(DEBOUNCE * 3).is_ok() {}
    }

    fn expect_quiet(rx: &Receiver<()>, what: &str) {
        assert!(
            rx.recv_timeout(Duration::from_millis(800)).is_err(),
            "unexpected change after {what}"
        );
    }

    #[test]
    fn modified_and_new_files_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = tmp.path().join("ssh");
        fs::create_dir_all(ssh.join("conf.d")).unwrap();
        fs::write(ssh.join("config"), "Host a\n").unwrap();
        let (mut watcher, rx) = watcher();
        let errors = watcher.set_dirs(&[dir(ssh.clone(), false), dir(ssh.join("conf.d"), false)]);
        assert!(errors.is_empty(), "{errors:?}");
        settle(&rx);

        fs::write(ssh.join("config"), "Host a b\n").unwrap();
        expect_change(&rx, "modifying config");
        fs::write(ssh.join("conf.d").join("new.conf"), "Host n\n").unwrap();
        expect_change(&rx, "adding an included file");
    }

    #[test]
    fn reading_files_is_not_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("config"), "Host a\n").unwrap();
        let (mut watcher, rx) = watcher();
        watcher.set_dirs(&[dir(tmp.path().to_path_buf(), false)]);
        settle(&rx);
        for _ in 0..3 {
            fs::read_to_string(tmp.path().join("config")).unwrap();
        }
        expect_quiet(&rx, "reading the config");
    }

    #[test]
    fn missing_directory_is_watched_through_its_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let target = home.join("ssh").join("conf.d");
        let (mut watcher, rx) = watcher();
        watcher.set_dirs(&[dir(target.clone(), false)]);
        settle(&rx);

        fs::write(home.join("unrelated.txt"), "x").unwrap();
        expect_quiet(&rx, "an unrelated file in the ancestor");

        fs::create_dir(home.join("ssh")).unwrap();
        expect_change(&rx, "creating the parent of the missing dir");
        // The core reloads and re-applies the dirs; the parent is now watched.
        watcher.set_dirs(&[dir(target.clone(), false)]);
        settle(&rx);
        fs::create_dir(&target).unwrap();
        expect_change(&rx, "creating the missing dir");
    }

    #[test]
    fn recursive_watch_sees_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let hosts = tmp.path().join("hosts");
        fs::create_dir_all(hosts.join("team")).unwrap();
        let (mut watcher, rx) = watcher();
        watcher.set_dirs(&[dir(hosts.clone(), true)]);
        settle(&rx);
        fs::write(hosts.join("team").join("x.conf"), "Host x\n").unwrap();
        expect_change(&rx, "a file in a nested dir");
    }

    #[cfg(unix)]
    #[test]
    fn missing_directory_behind_a_symlink_is_matched_canonically() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (mut watcher, rx) = watcher();
        watcher.set_dirs(&[dir(link.join("ssh"), false)]);
        let targets = watcher.targets.lock().unwrap().clone();
        assert!(
            targets.contains(&fs::canonicalize(&real).unwrap().join("ssh")),
            "{targets:?}"
        );
        settle(&rx);
        fs::create_dir(real.join("ssh")).unwrap();
        expect_change(&rx, "creating the dir through its real path");
    }

    #[test]
    fn relevance_filter() {
        let targets = vec![PathBuf::from("/h/.ssh"), PathBuf::from("/h/.ssh/conf.d")];
        let event = |kind, path: &str| {
            Ok(Event {
                kind,
                paths: vec![PathBuf::from(path)],
                attrs: Default::default(),
            })
        };
        let modify = EventKind::Modify(notify::event::ModifyKind::Any);
        let access = EventKind::Access(notify::event::AccessKind::Any);
        assert!(is_relevant(&event(modify, "/h/.ssh/config"), &targets));
        assert!(is_relevant(&event(modify, "/h"), &targets));
        assert!(!is_relevant(&event(modify, "/h/Documents/x"), &targets));
        assert!(!is_relevant(&event(access, "/h/.ssh/config"), &targets));
        assert!(is_relevant(
            &Err(notify::Error::generic("overflow")),
            &targets
        ));
    }
}
