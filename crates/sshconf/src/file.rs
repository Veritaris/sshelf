//! One config file as a lossless list of lines, grouped into blocks.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::line::{Eol, Line};

const BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    pub path: PathBuf,
    /// The file started with a UTF-8 byte order mark.
    pub bom: bool,
    pub lines: Vec<Line>,
}

/// A header line (`Host`/`Match`) and the lines up to the next header.
/// `header` is `None` for the lines before the first header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Index into [`ConfigFile::lines`].
    pub header: Option<usize>,
    /// Line indices of the block, including the header.
    pub lines: Range<usize>,
}

impl ConfigFile {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &bytes)
    }

    /// Splits `bytes` into lines on `\n`; a preceding `\r` is recorded as CRLF.
    pub fn parse(path: impl Into<PathBuf>, bytes: &[u8]) -> Result<Self> {
        let path = path.into();
        let (bom, mut rest) = match bytes.strip_prefix(BOM) {
            Some(body) => (true, body),
            None => (false, bytes),
        };

        let mut lines = Vec::new();
        while !rest.is_empty() {
            let (segment, eol, next) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => match rest[..i].strip_suffix(b"\r") {
                    Some(seg) => (seg, Eol::CrLf, &rest[i + 1..]),
                    None => (&rest[..i], Eol::Lf, &rest[i + 1..]),
                },
                None => (rest, Eol::None, &rest[rest.len()..]),
            };
            let number = lines.len() + 1;
            let raw = std::str::from_utf8(segment).map_err(|_| Error::NotUtf8 {
                path: path.clone(),
                line: number,
            })?;
            lines.push(Line::parse(number, raw.to_string(), eol));
            rest = next;
        }
        Ok(Self { path, bom, lines })
    }

    /// The file's exact bytes: BOM (if any), then every line's raw text and ending.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.bom {
            out.extend_from_slice(BOM);
        }
        for line in &self.lines {
            out.extend_from_slice(line.raw.as_bytes());
            out.extend_from_slice(line.eol.as_str().as_bytes());
        }
        out
    }

    pub fn blocks(&self) -> Vec<Block> {
        let mut blocks = Vec::new();
        let mut header = None;
        let mut start = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if line.kind.is_header() {
                if header.is_some() || i > start {
                    blocks.push(Block {
                        header,
                        lines: start..i,
                    });
                }
                header = Some(i);
                start = i;
            }
        }
        if header.is_some() || start < self.lines.len() {
            blocks.push(Block {
                header,
                lines: start..self.lines.len(),
            });
        }
        blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::LineKind;

    fn parse(text: &str) -> ConfigFile {
        ConfigFile::parse("test", text.as_bytes()).expect("utf-8 input")
    }

    fn eols(file: &ConfigFile) -> Vec<Eol> {
        file.lines.iter().map(|l| l.eol).collect()
    }

    #[test]
    fn empty_file_has_no_lines() {
        let f = parse("");
        assert!(f.lines.is_empty());
        assert!(f.blocks().is_empty());
        assert!(f.to_bytes().is_empty());
    }

    #[test]
    fn line_endings() {
        let f = parse("a\nb\r\nc");
        assert_eq!(eols(&f), [Eol::Lf, Eol::CrLf, Eol::None]);
        assert_eq!(f.lines[1].raw, "b");
        assert_eq!(f.lines[2].number, 3);

        let f = parse("\n\n");
        assert_eq!(eols(&f), [Eol::Lf, Eol::Lf]);
    }

    #[test]
    fn lone_carriage_return_stays_in_raw() {
        let f = parse("Host a\rb\nPort 22\r");
        assert_eq!(f.lines[0].raw, "Host a\rb");
        assert_eq!(f.lines[1].raw, "Port 22\r");
        assert_eq!(f.lines[1].eol, Eol::None);
        assert_eq!(f.to_bytes(), b"Host a\rb\nPort 22\r");
    }

    #[test]
    fn bom_is_preserved() {
        let bytes = b"\xEF\xBB\xBFHost a\n";
        let f = ConfigFile::parse("test", bytes).unwrap();
        assert!(f.bom);
        assert!(matches!(f.lines[0].kind, LineKind::HostHeader(_)));
        assert_eq!(f.to_bytes(), bytes);
    }

    #[test]
    fn invalid_utf8_reports_line() {
        let err = ConfigFile::parse("cfg", b"Host a\nUser \xFF\n").unwrap_err();
        assert!(matches!(err, Error::NotUtf8 { line: 2, .. }), "{err}");
    }

    #[test]
    fn blocks_group_headers_with_following_lines() {
        let f = parse("# top\nUser x\n\nHost a\n  Port 1\nMatch all\n  Port 2\nHost b\n");
        let blocks: Vec<_> = f
            .blocks()
            .into_iter()
            .map(|b| (b.header, b.lines))
            .collect();
        assert_eq!(
            blocks,
            [
                (None, 0..3),
                (Some(3), 3..5),
                (Some(5), 5..7),
                (Some(7), 7..8)
            ]
        );
    }

    #[test]
    fn file_starting_with_header_has_no_global_block() {
        let f = parse("Host a\nPort 1\n");
        let blocks = f.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].header, Some(0));
    }
}
