//! Tokenizer for a single config line, mirroring OpenSSH `readconf.c`:
//! `strdelim` for the keyword and `argv_split` (with comment termination) for
//! the arguments. Verified against OpenSSH_for_Windows 9.5p1 with `ssh -G`.

/// Characters ssh strips from the end of a line before parsing.
const TRAILING_WS: &[char] = &[' ', '\t', '\r', '\n', '\x0c'];
/// ssh's `WHITESPACE` set used around the keyword.
const WHITESPACE: &[char] = &[' ', '\t', '\r', '\n'];

/// First-level split of a line.
#[derive(Debug, PartialEq, Eq)]
pub enum Head<'a> {
    Blank,
    Comment,
    /// `rest` starts after the keyword separator (whitespace and at most one `=`).
    Keyword {
        keyword: &'a str,
        rest: &'a str,
    },
}

/// Classifies a raw line (without its line ending) and splits off the keyword.
pub fn head(raw: &str) -> Head<'_> {
    let line = raw
        .trim_end_matches(TRAILING_WS)
        .trim_start_matches(WHITESPACE);
    if line.is_empty() {
        return Head::Blank;
    }
    if line.starts_with('#') {
        return Head::Comment;
    }
    let end = line
        .find(|c: char| WHITESPACE.contains(&c) || c == '=')
        .unwrap_or(line.len());
    let keyword = &line[..end];
    let mut rest = line[end..].trim_start_matches(WHITESPACE);
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start_matches(WHITESPACE);
    }
    Head::Keyword { keyword, rest }
}

/// Arguments of a line and its trailing comment (`# …`, kept verbatim).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub values: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LexError {
    UnterminatedQuote,
}

/// Splits arguments like OpenSSH `argv_split(…, terminate_on_comment = 1)`:
/// whitespace-separated words; `"…"` / `'…'` quotes anywhere in a word;
/// escapes `\"`, `\'`, `\\` and (outside quotes) `\ `; a `#` at the start of a
/// word starts a comment that runs to the end of the line.
pub fn split_args(rest: &str) -> Result<Args, LexError> {
    let chars: Vec<(usize, char)> = rest.char_indices().collect();
    let mut args = Args::default();
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if c == ' ' || c == '\t' {
            i += 1;
            continue;
        }
        if c == '#' {
            args.comment = Some(rest[pos..].to_string());
            break;
        }

        let mut word = String::new();
        let mut quote: Option<char> = None;
        while i < chars.len() {
            let c = chars[i].1;
            let next = chars.get(i + 1).map(|&(_, n)| n);
            if c == '\\' {
                match next {
                    Some(n @ ('\'' | '"' | '\\')) => {
                        word.push(n);
                        i += 2;
                        continue;
                    }
                    Some(' ') if quote.is_none() => {
                        word.push(' ');
                        i += 2;
                        continue;
                    }
                    // Unrecognised escape: the backslash is kept.
                    _ => word.push(c),
                }
            } else if quote.is_none() && (c == ' ' || c == '\t') {
                break;
            } else if quote.is_none() && (c == '"' || c == '\'') {
                quote = Some(c);
            } else if quote == Some(c) {
                quote = None;
            } else {
                word.push(c);
            }
            i += 1;
        }
        if quote.is_some() {
            return Err(LexError::UnterminatedQuote);
        }
        args.values.push(word);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw<'a>(keyword: &'a str, rest: &'a str) -> Head<'a> {
        Head::Keyword { keyword, rest }
    }

    fn values(rest: &str) -> Vec<String> {
        split_args(rest).expect("valid args").values
    }

    #[test]
    fn blank_and_comment_lines() {
        assert_eq!(head(""), Head::Blank);
        assert_eq!(head(" \t \r"), Head::Blank);
        assert_eq!(head("\x0c"), Head::Blank);
        assert_eq!(head("# comment"), Head::Comment);
        assert_eq!(head("\t  #indented"), Head::Comment);
    }

    #[test]
    fn keyword_separators() {
        assert_eq!(head("HostName example.com"), kw("HostName", "example.com"));
        assert_eq!(head("HostName=example.com"), kw("HostName", "example.com"));
        assert_eq!(
            head("HostName = example.com"),
            kw("HostName", "example.com")
        );
        assert_eq!(
            head("HostName\t=\texample.com"),
            kw("HostName", "example.com")
        );
        assert_eq!(head("  Port 22  \t"), kw("Port", "22"));
        assert_eq!(head("Port"), kw("Port", ""));
        // Only one `=` belongs to the separator.
        assert_eq!(head("SetEnv ==x"), kw("SetEnv", "=x"));
        assert_eq!(head("SetEnv= =x"), kw("SetEnv", "=x"));
    }

    #[test]
    fn keyword_with_hash_is_not_a_comment() {
        assert_eq!(head("Host#x y"), kw("Host#x", "y"));
    }

    #[test]
    fn plain_words() {
        assert_eq!(values("a b\t\tc"), ["a", "b", "c"]);
        assert!(values("").is_empty());
    }

    #[test]
    fn quotes() {
        assert_eq!(values(r#""my user""#), ["my user"]);
        assert_eq!(values("'single quoted'"), ["single quoted"]);
        assert_eq!(values(r#"a"b c"d"#), ["ab cd"]);
        assert_eq!(values(r#""" x"#), ["", "x"]);
        assert_eq!(values(r#""it's""#), ["it's"]);
        assert_eq!(values(r#"'say "hi"'"#), [r#"say "hi""#]);
    }

    #[test]
    fn escapes() {
        assert_eq!(values(r#"a\"b"#), [r#"a"b"#]);
        assert_eq!(values(r"a\'b"), ["a'b"]);
        assert_eq!(values(r"a\\b"), [r"a\b"]);
        assert_eq!(values(r"a\ b c"), ["a b", "c"]);
        // Unrecognised escapes keep the backslash (Windows paths survive).
        assert_eq!(values(r"C:\Users\me\.ssh\id"), [r"C:\Users\me\.ssh\id"]);
    }

    #[test]
    fn trailing_comment() {
        let args = split_args("example.com # prod box").unwrap();
        assert_eq!(args.values, ["example.com"]);
        assert_eq!(args.comment.as_deref(), Some("# prod box"));
        // `#` inside a word or quotes is data.
        assert_eq!(values("x#y"), ["x#y"]);
        assert_eq!(values(r#""a # b""#), ["a # b"]);
        assert_eq!(
            split_args("#only").unwrap().comment.as_deref(),
            Some("#only")
        );
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(split_args(r#""open"#), Err(LexError::UnterminatedQuote));
        assert_eq!(split_args("a 'b"), Err(LexError::UnterminatedQuote));
    }
}
