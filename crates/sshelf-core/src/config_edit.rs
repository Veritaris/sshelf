//! Config editing (SPEC.md §4 "Writing", §5): an [`Edit`] of one file is
//! turned into a candidate, validated with `ssh -G -F <temp file>` and
//! written by [`ConfigWriter`] — the only writer for ssh config files.

use std::collections::HashSet;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sshconf::diff::{DiffOp, diff};
use sshconf::{ConfigFile, LineKind};

use crate::process;
use crate::ssh::{RESOLVE_TIMEOUT, Tool};
use crate::storage::replace_file;

/// What an edit does to its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Replace the lines of one block (in the base file) with `text`.
    Replace { lines: Range<usize>, text: String },
    /// Add a new host block where SPEC.md §4 puts it.
    Insert { text: String },
}

/// One edit of one config file, as sent by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Chosen by the UI to match replies to its editor.
    pub id: u64,
    pub file: PathBuf,
    /// The file's bytes when editing started (empty for a file that does
    /// not exist yet); a save is refused if the file no longer matches.
    pub base: Vec<u8>,
    pub change: Change,
    /// The host to validate with `ssh -G`.
    pub alias: String,
}

impl Edit {
    /// The edited file and the lines of the edited block in it.
    pub fn candidate(&self) -> Result<(ConfigFile, Range<usize>), String> {
        let base = ConfigFile::parse(&self.file, &self.base).map_err(|e| e.to_string())?;
        Ok(match &self.change {
            Change::Replace { lines, text } => {
                if lines.end > base.lines.len() || lines.start > lines.end {
                    return Err("the edited block is no longer in the file".into());
                }
                let edited = base.replace_lines(lines.clone(), text);
                let len = text.split('\n').count() * usize::from(!text.is_empty());
                (edited, lines.start..lines.start + len)
            }
            Change::Insert { text } => base.insert_block(text),
        })
    }
}

/// A reason an edit cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// 0-based line in the edited block text; `None` for problems elsewhere.
    pub line: Option<usize>,
    pub message: String,
}

/// Problems the edit introduces: invalid lines in the edited block, and
/// errors `ssh -G -F` reports for the candidate file that it did not already
/// report for the file as it was (old problems never block a save).
pub fn validate(ssh: &Tool, edit: &Edit) -> Result<Vec<Problem>, String> {
    let (candidate, range) = edit.candidate()?;
    let mut problems: Vec<Problem> = candidate.lines[range.clone()]
        .iter()
        .enumerate()
        .filter_map(|(i, line)| match &line.kind {
            LineKind::Invalid(invalid) => Some(Problem {
                line: Some(i),
                message: invalid.reason.clone(),
            }),
            _ => None,
        })
        .collect();

    let found = check(ssh, &candidate.to_bytes(), &edit.alias)?;
    if found.is_empty() {
        return Ok(problems);
    }
    let before = if edit.base.is_empty() {
        Vec::new()
    } else {
        check(ssh, &edit.base, &edit.alias)?
    };
    // Candidate line → line of the base file it was kept from.
    let base = ConfigFile::parse(&edit.file, &edit.base).map_err(|e| e.to_string())?;
    let raw = |f: &ConfigFile| f.lines.iter().map(|l| l.raw.clone()).collect::<Vec<_>>();
    let mut kept = vec![None; candidate.lines.len()];
    for op in diff(&raw(&base), &raw(&candidate)) {
        if let DiffOp::Same { old, new } = op {
            kept[new] = Some(old);
        }
    }
    for (line, message) in found {
        let old = line.map(|n| kept.get(n - 1).copied().flatten().map(|o| o + 1));
        let existed = before.iter().any(|(l, m)| {
            *m == message
                && match old {
                    Some(Some(o)) => *l == Some(o),
                    Some(None) => false,
                    None => l.is_none(),
                }
        });
        if existed {
            continue;
        }
        let problem = match line {
            Some(n) if range.contains(&(n - 1)) => Problem {
                line: Some(n - 1 - range.start),
                message,
            },
            Some(n) => Problem {
                line: None,
                message: format!("line {n} of the file: {message}"),
            },
            None => Problem {
                line: None,
                message,
            },
        };
        if !problems.contains(&problem) {
            problems.push(problem);
        }
    }
    Ok(problems)
}

