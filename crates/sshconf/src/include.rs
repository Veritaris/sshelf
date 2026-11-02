//! Loads a config file and everything it `Include`s, following ssh's rules:
//! paths are glob-expanded in sorted order, `~` is the home directory, relative
//! paths resolve against the ssh dir (`~/.ssh`, not the including file), a glob
//! matching nothing is skipped silently, and nesting is limited to 16 levels.
//! An `Include` inside a `Host`/`Match` block is conditional on that block.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use glob::{MatchOptions, Pattern};

use crate::error::Result;
use crate::file::ConfigFile;
use crate::line::{Line, LineKind};

/// ssh's `READCONF_MAX_DEPTH`.
pub const MAX_INCLUDE_DEPTH: usize = 16;

/// Index into [`ConfigSet::files`]; the main file is always `0`.
pub type FileId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveOptions {
    /// Expansion of `~`.
    pub home: PathBuf,
    /// Base for relative `Include` paths (normally `~/.ssh`).
    pub ssh_dir: PathBuf,
    pub max_depth: usize,
}

impl ResolveOptions {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            ssh_dir: home.join(".ssh"),
            home,
            max_depth: MAX_INCLUDE_DEPTH,
        }
    }
}

/// A line in one of the loaded files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineRef {
    pub file: FileId,
    /// Index into [`ConfigFile::lines`].
    pub line: usize,
}

/// A `Host`/`Match` header in ssh's evaluation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderRef {
    pub at: LineRef,
    /// Headers of the blocks whose `Include` led here, outermost first.
    /// Empty when the header is unconditionally reachable.
    pub conditions: Vec<LineRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    /// ssh itself would reject the config.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub at: LineRef,
    pub message: String,
}

/// The main config file and all files reachable through `Include`.
#[derive(Debug, Clone)]
pub struct ConfigSet {
    /// Each file once, in first-reached order; `files[0]` is the main file.
    pub files: Vec<ConfigFile>,
    pub headers: Vec<HeaderRef>,
    /// Every resolved `Include` argument, in evaluation order.
    pub includes: Vec<IncludeTarget>,
    pub diagnostics: Vec<Diagnostic>,
}

/// One resolved `Include` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeTarget {
    /// The `Include` line.
    pub at: LineRef,
    /// Literal directory the glob is relative to (home or ssh dir); empty when
    /// the argument is an absolute path.
    pub base: PathBuf,
    /// The glob as written, without a leading `~/`.
    pub pattern: String,
}

/// A directory to watch for changes that affect a [`ConfigSet`]. It may not
/// exist yet; the watcher then has to watch its nearest existing ancestor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WatchDir {
    pub path: PathBuf,
    /// A directory component of an `Include` glob has wildcards.
    pub recursive: bool,
}

impl ConfigSet {
    /// Reads `main` and resolves its includes. Only failing to read `main`
    /// itself is an error; problems with included files become diagnostics.
    pub fn load(main: &Path, opts: &ResolveOptions) -> Result<Self> {
        let file = ConfigFile::read(main)?;
        let mut loader = Loader {
            opts,
            set: ConfigSet {
                files: Vec::new(),
                headers: Vec::new(),
                includes: Vec::new(),
                diagnostics: Vec::new(),
            },
            ids: HashMap::new(),
            walked: HashSet::new(),
            stack: Vec::new(),
        };
        let id = loader.add(identity(main), file);
        loader.walk(id, 0, &[]);
        Ok(loader.set)
    }

    pub fn line(&self, at: LineRef) -> &Line {
        &self.files[at.file].lines[at.line]
    }

    /// Directories whose changes can affect this set: the parent of every
    /// loaded file and the deepest literal directory of every `Include` glob.
    /// Watching directories rather than files also catches newly added files
    /// and editors that save by renaming a temp file over the original.
    pub fn watch_dirs(&self) -> Vec<WatchDir> {
        let mut dirs: BTreeMap<PathBuf, bool> = BTreeMap::new();
        for file in &self.files {
            let parent = match file.path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            };
            dirs.entry(parent).or_insert(false);
        }
        for target in &self.includes {
            let (dir, recursive) = literal_dir(&target.base, &target.pattern);
            *dirs.entry(dir).or_insert(false) |= recursive;
        }
        dirs.into_iter()
            .map(|(path, recursive)| WatchDir { path, recursive })
            .collect()
    }

    /// `path:line` for messages.
    pub fn location(&self, at: LineRef) -> String {
        format!(
            "{}:{}",
            self.files[at.file].path.display(),
            self.line(at).number
        )
    }
}

