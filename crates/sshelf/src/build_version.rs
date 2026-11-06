//! The version string's rules (SPEC.md §10 *Update check*), used by
//! `build.rs` (which embeds the result) and by its tests: the release tag
//! when the build is exactly at one with a clean tree, else `dev`; the short
//! commit, else `unknown`; `SSHELF_VERSION` / `SSHELF_COMMIT` win.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What a build calls itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: String,
    pub commit: String,
}

/// The build of the sources in `dir`; `version` / `commit` are the
/// environment's overrides (empty counts as unset).
pub fn describe(dir: &Path, version: Option<String>, commit: Option<String>) -> Build {
    let set = |value: Option<String>| value.filter(|v| !v.trim().is_empty());
    Build {
        version: set(version)
            .or_else(|| release_tag(dir))
            .unwrap_or_else(|| "dev".into()),
        commit: set(commit)
            .or_else(|| git(dir, &["rev-parse", "--short=7", "HEAD"]).filter(|c| !c.is_empty()))
            .unwrap_or_else(|| "unknown".into()),
    }
}

/// The `X.Y.Z` tag on `HEAD`, if the tracked files are unchanged.
fn release_tag(dir: &Path) -> Option<String> {
    let tag = git(
        dir,
        &[
            "describe",
            "--tags",
            "--exact-match",
            "--match",
            "[0-9]*.[0-9]*.[0-9]*",
        ],
    )?;
    let changed = git(dir, &["status", "--porcelain", "--untracked-files=no"])?;
    (changed.is_empty() && !tag.is_empty()).then_some(tag)
}

/// The files whose change moves `HEAD` or a tag: `HEAD` (of this worktree)
/// and the shared `refs` and `packed-refs`, those that exist.
pub fn watched(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(git_dir) = git(dir, &["rev-parse", "--absolute-git-dir"]) {
        paths.push(Path::new(&git_dir).join("HEAD"));
    }
    if let Some(common) = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        paths.push(Path::new(&common).join("refs"));
        paths.push(Path::new(&common).join("packed-refs"));
    }
    paths.retain(|p| p.exists());
    paths
}

/// `git -C dir args…`'s trimmed output; `None` without git or on failure.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// `git args…` in `dir`, isolated from the user's and the system's git
    /// configuration (no signing, a made-up author).
    fn run(dir: &Path, args: &[&str]) {
        let empty = dir.parent().unwrap().join("gitconfig");
        fs::write(&empty, "").unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &empty)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// A repository with one commit of `file.txt`, in its own temp folder.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("repo");
        fs::create_dir(&dir).unwrap();
        run(&dir, &["init", "-q"]);
        fs::write(dir.join("file.txt"), "one\n").unwrap();
        run(&dir, &["add", "file.txt"]);
        run(&dir, &["commit", "-q", "-m", "one"]);
        (root, dir)
    }

    #[test]
    fn without_git_it_is_dev_and_unknown() {
        let root = tempfile::tempdir().unwrap();
        let build = describe(root.path(), None, None);
        assert_eq!(build.version, "dev");
        assert_eq!(build.commit, "unknown");
        assert!(watched(root.path()).is_empty());
    }

    #[test]
    fn an_untagged_commit_is_dev_with_its_hash() {
        let (_root, dir) = repo();
        let build = describe(&dir, None, None);
        assert_eq!(build.version, "dev");
        assert_eq!(build.commit.len(), 7, "{build:?}");
        assert!(build.commit.chars().all(|c| c.is_ascii_hexdigit()));
        let watched = watched(&dir);
        assert!(watched.iter().any(|p| p.ends_with("HEAD")), "{watched:?}");
        assert!(watched.iter().any(|p| p.ends_with("refs")), "{watched:?}");
    }

    #[test]
    fn a_release_tag_names_the_version_while_the_tree_is_clean() {
        let (_root, dir) = repo();
        run(&dir, &["tag", "0.4.0"]);
        assert_eq!(describe(&dir, None, None).version, "0.4.0");
        // Untracked files don't count; a changed tracked one does.
        fs::write(dir.join("new.txt"), "x").unwrap();
        assert_eq!(describe(&dir, None, None).version, "0.4.0");
        fs::write(dir.join("file.txt"), "two\n").unwrap();
        assert_eq!(describe(&dir, None, None).version, "dev");
    }

    #[test]
    fn other_tags_and_later_commits_are_dev() {
        let (_root, dir) = repo();
        run(&dir, &["tag", "v0.4.0"]);
        run(&dir, &["tag", "nightly"]);
        assert_eq!(describe(&dir, None, None).version, "dev");
        run(&dir, &["tag", "0.4.0"]);
        fs::write(dir.join("file.txt"), "two\n").unwrap();
        run(&dir, &["commit", "-q", "-am", "two"]);
        assert_eq!(describe(&dir, None, None).version, "dev");
    }

    #[test]
    fn the_environment_wins() {
        let (_root, dir) = repo();
        run(&dir, &["tag", "0.4.0"]);
        let build = describe(&dir, Some("0.5.1".into()), Some("abcdef0".into()));
        assert_eq!(
            build,
            Build {
                version: "0.5.1".into(),
                commit: "abcdef0".into()
            }
        );
        // Empty overrides are unset.
        let build = describe(&dir, Some(String::new()), Some(" ".into()));
        assert_eq!(build.version, "0.4.0");
        assert_ne!(build.commit, " ");
        // Without git too.
        let none = tempfile::tempdir().unwrap();
        let build = describe(none.path(), Some("0.5.1".into()), Some("abcdef0".into()));
        assert_eq!(
            (build.version.as_str(), build.commit.as_str()),
            ("0.5.1", "abcdef0")
        );
    }
}
