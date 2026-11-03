//! One config line: its raw text, line ending and parsed meaning.
//!
//! `raw` and `eol` are what gets written back; the parsed [`LineKind`] is
//! derived from them and never used for output.

use crate::lexer::{self, Head, LexError};

/// Directives whose value is the raw rest of the line (ssh `parse_command`):
/// quotes and `#` are part of the command, not syntax.
const RAW_COMMANDS: &[&str] = &[
    "proxycommand",
    "remotecommand",
    "localcommand",
    "knownhostscommand",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
    /// Last line of a file without a trailing newline.
    None,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::None => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 1-based line number in its file.
    pub number: usize,
    /// Original text without the line ending.
    pub raw: String,
    pub eol: Eol,
    pub kind: LineKind,
}

impl Line {
    pub fn parse(number: usize, raw: String, eol: Eol) -> Self {
        let kind = LineKind::parse(&raw);
        Self {
            number,
            raw,
            eol,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Blank,
    Comment,
    HostHeader(HostHeader),
    MatchHeader(MatchHeader),
    Include(Include),
    Directive(Directive),
    /// A line ssh would reject; kept verbatim so it still round-trips.
    Invalid(Invalid),
}

impl LineKind {
    pub fn parse(raw: &str) -> Self {
        let (keyword, rest) = match lexer::head(raw) {
            Head::Blank => return LineKind::Blank,
            Head::Comment => return LineKind::Comment,
            Head::Keyword { keyword, rest } => (keyword, rest),
        };
        if keyword.is_empty() {
            return invalid("missing keyword");
        }
        if keyword.contains(['"', '\'']) {
            return invalid("quote in keyword");
        }
        let args = match lexer::split_args(rest) {
            Ok(args) => args,
            Err(LexError::UnterminatedQuote) => return invalid("unterminated quote"),
        };
        let comment = args.comment;

        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                if args.values.is_empty() {
                    return invalid("`Host` without patterns");
                }
                let mut patterns = Vec::with_capacity(args.values.len());
                for value in args.values {
                    let pattern = HostPattern::new(value);
                    if pattern.pattern.is_empty() {
                        return invalid("empty host pattern");
                    }
                    patterns.push(pattern);
                }
                LineKind::HostHeader(HostHeader { patterns, comment })
            }
            "match" => {
                if args.values.is_empty() {
                    return invalid("`Match` without criteria");
                }
                LineKind::MatchHeader(MatchHeader {
                    args: args.values,
                    comment,
                })
            }
            "include" => {
                if args.values.is_empty() {
                    return invalid("`Include` without paths");
                }
                LineKind::Include(Include {
                    paths: args.values,
                    comment,
                })
            }
            lower if is_raw_command(lower) => LineKind::Directive(Directive {
                keyword: keyword.to_string(),
                args: if rest.is_empty() {
                    Vec::new()
                } else {
                    vec![rest.to_string()]
                },
                comment: None,
            }),
            _ => LineKind::Directive(Directive {
                keyword: keyword.to_string(),
                args: args.values,
                comment,
            }),
        }
    }

    /// `Host` and `Match` lines start a new block.
    pub fn is_header(&self) -> bool {
        matches!(self, LineKind::HostHeader(_) | LineKind::MatchHeader(_))
    }
}

/// Whether `keyword` takes the raw rest of the line as its value.
pub(crate) fn is_raw_command(keyword: &str) -> bool {
    RAW_COMMANDS.iter().any(|k| k.eq_ignore_ascii_case(keyword))
}

fn invalid(reason: &str) -> LineKind {
    LineKind::Invalid(Invalid {
        reason: reason.to_string(),
    })
}

/// One pattern of a `Host` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPattern {
    /// Pattern text without the leading `!`.
    pub pattern: String,
    pub negated: bool,
}

impl HostPattern {
    fn new(value: String) -> Self {
        match value.strip_prefix('!') {
            Some(p) => Self {
                pattern: p.to_string(),
                negated: true,
            },
            None => Self {
                pattern: value,
                negated: false,
            },
        }
    }

