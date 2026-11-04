//! Streamer mode's filter (SPEC.md §10 *Streamer mode*): which parts of a
//! text could identify a machine, an account or the user, and of what kind.
//!
//! A [`Redactor`] knows the sensitive values of the loaded config and tunnels
//! (`HostName`, `User`, ProxyJump / ProxyCommand hosts, key and other file
//! paths, forward bind addresses and targets), the home folder and the local
//! login. It also recognizes shapes: IPv4 and IPv6 addresses, paths under
//! `~` or home, key fingerprints and public-key blobs, and port numbers in
//! context (`port 22`, `host:22`, `-p 22`). Core rebuilds it on every config
//! load and tunnel change. It never decides how a token looks; the UI does.

use std::collections::{HashMap, HashSet};
use std::net::Ipv6Addr;
use std::path::Path;

use sshconf::{ConfigSet, HostList, LineKind};

use crate::tunnels::TunnelsFile;

/// What a masked value is; free text shows it as this label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Ip,
    Host,
    User,
    Port,
    Path,
    Key,
}

impl Kind {
    /// The label free text shows instead of the value.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ip => "ip",
            Kind::Host => "host",
            Kind::User => "user",
            Kind::Port => "port",
            Kind::Path => "path",
            Kind::Key => "key",
        }
    }
}

