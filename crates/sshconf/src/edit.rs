//! Editing (SPEC.md §4 "Writing", §5): a block is edited as text (lines
//! joined with `\n`), shared by the structured form and the raw editor, and
//! applied back to its file so that every unchanged line keeps its exact bytes
//! and line ending.

use std::ops::Range;

use crate::diff::{DiffOp, diff};
use crate::file::{Block, ConfigFile};
use crate::lexer::{self, Layout};
use crate::line::{self, Eol, Line, LineKind};

/// Indentation of a directive added to a block that has none to copy.
const DEFAULT_INDENT: &str = "    ";

impl ConfigFile {
    /// The block whose header is line `header` (0-based).
    pub fn block_at(&self, header: usize) -> Option<Block> {
        self.blocks().into_iter().find(|b| b.header == Some(header))
    }

    /// The raw text of `lines`, joined with `\n` (line endings are restored
    /// by [`ConfigFile::replace_lines`]).
    pub fn text(&self, lines: Range<usize>) -> String {
        self.lines[lines]
            .iter()
            .map(|l| l.raw.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Replaces `lines` with `text`. Lines matching an old line keep its
    /// line ending, a changed line takes the ending of the line it replaces,
    /// added lines get the file's usual one. Empty `text` deletes the lines.
    /// Whether the file ends with a newline is kept.
    pub fn replace_lines(&self, lines: Range<usize>, text: &str) -> ConfigFile {
        let new = split(text);
        let old: Vec<&str> = self.lines[lines.clone()]
            .iter()
            .map(|l| l.raw.as_str())
            .collect();
        let eol = self.usual_eol();
        let mut middle = Vec::new();
        // Endings of removed lines not yet taken by a replacement.
        let mut removed: Vec<Eol> = Vec::new();
        for op in diff(&old, &new) {
            match op {
                DiffOp::Same { old: i, new: j } => {
                    removed.clear();
                    middle.push((new[j].to_string(), self.lines[lines.start + i].eol));
                }
                DiffOp::Removed { old: i } => removed.push(self.lines[lines.start + i].eol),
                DiffOp::Added { new: j } => {
                    let e = if removed.is_empty() {
                        eol
                    } else {
                        removed.remove(0)
                    };
                    middle.push((new[j].to_string(), e));
                }
            }
        }
        let before = self.lines[..lines.start]
            .iter()
            .map(|l| (l.raw.clone(), l.eol));
        let after = self.lines[lines.end..]
            .iter()
            .map(|l| (l.raw.clone(), l.eol));
        self.rebuild(before.chain(middle).chain(after).collect())
    }

    /// Where a new host goes (SPEC.md §4): before the first `Host *` or
    /// `Match all` block, and before the comment lines directly above it;
    /// the end of the file when there is none.
    pub fn insert_position(&self) -> usize {
        let catch_all = self.lines.iter().position(|l| match &l.kind {
            LineKind::HostHeader(h) => h.patterns.iter().any(|p| !p.negated && p.pattern == "*"),
            LineKind::MatchHeader(m) => m.is_all(),
            _ => false,
        });
        let Some(mut at) = catch_all else {
            return self.lines.len();
        };
        while at > 0 && self.lines[at - 1].kind == LineKind::Comment {
            at -= 1;
        }
        at
    }

    /// Inserts a new block (`text`, starting with its header) at
    /// [`ConfigFile::insert_position`], separated from its neighbours by blank
    /// lines. Returns the new file and the lines of the new block.
    pub fn insert_block(&self, text: &str) -> (ConfigFile, Range<usize>) {
        let at = self.insert_position();
        let mut new: Vec<&str> = split(text);
        while new.last().is_some_and(|l| l.trim().is_empty()) {
            new.pop();
        }
        let mut inserted: Vec<&str> = Vec::new();
        if at > 0 && self.lines[at - 1].kind != LineKind::Blank {
            inserted.push("");
        }
        let start = at + inserted.len();
        inserted.extend(&new);
        let end = at + inserted.len();
        if at < self.lines.len() {
            inserted.push("");
        }
        let eol = self.usual_eol();
        let lines = self.lines[..at]
            .iter()
            .map(|l| (l.raw.clone(), l.eol))
            .chain(inserted.iter().map(|raw| (raw.to_string(), eol)))
            .chain(self.lines[at..].iter().map(|l| (l.raw.clone(), l.eol)))
            .collect();
        (self.rebuild(lines), start..end)
    }

    /// CRLF when most lines use it, otherwise LF.
    fn usual_eol(&self) -> Eol {
        let crlf = self.lines.iter().filter(|l| l.eol == Eol::CrLf).count();
        let lf = self.lines.iter().filter(|l| l.eol == Eol::Lf).count();
        if crlf > lf { Eol::CrLf } else { Eol::Lf }
    }

    /// A file from `(raw, eol)` lines: every line but the last ends with a
    /// newline, and the last one does exactly when this file's last line did.
    fn rebuild(&self, mut lines: Vec<(String, Eol)>) -> ConfigFile {
        let eol = self.usual_eol();
        let final_newline = self.lines.last().is_none_or(|l| l.eol != Eol::None);
        let count = lines.len();
        for (i, (_, e)) in lines.iter_mut().enumerate() {
            if i + 1 < count {
                if *e == Eol::None {
                    *e = eol;
                }
            } else if !final_newline {
                *e = Eol::None;
            } else if *e == Eol::None {
                *e = eol;
            }
        }
        ConfigFile {
            path: self.path.clone(),
            bom: self.bom,
            lines: lines
                .into_iter()
                .enumerate()
                .map(|(i, (raw, eol))| Line::parse(i + 1, raw, eol))
                .collect(),
        }
    }
}

/// Lines of block text; empty text has none.
fn split(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

/// Arguments of every `keyword` directive in block `text`, in order (the
/// header line is not a directive). The first one is what ssh uses for
/// single-valued keywords.
pub fn values(text: &str, keyword: &str) -> Vec<Vec<String>> {
    split(text)
        .into_iter()
        .filter_map(|raw| match LineKind::parse(raw) {
            LineKind::Directive(d) if d.is(keyword) => Some(d.args),
            _ => None,
        })
        .collect()
}

/// Sets a single-valued `keyword`: rewrites its first line (later duplicates,
/// which ssh ignores, stay as they are) or adds one. `None` removes every line.
pub fn set(text: &str, keyword: &str, args: Option<&[String]>) -> String {
    let lines = split(text);
    let found = occurrences(&lines, keyword);
    match (args, found.first()) {
        (None, _) => keep(&lines, |i| !found.contains(&i)),
        (Some(args), Some(&first)) => {
            let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            if current_args(lines[first]) != args {
                out[first] = rewrite(lines[first], args);
            }
            out.join("\n")
        }
        (Some(args), None) => {
            let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            out.insert(content_end(&lines), new_line(&lines, keyword, args));
            out.join("\n")
        }
    }
}

/// Sets every value of a repeatable `keyword` (`IdentityFile`,
/// `LocalForward`, …). Old and new values are matched like a diff: kept
/// values keep their line, a changed value is rewritten in the line it
/// replaces, removed ones are deleted and added ones follow the line before.
pub fn set_all(text: &str, keyword: &str, all: &[Vec<String>]) -> String {
    let lines = split(text);
    let found = occurrences(&lines, keyword);
    let old: Vec<Vec<String>> = found.iter().map(|&i| current_args(lines[i])).collect();

    // Per line: `None` keeps it, `Some(None)` deletes, `Some(Some(t))` rewrites.
    let mut change: Vec<Option<Option<String>>> = vec![None; lines.len()];
    // (insert before this line index, new line)
    let mut inserts: Vec<(usize, String)> = Vec::new();
    let mut cursor = found
        .first()
        .copied()
        .unwrap_or_else(|| content_end(&lines));
    let mut removed: Vec<usize> = Vec::new();
    for op in diff(&old, all) {
        match op {
            DiffOp::Same { old: k, .. } => {
                for &r in &removed {
                    change[found[r]] = Some(None);
                }
                removed.clear();
                cursor = found[k] + 1;
            }
            DiffOp::Removed { old: k } => removed.push(k),
            DiffOp::Added { new: j } => {
                if removed.is_empty() {
                    inserts.push((cursor, new_line(&lines, keyword, &all[j])));
                } else {
                    let line = found[removed.remove(0)];
                    change[line] = Some(Some(rewrite(lines[line], &all[j])));
                    cursor = line + 1;
                }
            }
        }
    }
    for &r in &removed {
        change[found[r]] = Some(None);
    }

    let mut out = Vec::new();
    for i in 0..=lines.len() {
        out.extend(
            inserts
                .iter()
                .filter(|(at, _)| *at == i)
                .map(|(_, l)| l.clone()),
        );
        if i < lines.len() {
            match &change[i] {
                None => out.push(lines[i].to_string()),
                Some(Some(new)) => out.push(new.clone()),
                Some(None) => {}
            }
        }
    }
    out.join("\n")
}

/// Renames the alias `old` on the block's `Host` line (its first line) to
/// `new`; `None` when the first line is not a `Host` line naming `old`.
pub fn rename_alias(text: &str, old: &str, new: &str) -> Option<String> {
    let mut lines: Vec<String> = split(text).into_iter().map(str::to_string).collect();
    let first = lines.first()?;
    let LineKind::HostHeader(header) = LineKind::parse(first) else {
        return None;
    };
    if !header
        .patterns
        .iter()
        .any(|p| !p.negated && p.pattern == old)
    {
        return None;
    }
    let words: Vec<String> = header
        .patterns
        .iter()
        .map(|p| match (p.negated, p.pattern == old) {
            (true, _) => format!("!{}", p.pattern),
            (false, true) => new.to_string(),
            (false, false) => p.pattern.clone(),
        })
        .collect();
    lines[0] = rewrite(first, &words);
    Some(lines.join("\n"))
}

/// The aliases of a block's `Host` line (its first line), if it has one.
pub fn header_aliases(text: &str) -> Option<Vec<String>> {
    match LineKind::parse(split(text).first()?) {
        LineKind::HostHeader(h) => Some(
            h.patterns
                .into_iter()
                .filter(|p| !p.negated && !p.is_wildcard())
                .map(|p| p.pattern)
                .collect(),
        ),
        _ => None,
    }
}

fn occurrences(lines: &[&str], keyword: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, raw)| matches!(LineKind::parse(raw), LineKind::Directive(d) if d.is(keyword)))
        .map(|(i, _)| i)
        .collect()
}

fn keep(lines: &[&str], keep: impl Fn(usize) -> bool) -> String {
    lines
        .iter()
        .enumerate()
        .filter(|&(i, _)| keep(i))
        .map(|(_, l)| *l)
        .collect::<Vec<_>>()
        .join("\n")
}

fn current_args(raw: &str) -> Vec<String> {
    match LineKind::parse(raw) {
        LineKind::Directive(d) => d.args,
        LineKind::HostHeader(h) => h
            .patterns
            .into_iter()
            .map(|p| {
                if p.negated {
                    format!("!{}", p.pattern)
                } else {
                    p.pattern
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Index after the last line that is neither blank nor a comment: where a
/// new directive goes, so trailing blanks and the next block's leading
/// comments stay below it.
fn content_end(lines: &[&str]) -> usize {
    lines
        .iter()
        .rposition(|raw| !matches!(LineKind::parse(raw), LineKind::Blank | LineKind::Comment))
        .map_or(0, |i| i + 1)
}

/// A new directive line, indented like the block's first directive.
fn new_line(lines: &[&str], keyword: &str, args: &[String]) -> String {
    let indent = lines
        .iter()
        .filter(|raw| matches!(LineKind::parse(raw), LineKind::Directive(_)))
        .find_map(|raw| lexer::layout(raw).map(|l| l.indent.to_string()))
        .unwrap_or_else(|| DEFAULT_INDENT.to_string());
    format!("{indent}{keyword} {}", words(keyword, args))
}

/// `args` as written on a line: quoted words, or the raw command.
fn words(keyword: &str, args: &[String]) -> String {
    if line::is_raw_command(keyword) {
        args.join(" ")
    } else {
        args.iter()
            .map(|a| lexer::quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// `raw` with new arguments; indentation, keyword spelling, separator and a
/// trailing comment (with the space before it) are kept.
fn rewrite(raw: &str, args: &[String]) -> String {
    let Some(Layout {
        indent,
        keyword,
        separator,
        rest,
    }) = lexer::layout(raw)
    else {
        return raw.to_string();
    };
    let separator = if separator.is_empty() { " " } else { separator };
    let comment = if line::is_raw_command(keyword) {
        String::new()
    } else {
        match lexer::split_args(rest).ok().and_then(|a| a.comment) {
            Some(comment) => {
                let before = &rest[..rest.len() - comment.len()];
                let gap = &before[before.trim_end_matches([' ', '\t']).len()..];
                format!("{}{comment}", if gap.is_empty() { " " } else { gap })
            }
            None => String::new(),
        }
    };
    format!(
        "{indent}{keyword}{separator}{}{comment}",
        words(keyword, args)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> ConfigFile {
        ConfigFile::parse("config", text.as_bytes()).unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn text_round_trips_through_replace() {
        let f = file("# top\r\nHost a\r\n\tHostName x # c\r\n\r\nHost b\r\n  Port 1");
        let block = f.block_at(1).unwrap();
        let text = f.text(block.lines.clone());
        assert_eq!(text, "Host a\n\tHostName x # c\n");
        assert_eq!(f.replace_lines(block.lines, &text).to_bytes(), f.to_bytes());
    }

    #[test]
    fn replace_keeps_unchanged_line_endings() {
        let f = file("Host a\r\n  User u\n  Port 1\r\nHost b\n");
        let block = f.block_at(0).unwrap();
        let edited = f.replace_lines(block.lines, "Host a\n  User u\n  Port 2\n  Compression yes");
        // `Port 2` replaces `Port 1` (CRLF); the added line gets the usual
        // ending (a tie between CRLF and LF is LF).
        assert_eq!(
            edited.to_bytes(),
            b"Host a\r\n  User u\n  Port 2\r\n  Compression yes\nHost b\n"
        );
        let f = file("Host a\r\n  Port 1\r\nHost b\n");
        let edited = f.replace_lines(0..2, "Host a\n  Port 1\n  User u");
        assert_eq!(
            edited.to_bytes(),
            b"Host a\r\n  Port 1\r\n  User u\r\nHost b\n"
        );
    }

    #[test]
    fn replace_keeps_a_missing_final_newline() {
        let f = file("Host a\n  Port 1");
        let edited = f.replace_lines(0..2, "Host a\n  Port 1\n  User u");
        assert_eq!(edited.to_bytes(), b"Host a\n  Port 1\n  User u");
        let f = file("Host a\n  Port 1\n");
        assert_eq!(f.replace_lines(1..2, "").to_bytes(), b"Host a\n");
    }

    #[test]
    fn replace_keeps_bom_and_renumbers() {
        let f = ConfigFile::parse("c", b"\xEF\xBB\xBFHost a\nHost b\n").unwrap();
        let edited = f.replace_lines(0..1, "Host a\n  Port 2");
        assert!(edited.bom);
        assert_eq!(edited.lines[2].number, 3);
        assert!(matches!(edited.lines[2].kind, LineKind::HostHeader(_)));
    }

    #[test]
    fn set_rewrites_only_the_value() {
        let text = "Host a\n\thostname = old.example # prod\n  Port 22";
        assert_eq!(
            set(text, "HostName", Some(&s(&["new.example"]))),
            "Host a\n\thostname = new.example # prod\n  Port 22"
        );
        // Unchanged value: identical text, even with odd spacing.
        let odd = "Host a\n  Port   22  ";
        assert_eq!(set(odd, "port", Some(&s(&["22"]))), odd);
    }

    #[test]
    fn set_adds_after_the_last_directive_with_its_indent() {
        let text = "Host a\n\tHostName x\n\n# next block\n";
        assert_eq!(
            set(text, "User", Some(&s(&["me"]))),
            "Host a\n\tHostName x\n\tUser me\n\n# next block\n"
        );
        assert_eq!(
            set("Host a", "Port", Some(&s(&["2"]))),
            "Host a\n    Port 2"
        );
    }

    #[test]
    fn set_quotes_and_keeps_raw_commands() {
        assert_eq!(
            set("Host a", "IdentityFile", Some(&s(&["~/my keys/id"]))),
            "Host a\n    IdentityFile \"~/my keys/id\""
        );
        assert_eq!(
            set(
                "Host a\n  ProxyCommand old",
                "ProxyCommand",
                Some(&s(&["ssh -W \"%h:%p\" b"]))
            ),
            "Host a\n  ProxyCommand ssh -W \"%h:%p\" b"
        );
    }

    #[test]
    fn set_none_removes_every_line() {
        let text = "Host a\n  User x\n  Port 1\n  user y";
        assert_eq!(set(text, "User", None), "Host a\n  Port 1");
        assert_eq!(set("Host a", "User", None), "Host a");
    }

    #[test]
    fn set_touches_only_the_first_duplicate() {
        let text = "Host a\n  User x\n  User y";
        assert_eq!(
            set(text, "User", Some(&s(&["z"]))),
            "Host a\n  User z\n  User y"
        );
        assert_eq!(values(text, "user"), [s(&["x"]), s(&["y"])]);
    }

    #[test]
    fn set_all_rewrites_removes_and_appends() {
        let text = "Host a\n  LocalForward 1 h:1\n  User u\n  LocalForward 2 h:2 # two\n";
        assert_eq!(
            set_all(
                text,
                "LocalForward",
                &[s(&["1", "h:1"]), s(&["3", "h:3"]), s(&["4", "h:4"])]
            ),
            "Host a\n  LocalForward 1 h:1\n  User u\n  LocalForward 3 h:3 # two\n  LocalForward 4 h:4\n"
        );
        assert_eq!(
            set_all(text, "LocalForward", &[s(&["2", "h:2"])]),
            "Host a\n  User u\n  LocalForward 2 h:2 # two\n"
        );
        assert_eq!(
            set_all(
                "Host a\n  User u\n",
                "IdentityFile",
                &[s(&["~/k1"]), s(&["~/k2"])]
            ),
            "Host a\n  User u\n  IdentityFile ~/k1\n  IdentityFile ~/k2\n"
        );
        assert_eq!(set_all(text, "LocalForward", &[]), "Host a\n  User u\n");
    }

    #[test]
    fn rename_changes_only_that_alias() {
        assert_eq!(
            rename_alias("Host  prod db1 !bad # main\n  Port 1", "prod", "prod-db").as_deref(),
            Some("Host  prod-db db1 !bad # main\n  Port 1")
        );
        assert_eq!(rename_alias("Host a", "b", "c"), None);
        assert_eq!(rename_alias("Host !a", "a", "c"), None);
        assert_eq!(rename_alias("Match all", "a", "c"), None);
        assert_eq!(header_aliases("Host a b *.corp !c"), Some(s(&["a", "b"])));
    }

    #[test]
    fn new_host_goes_before_the_catch_all_and_its_comments() {
        let f = file("Host a\n  Port 1\n\n# defaults\nHost *\n  User me\n");
        assert_eq!(f.insert_position(), 3);
        let (edited, lines) = f.insert_block("Host new\n  HostName n\n");
        assert_eq!(
            String::from_utf8(edited.to_bytes()).unwrap(),
            "Host a\n  Port 1\n\nHost new\n  HostName n\n\n# defaults\nHost *\n  User me\n"
        );
        assert_eq!(lines, 3..5);

        let f = file("Host a\nMatch all\n");
        let (edited, lines) = f.insert_block("Host n");
        assert_eq!(edited.to_bytes(), b"Host a\n\nHost n\n\nMatch all\n");
        assert_eq!(lines, 2..3);
    }

    #[test]
    fn narrow_wildcards_do_not_count_as_catch_all() {
        let f = file("Host *.corp\n  User c\nHost !* x\n  Port 2");
        assert_eq!(f.insert_position(), 4);
        let (edited, lines) = f.insert_block("Host n");
        assert_eq!(
            edited.to_bytes(),
            b"Host *.corp\n  User c\nHost !* x\n  Port 2\n\nHost n"
        );
        assert_eq!(lines, 5..6);
    }

    #[test]
    fn new_host_in_an_empty_or_crlf_file() {
        let (edited, lines) = file("").insert_block("Host n\n  Port 1");
        assert_eq!(edited.to_bytes(), b"Host n\n  Port 1\n");
        assert_eq!(lines, 0..2);
        let (edited, _) = file("Host a\r\nHost *\r\n").insert_block("Host n");
        assert_eq!(edited.to_bytes(), b"Host a\r\n\r\nHost n\r\n\r\nHost *\r\n");
    }
}
