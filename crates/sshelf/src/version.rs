//! This build's version and commit, embedded by `build.rs` (SPEC.md §10
//! *Update check*): a release tag or `dev`, a short commit or `unknown`.

/// "0.4.0", or "dev" outside a release build.
pub const VERSION: &str = env!("SSHELF_BUILD_VERSION");
/// "a1b2c3d", or "unknown" without git.
pub const COMMIT: &str = env!("SSHELF_BUILD_COMMIT");

/// "sshelf 0.4.0 (a1b2c3d)": `--version`'s line, and what the Settings chip
/// copies.
pub fn line() -> String {
    format!("sshelf {VERSION} ({COMMIT})")
}

/// "sshelf 0.4.0 · a1b2c3d": the chip next to the Settings title.
pub fn chip() -> String {
    format!("sshelf {VERSION} · {COMMIT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_version_and_commit() {
        assert!(!VERSION.is_empty() && !COMMIT.is_empty());
        assert_eq!(line(), format!("sshelf {VERSION} ({COMMIT})"));
        assert_eq!(chip(), format!("sshelf {VERSION} · {COMMIT}"));
    }
}