/// `ssh -G -F <temp copy of bytes> -- <alias>`: the errors it reports, as
/// (1-based line, message). Nothing is connected to.
fn check(ssh: &Tool, bytes: &[u8], alias: &str) -> Result<Vec<(Option<usize>, String)>, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("creating a temp dir: {e}"))?;
    let path = dir.path().join("config");
    fs::write(&path, bytes).map_err(|e| format!("writing a temp file: {e}"))?;
    let mut cmd = ssh.command();
    cmd.arg("-G").arg("-F").arg(&path).args(["--", alias]);
    let out = process::run(cmd, RESOLVE_TIMEOUT).map_err(|e| e.to_string())?;
    if out.code == Some(0) {
        return Ok(Vec::new());
    }
    let mut errors = parse_errors(&out.stderr, &path.display().to_string());
    if errors.is_empty() {
        let first = out.stderr.lines().map(str::trim).find(|l| !l.is_empty());
        errors.push((
            None,
            first
                .map(str::to_string)
                .unwrap_or_else(|| format!("`ssh -G` failed with exit code {:?}", out.code)),
        ));
    }
    Ok(errors)
}

/// Parses ssh's config errors, e.g. `<file> line 3: Bad port 'abc'.` and
/// `<file>: line 4: Bad configuration option: bogus` (OpenSSH 9.5p1). The
/// closing `terminating, N bad configuration options` line is dropped.
pub fn parse_errors(stderr: &str, file: &str) -> Vec<(Option<usize>, String)> {
    let mut errors = Vec::new();
    for raw in stderr.lines().map(str::trim) {
        if raw.is_empty() || raw.contains("terminating, ") {
            continue;
        }
        let rest = raw.strip_prefix(file).unwrap_or(raw);
        let parsed = rest.find("line ").and_then(|at| {
            let after = &rest[at + 5..];
            let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let message = after[digits..].strip_prefix(':')?;
            let line = after[..digits].parse::<usize>().ok().filter(|&n| n > 0)?;
            Some((Some(line), message.trim().to_string()))
        });
        errors.push(
            parsed.unwrap_or_else(|| (None, rest.trim_start_matches(':').trim().to_string())),
        );
    }
    errors
}

/// Why sshelf will not write `path`, if it will not. Files that are not
/// UTF-8 never load, so they never get here.
pub fn read_only_reason(path: &Path) -> Option<String> {
    match fs::metadata(path) {
        Ok(meta) if meta.permissions().readonly() => Some("the file is read-only".into()),
        Ok(meta) if !meta.is_file() => Some("not a regular file".into()),
        _ => None,
    }
}

/// The only writer for ssh config files: atomic replaces, and a copy of each
/// file in the app's `backups` dir before its first change in a session.
/// Backups are not kept next to the file, where an `Include` glob could pick
/// them up as config.
#[derive(Debug)]
pub struct ConfigWriter {
    backup_dir: PathBuf,
    backed_up: HashSet<PathBuf>,
}

impl ConfigWriter {
    pub fn new(backup_dir: PathBuf) -> Self {
        Self {
            backup_dir,
            backed_up: HashSet::new(),
        }
    }

    /// Replaces `path` with `bytes`. A symlink is written through to its
    /// target; on Unix the file keeps its permissions (a new one gets 600).
    pub fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let target = match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)
                .map_err(|e| format!("following the link {}: {e}", path.display()))?,
            _ => path.to_path_buf(),
        };
        if let Some(reason) = read_only_reason(&target) {
            return Err(format!("{}: {reason}", target.display()));
        }
        self.backup_once(&target)?;
        let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
        replace_file(&target, bytes).map_err(|e| format!("writing {}: {e}", target.display()))?;
        restore_permissions(&target, permissions)
            .map_err(|e| format!("setting permissions of {}: {e}", target.display()))
    }

    fn backup_once(&mut self, file: &Path) -> Result<(), String> {
        if !file.is_file() || self.backed_up.contains(file) {
            return Ok(());
        }
        let stamp = humantime::format_rfc3339_seconds(SystemTime::now())
            .to_string()
            .replace(':', "");
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config".into());
        fs::create_dir_all(&self.backup_dir)
            .map_err(|e| format!("creating {}: {e}", self.backup_dir.display()))?;
        let backup = (0..)
            .map(|n| match n {
                0 => self.backup_dir.join(format!("{name}.{stamp}.bak")),
                n => self.backup_dir.join(format!("{name}.{stamp}-{n}.bak")),
            })
            .find(|p| !p.exists())
            .expect("an unused backup name");
        fs::copy(file, &backup).map_err(|e| format!("backing up {}: {e}", file.display()))?;
        self.backed_up.insert(file.to_path_buf());
        Ok(())
    }
}