/// A piece of a filtered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment<'a> {
    /// Shown as it is.
    Plain(&'a str),
    /// A sensitive value of `kind`.
    Token(Kind, &'a str),
    /// A host alias, shown as "host N" (only with hidden aliases).
    Alias(&'a str, usize),
}

/// The five dots every masked field shows, never the real length.
pub const DOTS: &str = "•••••";

/// What a known value is matched as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needle {
    /// A word-bounded value (host, user).
    Word(Kind),
    /// A path prefix: separators and ASCII case are loose, and the match
    /// runs on to the end of the path.
    Path,
    /// A host alias (only matched with hidden aliases).
    Alias(usize),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redactor {
    /// Known values, longest first.
    needles: Vec<(String, Needle)>,
    /// Alias → its host's position in the config (from 1).
    aliases: HashMap<String, usize>,
    /// Aliases that are masked whenever streamer mode is on: an IP address,
    /// or equal to a HostName.
    always: HashSet<String>,
}

/// Sources the redactor is built from.
pub struct Sources<'a> {
    pub set: Option<&'a ConfigSet>,
    pub hosts: Option<&'a HostList>,
    pub tunnels: &'a TunnelsFile,
    /// The user's home folder.
    pub home: &'a Path,
    /// The local login (`ssh -G` reports it as `user` when none is set).
    pub local_user: Option<&'a str>,
}

impl Redactor {
    pub fn build(sources: Sources<'_>) -> Self {
        let mut known = Known::default();
        let mut aliases = HashMap::new();
        if let Some(hosts) = sources.hosts {
            for (index, host) in hosts.hosts.iter().enumerate() {
                for alias in &host.aliases {
                    aliases.entry(alias.clone()).or_insert(index + 1);
                }
            }
        }
        if let Some(set) = sources.set {
            for line in set.files.iter().flat_map(|f| &f.lines) {
                if let LineKind::Directive(d) = &line.kind {
                    known.directive(&d.keyword.to_ascii_lowercase(), &d.args, &aliases);
                }
            }
        }
        for tunnel in &sources.tunnels.tunnels {
            for forward in &tunnel.forwards {
                known.address(&forward.bind, &aliases);
                if let Some(target) = &forward.target {
                    known.host_port(target, &aliases);
                }
            }
        }
        let home = sources.home.to_string_lossy();
        if home.len() > 3 {
            known
                .paths
                .insert(home.trim_end_matches(['/', '\\']).to_string());
        }
        if let Some(user) = sources.local_user {
            known.word(user, Kind::User);
        }

        let always = aliases
            .keys()
            .filter(|a| is_ip(a) || known.words.get(*a) == Some(&Kind::Host))
            .cloned()
            .collect();
        let mut needles: Vec<(String, Needle)> = known
            .words
            .into_iter()
            .map(|(value, kind)| (value, Needle::Word(kind)))
            .chain(known.paths.into_iter().map(|p| (p, Needle::Path)))
            .chain(aliases.iter().map(|(a, n)| (a.clone(), Needle::Alias(*n))))
            .collect();
        needles.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        Redactor {
            needles,
            aliases,
            always,
        }
    }

    /// The "host N" number of `alias`.
    pub fn alias_number(&self, alias: &str) -> Option<usize> {
        self.aliases.get(alias).copied()
    }

    /// Whether `alias` is masked whenever streamer mode is on (an IP, or
    /// equal to a HostName), not only with hidden aliases.
    pub fn alias_always_masked(&self, alias: &str) -> bool {
        self.always.contains(alias) || is_ip(alias)
    }

    /// Splits `text` into plain parts and tokens. Aliases become
    /// [`Segment::Alias`] only when `hide_aliases`.
    pub fn segments<'a>(&self, text: &'a str, hide_aliases: bool) -> Vec<Segment<'a>> {
        let mut out = Vec::new();
        let mut plain_from = 0;
        let mut last_token_end = None;
        let mut i = 0;
        while i < text.len() {
            if !text.is_char_boundary(i) {
                i += 1;
                continue;
            }
            match self.match_at(text, i, hide_aliases, last_token_end) {
                Some((end, segment)) => {
                    if plain_from < i {
                        out.push(Segment::Plain(&text[plain_from..i]));
                    }
                    // `:port` follows an address, not a path (`config:3`).
                    if matches!(
                        segment,
                        Segment::Token(Kind::Ip | Kind::Host, _) | Segment::Alias(..)
                    ) {
                        last_token_end = Some(end);
                    }
                    out.push(segment);
                    plain_from = end;
                    i = end;
                }
                None => i += text[i..].chars().next().map_or(1, char::len_utf8),
            }
        }
        if plain_from < text.len() {
            out.push(Segment::Plain(&text[plain_from..]));
        }
        out
    }

    /// Whether `text` holds anything sensitive (a token).
    pub fn finds(&self, text: &str) -> bool {
        self.segments(text, false)
            .iter()
            .any(|s| matches!(s, Segment::Token(..)))
    }

    /// `text` with every token replaced by [`DOTS`] and every hidden alias
    /// by "host N": for places that can't draw tokens (the native tray menu,
    /// window titles).
    pub fn mask_plain(&self, text: &str, hide_aliases: bool) -> String {
        self.segments(text, hide_aliases)
            .into_iter()
            .map(|segment| match segment {
                Segment::Plain(s) => s.to_string(),
                Segment::Token(..) => DOTS.to_string(),
                Segment::Alias(_, n) => format!("host {n}"),
            })
            .collect()
    }

    /// The longest token starting at byte `i`, and where it ends.
    fn match_at<'a>(
        &self,
        text: &'a str,
        i: usize,
        hide_aliases: bool,
        last_token_end: Option<usize>,
    ) -> Option<(usize, Segment<'a>)> {
        let mut best: Option<(usize, Segment<'a>)> = None;
        let mut take = |end: usize, segment: Segment<'a>| {
            if best.as_ref().is_none_or(|(e, _)| end > *e) {
                best = Some((end, segment));
            }
        };
        let rest = &text[i..];
        for (value, needle) in &self.needles {
            match needle {
                Needle::Word(kind) => {
                    if rest.starts_with(value.as_str()) && word_bounded(text, i, i + value.len()) {
                        take(
                            i + value.len(),
                            Segment::Token(*kind, &text[i..i + value.len()]),
                        );
                    }
                }
                Needle::Path => {
                    if let Some(len) = path_prefix(rest, value) {
                        let end = path_end(text, i + len);
                        take(end, Segment::Token(Kind::Path, &text[i..end]));
                    }
                }
                Needle::Alias(n) => {
                    if hide_aliases
                        && rest.starts_with(value.as_str())
                        && word_bounded(text, i, i + value.len())
                    {
                        take(
                            i + value.len(),
                            Segment::Alias(&text[i..i + value.len()], *n),
                        );
                    }
                }
            }
        }
        if let Some(end) = ipv4_at(text, i) {
            take(end, Segment::Token(Kind::Ip, &text[i..end]));
        }
        if let Some(end) = ipv6_at(text, i) {
            take(end, Segment::Token(Kind::Ip, &text[i..end]));
        }
        if let Some(end) = tilde_path_at(text, i) {
            take(end, Segment::Token(Kind::Path, &text[i..end]));
        }
        if let Some(end) = key_at(text, i) {
            take(end, Segment::Token(Kind::Key, &text[i..end]));
        }
        if let Some(end) = port_at(text, i, last_token_end) {
            take(end, Segment::Token(Kind::Port, &text[i..end]));
        }
        best
    }
}