struct Loader<'a> {
    opts: &'a ResolveOptions,
    set: ConfigSet,
    /// Canonical path → file, so a file included twice is parsed once.
    ids: HashMap<PathBuf, FileId>,
    /// Files whose own lines were already checked for diagnostics.
    walked: HashSet<FileId>,
    /// Canonical paths of the files currently being read (cycle detection).
    stack: Vec<PathBuf>,
}

impl Loader<'_> {
    fn add(&mut self, key: PathBuf, file: ConfigFile) -> FileId {
        let id = self.set.files.len();
        self.set.files.push(file);
        self.ids.insert(key, id);
        id
    }

    fn walk(&mut self, id: FileId, depth: usize, conditions: &[LineRef]) {
        let first_walk = self.walked.insert(id);
        self.stack.push(identity(&self.set.files[id].path));
        let mut header = None;
        for i in 0..self.set.files[id].lines.len() {
            let at = LineRef { file: id, line: i };
            match &self.set.files[id].lines[i].kind {
                LineKind::HostHeader(_) | LineKind::MatchHeader(_) => {
                    header = Some(at);
                    self.set.headers.push(HeaderRef {
                        at,
                        conditions: conditions.to_vec(),
                    });
                }
                LineKind::Include(include) => {
                    let paths = include.paths.clone();
                    let mut inner = conditions.to_vec();
                    inner.extend(header);
                    for path in &paths {
                        self.include(at, path, depth, &inner);
                    }
                }
                LineKind::Invalid(invalid) if first_walk => {
                    let message = invalid.reason.clone();
                    self.diag(Severity::Error, at, message);
                }
                _ => {}
            }
        }
        self.stack.pop();
    }

    fn include(&mut self, at: LineRef, raw: &str, depth: usize, conditions: &[LineRef]) {
        if raw.contains('%') || raw.contains("${") {
            self.diag(
                Severity::Warning,
                at,
                format!(
                    "`{raw}`: ssh does not expand `%` tokens or `${{VAR}}` here; used literally"
                ),
            );
        }
        let Some((base, glob)) = self.target(raw) else {
            self.diag(
                Severity::Warning,
                at,
                format!("`{raw}`: `~user` paths are not supported"),
            );
            return;
        };
        let pattern = glob_pattern(&base, &glob);
        self.set.includes.push(IncludeTarget {
            at,
            base,
            pattern: glob,
        });
        let options = MatchOptions {
            case_sensitive: !cfg!(windows),
            require_literal_separator: true,
            require_literal_leading_dot: true,
        };
        let entries = match glob::glob_with(&pattern, options) {
            Ok(entries) => entries,
            Err(e) => {
                self.diag(Severity::Warning, at, format!("`{raw}`: bad pattern: {e}"));
                return;
            }
        };

        let mut matched = false;
        for entry in entries {
            match entry {
                Ok(path) if path.is_dir() => {
                    self.diag(
                        Severity::Warning,
                        at,
                        format!("{} is a directory, skipped", path.display()),
                    );
                }
                Ok(path) => {
                    matched = true;
                    self.include_file(at, &path, depth + 1, conditions);
                }
                Err(e) => self.diag(Severity::Warning, at, format!("`{raw}`: {e}")),
            }
        }
        if !matched {
            self.diag(Severity::Info, at, format!("`{raw}` matched no files"));
        }
    }

    fn include_file(&mut self, at: LineRef, path: &Path, depth: usize, conditions: &[LineRef]) {
        let key = identity(path);
        if self.stack.contains(&key) {
            self.diag(
                Severity::Error,
                at,
                format!("include cycle: {} is already being read", path.display()),
            );
            return;
        }
        if depth > self.opts.max_depth {
            self.diag(
                Severity::Error,
                at,
                format!(
                    "too many nested includes (limit {}) at {}",
                    self.opts.max_depth,
                    path.display()
                ),
            );
            return;
        }
        let id = match self.ids.get(&key) {
            Some(&id) => id,
            None => match ConfigFile::read(path) {
                Ok(file) => self.add(key, file),
                Err(e) => {
                    self.diag(Severity::Error, at, e.to_string());
                    return;
                }
            },
        };
        self.walk(id, depth, conditions);
    }

    /// Splits an `Include` argument into its literal base directory and the
    /// glob written by the user; `None` for unsupported `~user` paths.
    fn target(&self, raw: &str) -> Option<(PathBuf, String)> {
        if raw == "~" {
            Some((self.opts.home.clone(), String::new()))
        } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            Some((self.opts.home.clone(), rest.to_string()))
        } else if raw.starts_with('~') {
            None
        } else if Path::new(raw).is_absolute() || Path::new(raw).has_root() {
            Some((PathBuf::new(), raw.to_string()))
        } else {
            Some((self.opts.ssh_dir.clone(), raw.to_string()))
        }
    }

    fn diag(&mut self, severity: Severity, at: LineRef, message: String) {
        self.set.diagnostics.push(Diagnostic {
            severity,
            at,
            message,
        });
    }
}

