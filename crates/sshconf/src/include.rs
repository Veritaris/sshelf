//! Loads a config file and everything it `Include`s, following ssh's rules:
//! paths are glob-expanded in sorted order, `~` is the home directory, relative
//! paths resolve against the ssh dir (`~/.ssh`, not the including file), a glob
//! matching nothing is skipped silently, and nesting is limited to 16 levels.
//! An `Include` inside a `Host`/`Match` block is conditional on that block.

use std::collections::{HashMap, HashSet};
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
    pub diagnostics: Vec<Diagnostic>,
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
        let Some(pattern) = self.pattern(raw) else {
            self.diag(
                Severity::Warning,
                at,
                format!("`{raw}`: `~user` paths are not supported"),
            );
            return;
        };
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

    /// Glob pattern for an `Include` argument. The base directory is escaped so
    /// glob characters in e.g. the home path are taken literally.
    fn pattern(&self, raw: &str) -> Option<String> {
        let (base, rest) = if raw == "~" {
            (&self.opts.home, "")
        } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            (&self.opts.home, rest)
        } else if raw.starts_with('~') {
            return None;
        } else if Path::new(raw).is_absolute() || Path::new(raw).has_root() {
            return Some(raw.to_string());
        } else {
            (&self.opts.ssh_dir, raw)
        };
        let base = Pattern::escape(&base.to_string_lossy());
        Some(if rest.is_empty() {
            base
        } else {
            format!("{base}/{rest}")
        })
    }

    fn diag(&mut self, severity: Severity, at: LineRef, message: String) {
        self.set.diagnostics.push(Diagnostic {
            severity,
            at,
            message,
        });
    }
}

/// Key for "same file": canonical path when it exists, else the path as given.
fn identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