/// Values collected from the config and tunnels.
#[derive(Default)]
struct Known {
    words: HashMap<String, Kind>,
    paths: HashSet<String>,
}

impl Known {
    fn word(&mut self, value: &str, kind: Kind) {
        let value = value.trim_matches(['"', '\'']);
        // One letter would mask half of every sentence; numbers are ports.
        if value.chars().count() < 2 || value.chars().all(|c| c.is_ascii_digit()) {
            return;
        }
        if is_ip(value) {
            // IP shapes are always caught; still record the kind.
            self.words.insert(value.to_string(), Kind::Ip);
            return;
        }
        // A host wins over a user of the same text (both masked anyway).
        self.words.entry(value.to_string()).or_insert(kind);
    }

    fn path(&mut self, value: &str) {
        let value = value.trim_matches(['"', '\'']);
        if value.len() > 3 && !value.eq_ignore_ascii_case("none") {
            self.paths.insert(value.to_string());
        }
    }

    /// A host that isn't one of the config's aliases (those stay visible).
    fn host(&mut self, host: &str, aliases: &HashMap<String, usize>) {
        let host = host.trim_matches(['[', ']']);
        if host.is_empty() || host.contains('%') || (aliases.contains_key(host) && !is_ip(host)) {
            return;
        }
        self.word(host, Kind::Host);
    }

    /// `[user@]host[:port]`, also as `ssh://…`.
    fn jump(&mut self, value: &str, aliases: &HashMap<String, usize>) {
        let value = value.strip_prefix("ssh://").unwrap_or(value);
        let host = match value.rsplit_once('@') {
            Some((user, host)) => {
                self.word(user, Kind::User);
                host
            }
            None => value,
        };
        self.host_port(host, aliases);
    }

    /// `host:port`, `[v6]:port`, or a host alone.
    fn host_port(&mut self, value: &str, aliases: &HashMap<String, usize>) {
        if let Some(inner) = value.strip_prefix('[') {
            self.host(inner.split(']').next().unwrap_or(""), aliases);
        } else if value.matches(':').count() == 1 {
            self.host(value.split(':').next().unwrap_or(""), aliases);
        } else {
            self.host(value, aliases);
        }
    }

    /// A bind address (`127.0.0.1`, `*`, `localhost`, a v6 address).
    fn address(&mut self, value: &str, aliases: &HashMap<String, usize>) {
        if value != "*" && !value.is_empty() {
            self.host(value, aliases);
        }
    }

    fn directive(&mut self, keyword: &str, args: &[String], aliases: &HashMap<String, usize>) {
        match keyword {
            "hostname" => args.iter().for_each(|a| self.word(a, Kind::Host)),
            "user" => args.iter().for_each(|a| self.word(a, Kind::User)),
            "identityfile"
            | "certificatefile"
            | "userknownhostsfile"
            | "globalknownhostsfile"
            | "controlpath"
            | "identityagent"
            | "securitykeyprovider"
            | "pkcs11provider"
            | "revokedhostkeys"
            | "knownhostscommand" => {
                args.iter().for_each(|a| self.path(a));
            }
            "proxyjump" => {
                for jump in args.iter().flat_map(|a| a.split(',')) {
                    if !jump.eq_ignore_ascii_case("none") {
                        self.jump(jump, aliases);
                    }
                }
            }
            "proxycommand" => {
                // The command is one argument: its words are what counts.
                for word in args.iter().flat_map(|a| a.split_whitespace()) {
                    // Programs and flags stay; addresses (with a dot) and
                    // `user@host` are values.
                    if word.starts_with(['-', '/', '~']) || word.contains('%') {
                        continue;
                    }
                    if word.contains('@') {
                        self.jump(word, aliases);
                    } else if word.contains('.') && !word.contains('/') {
                        self.host_port(word, aliases);
                    }
                }
            }
            "localforward" | "remoteforward" | "dynamicforward" => {
                for arg in args {
                    // `[bind:]port`, `host:hostport`, `[v6]:port`, a socket path.
                    if arg.starts_with(['/', '~']) {
                        self.path(arg);
                    } else if arg.starts_with('[') || arg.matches(':').count() <= 1 {
                        self.host_port(arg, aliases);
                    } else if let Some((bind, _)) = arg.split_once(':') {
                        self.address(bind, aliases);
                    }
                }
            }
            "bindaddress" | "hostkeyalias" => args.iter().for_each(|a| self.host(a, aliases)),
            _ => {}
        }
    }
}

