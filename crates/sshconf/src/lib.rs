//! Lossless `ssh_config` parser and editor.
//!
//! Parses the user's ssh config (and `Include`d files) into a line-oriented
//! concrete syntax tree and writes it back byte-for-byte, so unchanged lines —
//! comments, whitespace, CRLF, ordering, unknown directives — survive edits.
//! See SPEC.md §4.

mod error;
pub mod lexer;

pub use error::{Error, Result};
