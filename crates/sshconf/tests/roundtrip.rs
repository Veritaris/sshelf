//! Round-trip corpus: parse → write must reproduce every input byte-for-byte.
//!
//! Corpus files cover syntax; byte-level details (CRLF, trailing whitespace,
//! BOM, missing final newline) are applied to every corpus file as variants and
//! spelled out with explicit escapes in [`BYTE_CASES`].

use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use sshconf::{ConfigFile, Error, LineKind};

/// Inputs whose exact bytes matter; kept in code so the escapes stay visible.
const BYTE_CASES: &[(&str, &[u8])] = &[
    (
        "trailing spaces and tabs",
        b"Host t   \n    HostName t.example.com\t\n    User eve  \t  \n    \n\t\nHost u \n",
    ),
    ("crlf", b"Host a\r\n    Port 22\r\n"),
    ("mixed endings", b"Host a\r\n    Port 22\n    User x\r\n"),
    ("no final newline", b"Host a\n    Port 22"),
    ("crlf without final newline", b"Host a\r\n    Port 22"),
    ("bom", b"\xEF\xBB\xBFHost a\r\n"),
    ("bom only", b"\xEF\xBB\xBF"),
    ("form feed at end of line", b"Host a\x0c\n"),
    ("lone carriage returns", b"Host a\rb\n\r\n\r"),
    ("blank lines only", b"\n\n\r\n  \n"),
    ("comment with trailing whitespace", b"# note   \r\n"),
];

fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("reading corpus dir")
        .map(|e| e.expect("corpus dir entry").path())
        .collect();
    paths.sort();
    assert!(paths.len() >= 10, "corpus looks incomplete: {paths:?}");
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = fs::read(&p).expect("reading corpus file");
            (name, bytes)
        })
        .collect()
}

fn assert_round_trip(name: &str, bytes: &[u8]) {
    let file = ConfigFile::parse(name, bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    let written = file.to_bytes();
    assert!(
        written == bytes,
        "{name}: round-trip mismatch\n input: {:?}\noutput: {:?}",
        String::from_utf8_lossy(bytes),
        String::from_utf8_lossy(&written),
    );
}

/// Byte-level variants of a corpus file.
fn variants(bytes: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    let text = std::str::from_utf8(bytes).expect("corpus is UTF-8");
    let mixed: String = text
        .split_inclusive('\n')
        .enumerate()
        .map(|(i, l)| {
            if i % 2 == 1 {
                l.replace('\n', "\r\n")
            } else {
                l.to_string()
            }
        })
        .collect();
    [
        ("crlf", text.replace('\n', "\r\n")),
        ("mixed", mixed),
        (
            "no final newline",
            text.strip_suffix('\n').unwrap_or(text).to_string(),
        ),
        ("bom", format!("\u{feff}{text}")),
        ("trailing whitespace", text.replace('\n', " \t \n")),
    ]
    .into_iter()
    .map(|(name, s)| (name, s.into_bytes()))
    .collect()
}

#[test]
fn corpus_files_round_trip() {
    for (name, bytes) in corpus() {
        assert_round_trip(&name, &bytes);
    }
}

#[test]
fn corpus_variants_round_trip() {
    for (name, bytes) in corpus() {
        for (variant, bytes) in variants(&bytes) {
            assert_round_trip(&format!("{name} ({variant})"), &bytes);
        }
    }
}

#[test]
fn byte_cases_round_trip() {
    for (name, bytes) in BYTE_CASES {
        assert_round_trip(name, bytes);
    }
}

#[test]
fn trailing_whitespace_is_not_part_of_values() {
    let file = ConfigFile::parse("t", BYTE_CASES[0].1).unwrap();
    let LineKind::Directive(user) = &file.lines[2].kind else {
        panic!("expected a directive: {:?}", file.lines[2]);
    };
    assert_eq!(user.args, ["eve"]);
}

/// Guards against misparsing: valid corpus files must not produce `Invalid`
/// lines, and the invalid file must produce exactly the expected ones.
#[test]
fn invalid_lines_only_where_expected() {
    for (name, bytes) in corpus() {
        let file = ConfigFile::parse(&name, &bytes).unwrap();
        let invalid: Vec<_> = file
            .lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Invalid(_)))
            .map(|l| l.number)
            .collect();
        if name.contains("invalid") {
            assert_eq!(invalid, [2, 3, 4, 5, 6, 7, 8], "{name}");
        } else {
            assert!(invalid.is_empty(), "{name}: invalid lines {invalid:?}");
        }
    }
}

proptest! {
    #[test]
    fn any_utf8_text_round_trips(text in any::<String>()) {
        let file = ConfigFile::parse("prop", text.as_bytes()).unwrap();
        prop_assert_eq!(file.to_bytes(), text.as_bytes());
    }

    #[test]
    fn config_like_lines_round_trip(
        lines in prop::collection::vec(
            (
                r#"[ \t]{0,2}(Host|Match|Include|HostName|User|ProxyCommand|#)?[ \t=]{0,2}[ -~\t]{0,24}"#,
                prop_oneof![Just("\n"), Just("\r\n"), Just("")],
            ),
            0..30,
        )
    ) {
        let text: String = lines.iter().map(|(l, eol)| format!("{l}{eol}")).collect();
        let file = ConfigFile::parse("prop", text.as_bytes()).unwrap();
        prop_assert_eq!(file.to_bytes(), text.as_bytes());
    }

    #[test]
    fn arbitrary_bytes_round_trip_or_fail_cleanly(bytes in any::<Vec<u8>>()) {
        match ConfigFile::parse("prop", &bytes) {
            Ok(file) => prop_assert_eq!(file.to_bytes(), bytes),
            Err(Error::NotUtf8 { .. }) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