fn is_ip(value: &str) -> bool {
    let value = value.trim_matches(['[', ']']);
    value.parse::<std::net::IpAddr>().is_ok()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// Nothing word-like touches `text[start..end]` on either side.
fn word_bounded(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_none_or(|c| !is_word_char(c)) && after.is_none_or(|c| !is_word_char(c))
}

/// Characters a path runs on with.
fn is_path_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '"' | '\'' | '<' | '>' | '|' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '`'
        )
}

/// Where the path through byte `from` ends; a trailing `.` or `:` (end of a
/// sentence) is left out.
fn path_end(text: &str, from: usize) -> usize {
    let mut end = from;
    for (offset, c) in text[from..].char_indices() {
        if !is_path_char(c) {
            break;
        }
        end = from + offset + c.len_utf8();
    }
    // A `:line` suffix (`config:3`) is a position, not part of the path.
    let run = &text[from..end];
    if let Some(colon) = run.rfind(':') {
        let digits = &run[colon + 1..];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            end = from + colon;
        }
    }
    while end > from && text[..end].ends_with(['.', ':']) {
        end -= 1;
    }
    end
}

/// The length of `rest`'s prefix equal to `path`, with `/` and `\` equal
/// and ASCII case ignored; the next character must not continue a name.
fn path_prefix(rest: &str, path: &str) -> Option<usize> {
    let mut len = 0;
    let mut chars = rest.chars();
    for p in path.chars() {
        let c = chars.next()?;
        let same =
            c.eq_ignore_ascii_case(&p) || (matches!(c, '/' | '\\') && matches!(p, '/' | '\\'));
        if !same {
            return None;
        }
        len += c.len_utf8();
    }
    match rest[len..].chars().next() {
        Some(c) if is_word_char(c) || c == '.' => None,
        _ => Some(len),
    }
}

/// `~/…` or `~\…` at a word start.
fn tilde_path_at(text: &str, i: usize) -> Option<usize> {
    let rest = &text[i..];
    if !(rest.starts_with("~/") || rest.starts_with("~\\")) {
        return None;
    }
    if text[..i]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '(' | '[' | '='))
    {
        return None;
    }
    let end = path_end(text, i + 2);
    (end > i + 2).then_some(end)
}

