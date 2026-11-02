//! Lossless `ssh_config` parser and editor.
//!
//! Parses the user's ssh config (and `Include`d files) into a line-oriented
//! concrete syntax tree and writes it back byte-for-byte, so unchanged lines —
//! comments, whitespace, CRLF, ordering, unknown directives — survive edits.
//! See SPEC.md §4.

mod error;
mod file;
mod hosts;
mod include;
pub mod lexer;
mod line;

pub use error::{Error, Result};
pub use file::{Block, ConfigFile};
pub use hosts::{HostEntry, HostList, PatternEntry, PatternKind};
pub use include::{
    ConfigSet, Diagnostic, FileId, HeaderRef, IncludeTarget, LineRef, MAX_INCLUDE_DEPTH,
    ResolveOptions, Severity, WatchDir,
};
pub use line::{
    Directive, Eol, HostHeader, HostPattern, Include, Invalid, Line, LineKind, MatchHeader,
};
