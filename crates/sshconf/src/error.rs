//! Hard errors of the parser. Syntax problems are never errors: they become
//! `Invalid` lines plus diagnostics, so a broken config can still be shown and
//! written back unchanged.

use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}:{line}: not valid UTF-8", path.display())]
    NotUtf8 { path: PathBuf, line: usize },
}

pub type Result<T> = std::result::Result<T, Error>;
