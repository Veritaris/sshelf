//! Minimal edits over the round-trip corpus: editing one block through the
//! form operations rewrites only the lines that changed, and everything else
//! stays byte-identical (CRLF variants included).

use std::fs;
use std::path::Path;

use sshconf::diff::{DiffOp, diff};
use sshconf::edit::{self, set};
use sshconf::{ConfigFile, LineKind};

fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .expect("reading corpus dir")
        .map(|e| e.expect("corpus dir entry").path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&p).expect("reading corpus file"))
        })
        .collect()
}

/// Each corpus file as is and with CRLF endings.
fn variants() -> Vec<(String, Vec<u8>)> {
    corpus()
        .into_iter()
        .flat_map(|(name, bytes)| {
            let text = String::from_utf8(bytes.clone()).unwrap();
            let crlf = text
                .replace("\r\n", "\n")
                .replace('\n', "\r\n")
                .into_bytes();
            [(name.clone(), bytes), (format!("{name} (crlf)"), crlf)]
        })
        .collect()
}

fn byte_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|&b| b == b'\n').collect()
}

/// Number of byte lines of `before` that are not kept in `after`.
fn changed_lines(before: &[u8], after: &[u8]) -> (usize, usize) {
    let (a, b) = (byte_lines(before), byte_lines(after));
    let ops = diff(&a, &b);
    let removed = ops
        .iter()
        .filter(|o| matches!(o, DiffOp::Removed { .. }))
        .count();
    let added = ops
        .iter()
        .filter(|o| matches!(o, DiffOp::Added { .. }))
        .count();
    (removed, added)
}

fn host_headers(file: &ConfigFile) -> Vec<usize> {
    file.lines
        .iter()
        .enumerate()
        .filter(|(_, l)| matches!(l.kind, LineKind::HostHeader(_)))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn unchanged_block_text_is_byte_identical() {
    for (name, bytes) in variants() {
        let file = ConfigFile::parse(&name, &bytes).unwrap();
        for block in file.blocks() {
            let text = file.text(block.lines.clone());
            let out = file.replace_lines(block.lines, &text).to_bytes();
            assert!(out == bytes, "{name}: block rewrite changed the file");
        }
    }
}

#[test]
fn setting_a_value_changes_exactly_one_line() {
    for (name, bytes) in variants() {
        let file = ConfigFile::parse(&name, &bytes).unwrap();
        for header in host_headers(&file) {
            let block = file.block_at(header).unwrap();
            let text = file.text(block.lines.clone());
            let had_port = !edit::values(&text, "Port").is_empty();
            let edited = set(&text, "Port", Some(&["40022".to_string()]));
            let out = file.replace_lines(block.lines.clone(), &edited).to_bytes();
            let (removed, added) = changed_lines(&bytes, &out);
            let expected = if had_port { (1, 1) } else { (0, 1) };
            assert_eq!(
                (removed, added),
                expected,
                "{name} line {}: {}",
                header + 1,
                String::from_utf8_lossy(&out)
            );
            let reparsed = ConfigFile::parse(&name, &out).unwrap();
            let block = reparsed.block_at(header).unwrap();
            assert_eq!(
                edit::values(&reparsed.text(block.lines), "port")[0],
                ["40022"],
                "{name}"
            );
        }
    }
}

#[test]
fn renaming_changes_only_the_header_line() {
    for (name, bytes) in variants() {
        let file = ConfigFile::parse(&name, &bytes).unwrap();
        for header in host_headers(&file) {
            let block = file.block_at(header).unwrap();
            let text = file.text(block.lines.clone());
            let Some(alias) = edit::header_aliases(&text).and_then(|a| a.into_iter().next()) else {
                continue;
            };
            let renamed = edit::rename_alias(&text, &alias, "renamed-host").unwrap();
            let out = file.replace_lines(block.lines, &renamed).to_bytes();
            assert_eq!(changed_lines(&bytes, &out), (1, 1), "{name}: {alias}");
        }
    }
}

#[test]
fn new_host_adds_lines_and_moves_nothing() {
    for (name, bytes) in variants() {
        let file = ConfigFile::parse(&name, &bytes).unwrap();
        let (edited, lines) = file.insert_block("Host brand-new\n    HostName n.example");
        let out = edited.to_bytes();
        let (removed, added) = changed_lines(&bytes, &out);
        // A file without a final newline gains one on its old last line.
        let no_final_newline = !bytes.is_empty() && !bytes.ends_with(b"\n");
        assert_eq!(removed, usize::from(no_final_newline), "{name}");
        assert!(
            (2..=4 + usize::from(no_final_newline)).contains(&added),
            "{name}"
        );
        assert_eq!(edited.lines[lines.start].raw, "Host brand-new", "{name}");
    }
}