/// An IPv4 address starting at byte `i`.
fn ipv4_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.') {
        return None;
    }
    let mut j = i;
    for octet in 0..4 {
        let start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() && j - start < 3 {
            j += 1;
        }
        if j == start || text[start..j].parse::<u16>().ok()? > 255 {
            return None;
        }
        if octet < 3 {
            if bytes.get(j) != Some(&b'.') {
                return None;
            }
            j += 1;
        }
    }
    let next_is = |k: usize, f: fn(u8) -> bool| bytes.get(k).is_some_and(|b| f(*b));
    if next_is(j, |b| b.is_ascii_alphanumeric())
        || (next_is(j, |b| b == b'.') && next_is(j + 1, |b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(j)
}

/// An IPv6 address starting at byte `i` (with a `%zone` if present).
fn ipv6_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let v6 = |b: u8| b.is_ascii_hexdigit() || b == b':' || b == b'.';
    if !v6(bytes[i]) || (i > 0 && (v6(bytes[i - 1]) || bytes[i - 1].is_ascii_alphanumeric())) {
        return None;
    }
    let mut end = i;
    while end < bytes.len() && v6(bytes[end]) {
        end += 1;
    }
    // Trailing `:` or `.` belongs to the sentence, not the address.
    while end > i && matches!(bytes[end - 1], b':' | b'.') && !text[i..end].ends_with("::") {
        end -= 1;
    }
    let candidate = &text[i..end];
    if candidate.matches(':').count() < 2 || !candidate.bytes().any(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    candidate.parse::<Ipv6Addr>().ok()?;
    if bytes.get(end) == Some(&b'%') {
        let mut zone = end + 1;
        while zone < bytes.len() && bytes[zone].is_ascii_alphanumeric() {
            zone += 1;
        }
        if zone > end + 1 {
            end = zone;
        }
    }
    if bytes.get(end).is_some_and(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(end)
}

/// A key fingerprint (the hash after `SHA256:` or `MD5:`, or 16 colon-
/// separated hex pairs) or a public-key blob (`AAAA…`).
fn key_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let base64 = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=');
    let run = |from: usize, ok: fn(u8) -> bool| {
        let mut j = from;
        while j < bytes.len() && ok(bytes[j]) {
            j += 1;
        }
        j
    };
    let before = &bytes[..i];
    // The hash after "SHA256:".
    if before.ends_with(b"SHA256:") {
        let end = run(i, base64);
        return (end - i >= 20).then_some(end);
    }
    // MD5: aa:bb:… (16 pairs), also after "MD5:".
    if i == 0 || !bytes[i - 1].is_ascii_alphanumeric() {
        let mut j = i;
        let mut pairs = 0;
        while j + 1 < bytes.len()
            && bytes[j].is_ascii_hexdigit()
            && bytes[j + 1].is_ascii_hexdigit()
        {
            pairs += 1;
            j += 2;
            if pairs < 16 && bytes.get(j) == Some(&b':') {
                j += 1;
            } else {
                break;
            }
        }
        if pairs == 16 && !bytes.get(j).is_some_and(|b| b.is_ascii_hexdigit()) {
            return Some(j);
        }
    }
    // A public-key blob.
    if text[i..].starts_with("AAAA") && (i == 0 || !base64(bytes[i - 1])) {
        let end = run(i, base64);
        return (end - i >= 40).then_some(end);
    }
    None
}

/// A port number in context: after "port " (any case), "-p ", or a `:` that
/// directly follows a token or a `]`.
fn port_at(text: &str, i: usize, last_token_end: Option<usize>) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes[i].is_ascii_digit() || (i > 0 && bytes[i - 1].is_ascii_digit()) {
        return None;
    }
    let before = &text[..i];
    let lower_tail = before[before.len().saturating_sub(5)..].to_ascii_lowercase();
    let in_context = lower_tail.ends_with("port ")
        || before.ends_with("-p ")
        || (before.ends_with(':')
            && (last_token_end == Some(i - 1) || before[..before.len() - 1].ends_with(']')));
    if !in_context {
        return None;
    }
    let mut end = i;
    while end < bytes.len() && bytes[end].is_ascii_digit() && end - i < 5 {
        end += 1;
    }
    if bytes.get(end).is_some_and(|b| b.is_ascii_alphanumeric())
        || text[i..end].parse::<u32>().ok()? > 65535
    {
        return None;
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use sshconf::{ConfigSet, ResolveOptions};

    use super::*;
    use crate::tunnels::{DEFAULT_BIND, Forward, ForwardKind, Tunnel};

    fn redactor(config: &str, tunnels: &[(&str, &str)], hide: bool) -> (Redactor, bool) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config");
        std::fs::write(&path, config).unwrap();
        let set = ConfigSet::load(&path, &ResolveOptions::new(tmp.path())).unwrap();
        let hosts = HostList::from_set(&set);
        let mut file = TunnelsFile::default();
        for (n, (bind, target)) in tunnels.iter().enumerate() {
            file.upsert_tunnel(Tunnel {
                id: format!("t-{n}"),
                name: format!("tunnel {n}"),
                host: "web".into(),
                forwards: vec![Forward {
                    kind: ForwardKind::L,
                    bind: bind.to_string(),
                    listen: 15432,
                    target: Some(target.to_string()),
                }],
                auto_reconnect: false,
            });
        }
        let r = Redactor::build(Sources {
            set: Some(&set),
            hosts: Some(&hosts),
            tunnels: &file,
            home: Path::new("/home/jdoe"),
            local_user: Some("jdoe"),
        });
        (r, hide)
    }

    /// The text as the UI would label it: tokens as `<kind>`, aliases as
    /// `{host N}`.
    fn shown((r, hide): &(Redactor, bool), text: &str) -> String {
        r.segments(text, *hide)
            .into_iter()
            .map(|s| match s {
                Segment::Plain(p) => p.to_string(),
                Segment::Token(kind, _) => format!("<{}>", kind.label()),
                Segment::Alias(_, n) => format!("{{host {n}}}"),
            })
            .collect()
    }

    const CONFIG: &str = "Host web\n    HostName web.prod.example.com\n    User deploy\n    Port 2222\n    IdentityFile ~/.ssh/id_web\n    ProxyJump ops@jump.example.net:2200\n\
Host db\n    HostName 10.0.4.12\n    ProxyCommand ssh -W %h:%p gw.example.org\n    LocalForward 127.0.0.1:5432 db.internal:5432\n\
Host 203.0.113.30\n    User root\n\
Host same\n    HostName same\n";

    #[test]
    fn every_kind_is_found_and_the_sentence_still_reads() {
        let r = redactor(CONFIG, &[("127.0.0.2", "vault.internal:8200")], false);
        assert_eq!(
            shown(
                &r,
                "ssh: connect to host web.prod.example.com port 2222: Connection refused"
            ),
            "ssh: connect to host <host> port <port>: Connection refused"
        );
        assert_eq!(
            shown(&r, "deploy@10.0.4.12: Permission denied"),
            "<user>@<ip>: Permission denied"
        );
        assert_eq!(shown(&r, "via ops@jump.example.net"), "via <user>@<host>");
        assert_eq!(shown(&r, "gw.example.org refused"), "<host> refused");
        assert_eq!(
            shown(&r, "to db.internal:5432 and vault.internal:8200"),
            "to <host>:<port> and <host>:<port>"
        );
        assert_eq!(shown(&r, "bind 127.0.0.2 failed"), "bind <ip> failed");
        assert_eq!(shown(&r, "logged in as root"), "logged in as <user>");
        // The local login and the home folder (any separator, any case).
        assert_eq!(shown(&r, "user jdoe"), "user <user>");
        assert_eq!(shown(&r, "key /home/jdoe/.ssh/id_x."), "key <path>.");
        assert_eq!(shown(&r, "key \\HOME\\JDOE\\.ssh\\id_x"), "key <path>");
        assert_eq!(shown(&r, "file ~/.ssh/known_hosts"), "file <path>");
        // Words that only contain a value stay.
        assert_eq!(shown(&r, "redeployment rootless"), "redeployment rootless");
    }

    #[test]
    fn the_longest_match_wins_and_nothing_is_left_behind() {
        let r = redactor(CONFIG, &[], false);
        assert_eq!(shown(&r, "10.0.4.12"), "<ip>");
        assert_eq!(shown(&r, "10.0.4.1 and 10.0.4.12."), "<ip> and <ip>.");
        // A path under home is one token, not home plus a rest.
        assert_eq!(shown(&r, "/home/jdoe/.ssh/config:3"), "<path>:3");
        assert_eq!(shown(&r, "x.web.prod.example.com"), "x.<host>");
    }

    #[test]
    fn ipv6_forms_and_lookalikes() {
        let r = redactor("", &[], false);
        assert_eq!(shown(&r, "to 2001:db8::1 port 22"), "to <ip> port <port>");
        assert_eq!(shown(&r, "[fe80::1%eth0]:22"), "[<ip>]:<port>");
        assert_eq!(shown(&r, "::1 and ::ffff:192.0.2.1"), "<ip> and <ip>");
        assert_eq!(shown(&r, "2001:0db8:0000:0000:0000:0000:0000:0001"), "<ip>");
        // Times, counts and MACs are not addresses or ports.
        assert_eq!(shown(&r, "14:02:11 after 22 min"), "14:02:11 after 22 min");
        assert_eq!(shown(&r, "aa:bb:cc:dd:ee:ff"), "aa:bb:cc:dd:ee:ff");
        assert_eq!(shown(&r, "version 1.2.3.4.5"), "version 1.2.3.4.5");
        assert_eq!(shown(&r, "999.1.1.1"), "999.1.1.1");
    }

    #[test]
    fn ports_only_in_context() {
        let r = redactor("", &[], false);
        assert_eq!(shown(&r, "Port 22, port 443"), "Port <port>, port <port>");
        assert_eq!(shown(&r, "ssh -p 2222 host"), "ssh -p <port> host");
        assert_eq!(shown(&r, "192.0.2.4:8080"), "<ip>:<port>");
        assert_eq!(shown(&r, "12:30 and :8080 alone"), "12:30 and :8080 alone");
    }

    #[test]
    fn fingerprints_and_public_keys() {
        let r = redactor("", &[], false);
        assert_eq!(
            shown(
                &r,
                "ED25519 SHA256:k3VqT8rW1xYb0nP4sLmE2cQ9dFgHjK7lMnOp6rStUvW"
            ),
            "ED25519 SHA256:<key>"
        );
        assert_eq!(
            shown(&r, "MD5:1a:2b:3c:4d:5e:6f:70:81:92:a3:b4:c5:d6:e7:f8:09"),
            "MD5:<key>"
        );
        assert_eq!(
            shown(
                &r,
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKk3VqT8rW1xYb0nP4sLmE2cQ9dFgHjK7lMn me@x"
            ),
            "ssh-ed25519 <key> me@x"
        );
    }

    #[test]
    fn aliases_stay_unless_hidden_or_always_masked() {
        let r = redactor(CONFIG, &[], false);
        assert_eq!(shown(&r, "web and db failed"), "web and db failed");
        let (built, _) = &r;
        assert_eq!(built.alias_number("web"), Some(1));
        assert_eq!(built.alias_number("db"), Some(2));
        assert_eq!(built.alias_number("203.0.113.30"), Some(3));
        // An alias that is an IP, or equals its HostName, is always masked.
        assert!(built.alias_always_masked("203.0.113.30"));
        assert!(built.alias_always_masked("same"));
        assert!(!built.alias_always_masked("web"));
        // A ProxyJump to an alias doesn't make the alias a value.
        let jump = redactor_with("Host a\n    ProxyJump b\nHost b\n    HostName b.example.com\n");
        assert_eq!(shown(&(jump, false), "via b"), "via b");

        let hidden = redactor(CONFIG, &[], true);
        assert_eq!(
            shown(&hidden, "web and db failed"),
            "{host 1} and {host 2} failed"
        );
        assert_eq!(shown(&hidden, "webhook"), "webhook");
    }

    fn redactor_with(config: &str) -> Redactor {
        redactor(config, &[], false).0
    }

    #[test]
    fn numbering_follows_the_config_and_stays_until_it_changes() {
        let first = redactor_with(CONFIG);
        let again = redactor_with(CONFIG);
        assert_eq!(first, again, "the same config gives the same redactor");
        let moved = redactor_with(&format!("Host new\n    HostName n.example.com\n{CONFIG}"));
        assert_eq!(moved.alias_number("new"), Some(1));
        assert_eq!(moved.alias_number("web"), Some(2));
    }

    #[test]
    fn plain_masking_for_titles_and_native_menus() {
        let (r, _) = redactor(CONFIG, &[], false);
        assert_eq!(
            r.mask_plain("deploy@10.0.4.12 :22", false),
            format!("{DOTS}@{DOTS} :22")
        );
        assert_eq!(r.mask_plain("web is up", true), "host 1 is up");
    }

    #[test]
    fn finds_tells_harmless_values_apart() {
        let (r, _) = redactor(CONFIG, &[], false);
        assert!(r.finds("10.0.4.12"));
        assert!(r.finds("ssh to deploy@x"));
        assert!(!r.finds("30"));
        assert!(!r.finds("no"));
        // Aliases are not sensitive by themselves.
        assert!(!r.finds("web"));
    }

    #[test]
    fn tunnel_binds_and_unicode_text() {
        let r = redactor("", &[(DEFAULT_BIND, "localhost:80")], false);
        assert_eq!(shown(&r, "listening on 127.0.0.1"), "listening on <ip>");
        assert_eq!(shown(&r, "→ localhost:80 ✓"), "→ <host>:<port> ✓");
    }
}