/// The glob for `base` + `glob`. The base is escaped so glob characters in e.g.
/// the home path are taken literally.
fn glob_pattern(base: &Path, glob: &str) -> String {
    if base.as_os_str().is_empty() {
        return glob.to_string();
    }
    let base = Pattern::escape(&base.to_string_lossy());
    if glob.is_empty() {
        base
    } else {
        format!("{base}/{glob}")
    }
}

/// Deepest directory of `base` + `glob` without wildcards, and whether a
/// directory component of `glob` has wildcards (so the watch must be recursive).
fn literal_dir(base: &Path, glob: &str) -> (PathBuf, bool) {
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let mut starts = Vec::new();
    let mut offset = 0;
    for part in glob.split(is_sep) {
        starts.push((offset, part));
        offset += part.len() + 1;
    }
    // The last component is the file name; only directories are watched.
    let last = starts.len() - 1;
    let wild = starts[..last]
        .iter()
        .position(|(_, part)| part.contains(['*', '?', '[']));
    let (end, _) = starts[wild.unwrap_or(last)];
    let prefix = &glob[..end];
    let dir = if base.as_os_str().is_empty() {
        PathBuf::from(prefix)
    } else {
        base.join(prefix)
    };
    (dir, wild.is_some())
}

/// Key for "same file": canonical path when it exists, else the path as given.
fn identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(base: &str, glob: &str) -> (PathBuf, bool) {
        literal_dir(Path::new(base), glob)
    }

    #[test]
    fn literal_dir_of_relative_globs() {
        let base = Path::new("ssh");
        assert_eq!(dir("ssh", "conf.d/*.conf"), (base.join("conf.d"), false));
        assert_eq!(dir("ssh", "other.conf"), (base.to_path_buf(), false));
        assert_eq!(dir("ssh", "*.conf"), (base.to_path_buf(), false));
        assert_eq!(dir("ssh", ""), (base.to_path_buf(), false));
        assert_eq!(dir("ssh", "a/b/c.conf"), (base.join("a/b"), false));
    }

    #[test]
    fn wildcard_directory_makes_watch_recursive() {
        let base = Path::new("ssh");
        assert_eq!(dir("ssh", "*/x.conf"), (base.to_path_buf(), true));
        assert_eq!(dir("ssh", "hosts/*/x/*.conf"), (base.join("hosts"), true));
        assert_eq!(dir("ssh", "h[0-9]/x.conf"), (base.to_path_buf(), true));
    }

    #[test]
    fn literal_dir_of_absolute_globs() {
        assert_eq!(
            dir("", "/etc/ssh/ssh_config.d/*.conf"),
            (PathBuf::from("/etc/ssh/ssh_config.d"), false)
        );
        assert_eq!(dir("", "/x.conf"), (PathBuf::from("/"), false));
    }

    #[cfg(windows)]
    #[test]
    fn literal_dir_of_windows_paths() {
        assert_eq!(
            dir("", r"C:\Users\me\.ssh\conf.d\*.conf"),
            (PathBuf::from(r"C:\Users\me\.ssh\conf.d"), false)
        );
        assert_eq!(dir("", "C:/x.conf"), (PathBuf::from(r"C:\"), false));
    }
}