#[cfg(unix)]
fn restore_permissions(path: &Path, old: Option<fs::Permissions>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = old.unwrap_or_else(|| fs::Permissions::from_mode(0o600));
    fs::set_permissions(path, permissions)
}

/// Windows: the new file inherits the directory's ACL, like one an editor
/// saves; a read-only attribute was refused before writing.
#[cfg(not(unix))]
fn restore_permissions(_path: &Path, _old: Option<fs::Permissions>) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_error_lines_are_parsed() {
        let stderr = "/tmp/x/config line 3: Bad port 'abc'.\r\n\
                      /tmp/x/config: line 4: Bad configuration option: bogus\n\
                      /tmp/x/config line 2: invalid quotes\n\
                      /tmp/x/config: terminating, 3 bad configuration options\n\
                      something else\n";
        assert_eq!(
            parse_errors(stderr, "/tmp/x/config"),
            [
                (Some(3), "Bad port 'abc'.".to_string()),
                (Some(4), "Bad configuration option: bogus".to_string()),
                (Some(2), "invalid quotes".to_string()),
                (None, "something else".to_string()),
            ]
        );
        // The file path itself may contain "line ".
        assert_eq!(
            parse_errors("/home/a line b/config line 7: x\n", "/home/a line b/config"),
            [(Some(7), "x".to_string())]
        );
    }

    #[test]
    fn candidate_replaces_or_inserts() {
        let edit = |change| Edit {
            id: 1,
            file: "config".into(),
            base: b"Host a\n  Port 1\nHost *\n  User u\n".to_vec(),
            change,
            alias: "a".into(),
        };
        let (file, range) = edit(Change::Replace {
            lines: 0..2,
            text: "Host a\n  Port 2\n  User me".into(),
        })
        .candidate()
        .unwrap();
        assert_eq!(
            file.to_bytes(),
            b"Host a\n  Port 2\n  User me\nHost *\n  User u\n"
        );
        assert_eq!(range, 0..3);

        let (file, range) = edit(Change::Insert {
            text: "Host n\n  HostName n".into(),
        })
        .candidate()
        .unwrap();
        assert_eq!(file.lines[range.start].raw, "Host n");
        assert_eq!(file.lines[range.end].raw, "");

        let stale = edit(Change::Replace {
            lines: 3..9,
            text: String::new(),
        });
        assert!(stale.candidate().is_err());
    }

    #[test]
    fn writer_backs_up_once_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        fs::write(&file, "Host a\n").unwrap();
        let backups = dir.path().join("app").join("backups");
        let mut writer = ConfigWriter::new(backups.clone());
        writer.write(&file, b"Host b\n").unwrap();
        writer.write(&file, b"Host c\n").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"Host c\n");
        let saved: Vec<_> = fs::read_dir(&backups)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(saved.len(), 1);
        assert_eq!(fs::read(&saved[0]).unwrap(), b"Host a\n");
        // No stray files next to the config.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn writer_creates_a_missing_file_without_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".ssh").join("config");
        let mut writer = ConfigWriter::new(dir.path().join("backups"));
        writer.write(&file, b"Host n\n").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"Host n\n");
        assert!(!dir.path().join("backups").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn read_only_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        fs::write(&file, "Host a\n").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions.clone()).unwrap();
        assert!(read_only_reason(&file).is_some());
        let mut writer = ConfigWriter::new(dir.path().join("backups"));
        assert!(writer.write(&file, b"Host b\n").is_err());
        assert_eq!(fs::read(&file).unwrap(), b"Host a\n");
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&file, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_is_written_through_and_keeps_its_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles-config");
        fs::write(&real, "Host a\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("config");
        symlink(&real, &link).unwrap();
        let mut writer = ConfigWriter::new(dir.path().join("backups"));
        writer.write(&link, b"Host b\n").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"Host b\n");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}