    pub fn is_wildcard(&self) -> bool {
        self.pattern.contains(['*', '?'])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostHeader {
    pub patterns: Vec<HostPattern>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchHeader {
    /// Criteria words as ssh tokenizes them (e.g. `host`, `a,b`, `exec`, `cmd`).
    pub args: Vec<String>,
    pub comment: Option<String>,
}

impl MatchHeader {
    /// `Match all` (also `Match canonical all`, `Match final all`).
    pub fn is_all(&self) -> bool {
        self.args.iter().any(|a| a.eq_ignore_ascii_case("all"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    /// Paths as written (unexpanded).
    pub paths: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// Keyword as written (case preserved).
    pub keyword: String,
    pub args: Vec<String>,
    pub comment: Option<String>,
}

impl Directive {
    /// Case-insensitive keyword comparison, as ssh does.
    pub fn is(&self, keyword: &str) -> bool {
        self.keyword.eq_ignore_ascii_case(keyword)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive(raw: &str) -> Directive {
        match LineKind::parse(raw) {
            LineKind::Directive(d) => d,
            other => panic!("{raw:?} parsed as {other:?}"),
        }
    }

    fn host(raw: &str) -> HostHeader {
        match LineKind::parse(raw) {
            LineKind::HostHeader(h) => h,
            other => panic!("{raw:?} parsed as {other:?}"),
        }
    }

    fn invalid_reason(raw: &str) -> String {
        match LineKind::parse(raw) {
            LineKind::Invalid(i) => i.reason,
            other => panic!("{raw:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn blank_and_comment() {
        assert_eq!(LineKind::parse("   "), LineKind::Blank);
        assert_eq!(LineKind::parse("  # note"), LineKind::Comment);
    }

    #[test]
    fn directive_forms() {
        for raw in [
            "HostName example.com",
            "HostName=example.com",
            "  hostname = example.com  ",
            "\tHOSTNAME\texample.com",
        ] {
            let d = directive(raw);
            assert!(d.is("HostName"), "{raw:?}");
            assert_eq!(d.args, ["example.com"], "{raw:?}");
        }
        assert_eq!(directive("hostname x").keyword, "hostname");
    }

    #[test]
    fn multi_value_and_trailing_comment() {
        let d = directive("LocalForward 8080 localhost:80 # web");
        assert_eq!(d.args, ["8080", "localhost:80"]);
        assert_eq!(d.comment.as_deref(), Some("# web"));
    }

    #[test]
    fn unknown_directive_is_kept() {
        let d = directive("FrobnicateLevel 11");
        assert_eq!(d.keyword, "FrobnicateLevel");
        assert_eq!(d.args, ["11"]);
    }

    #[test]
    fn raw_command_value() {
        let d = directive(r#"ProxyCommand ssh -W "%h:%p" bastion # not a comment"#);
        assert_eq!(d.args, [r#"ssh -W "%h:%p" bastion # not a comment"#]);
        assert_eq!(d.comment, None);
        assert_eq!(directive("RemoteCommand=tmux attach").args, ["tmux attach"]);
    }

    #[test]
    fn host_patterns() {
        let h = host("Host prod-db db1 !bad *.corp web?");
        let shown: Vec<_> = h
            .patterns
            .iter()
            .map(|p| (p.pattern.as_str(), p.negated, p.is_wildcard()))
            .collect();
        assert_eq!(
            shown,
            [
                ("prod-db", false, false),
                ("db1", false, false),
                ("bad", true, false),
                ("*.corp", false, true),
                ("web?", false, true),
            ]
        );
        assert_eq!(host("host=a").patterns[0].pattern, "a");
        assert_eq!(
            host(r#"Host "spaced name""#).patterns[0].pattern,
            "spaced name"
        );
        assert_eq!(host("HOST a # c").comment.as_deref(), Some("# c"));
    }

    #[test]
    fn match_header() {
        let LineKind::MatchHeader(m) = LineKind::parse(r#"Match host a,b exec "test -f x""#) else {
            panic!("not a match header");
        };
        assert_eq!(m.args, ["host", "a,b", "exec", "test -f x"]);
        assert!(!m.is_all());
        let LineKind::MatchHeader(m) = LineKind::parse("match ALL") else {
            panic!("not a match header");
        };
        assert!(m.is_all());
    }

    #[test]
    fn include_paths() {
        let LineKind::Include(inc) = LineKind::parse("Include conf.d/*.conf ~/extra # x") else {
            panic!("not an include");
        };
        assert_eq!(inc.paths, ["conf.d/*.conf", "~/extra"]);
        assert_eq!(inc.comment.as_deref(), Some("# x"));
        assert!(matches!(LineKind::parse("INCLUDE a"), LineKind::Include(_)));
    }

    #[test]
    fn invalid_lines() {
        assert_eq!(invalid_reason("HostName \"open"), "unterminated quote");
        assert_eq!(invalid_reason("Host"), "`Host` without patterns");
        assert_eq!(invalid_reason("Host !"), "empty host pattern");
        assert_eq!(invalid_reason("Host \"\""), "empty host pattern");
        assert_eq!(invalid_reason("Match"), "`Match` without criteria");
        assert_eq!(
            invalid_reason("Include # nothing"),
            "`Include` without paths"
        );
        assert_eq!(invalid_reason("= value"), "missing keyword");
        assert_eq!(invalid_reason("Host\"x\" y"), "quote in keyword");
    }

    #[test]
    fn headers() {
        assert!(LineKind::parse("Host a").is_header());
        assert!(LineKind::parse("Match all").is_header());
        assert!(!LineKind::parse("Include a").is_header());
    }
}
