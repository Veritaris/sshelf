//! Host list semantics (SPEC.md §4): blocks with concrete aliases are hosts;
//! wildcard-only, negation-only and `Match` blocks are patterns.
//!
//! Each `Host` block is one entry. A block mixing concrete aliases and
//! wildcards (`Host web1 *.corp`) is a host with only its concrete aliases and
//! `has_wildcards` set. An alias defined by several blocks is reported; lookups
//! return the first block in ssh's evaluation order.

use std::collections::{HashMap, HashSet};

use crate::include::{ConfigSet, Diagnostic, LineRef, Severity};
use crate::line::{HostPattern, LineKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// Concrete (non-wildcard, non-negated) aliases in written order.
    pub aliases: Vec<String>,
    /// The `Host` line.
    pub at: LineRef,
    /// Reached through an `Include` inside a `Host`/`Match` block, so ssh
    /// applies it only when the enclosing block matches too.
    pub conditional: bool,
    /// The block also has wildcard patterns.
    pub has_wildcards: bool,
}

impl HostEntry {
    /// The alias app data is keyed by.
    pub fn primary(&self) -> &str {
        &self.aliases[0]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternKind {
    Host(Vec<HostPattern>),
    /// `Match` criteria words.
    Match(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternEntry {
    pub at: LineRef,
    pub conditional: bool,
    pub kind: PatternKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostList {
    pub hosts: Vec<HostEntry>,
    pub patterns: Vec<PatternEntry>,
    /// Aliases defined by more than one block.
    pub diagnostics: Vec<Diagnostic>,
}

impl HostList {
    pub fn from_set(set: &ConfigSet) -> Self {
        let mut list = HostList::default();
        // A file included twice yields its headers twice; list each block once.
        let mut seen = HashSet::new();
        let mut owners: HashMap<String, LineRef> = HashMap::new();

        for header in &set.headers {
            if !seen.insert(header.at) {
                continue;
            }
            let conditional = !header.conditions.is_empty();
            match &set.line(header.at).kind {
                LineKind::HostHeader(h) => {
                    let positive = h.patterns.iter().filter(|p| !p.negated);
                    let aliases: Vec<String> = positive
                        .clone()
                        .filter(|p| !p.is_wildcard())
                        .map(|p| p.pattern.clone())
                        .collect();
                    if aliases.is_empty() {
                        list.patterns.push(PatternEntry {
                            at: header.at,
                            conditional,
                            kind: PatternKind::Host(h.patterns.clone()),
                        });
                        continue;
                    }
                    for alias in &aliases {
                        match owners.get(alias) {
                            // Repeated within the same block (`Host a a`).
                            Some(&first) if first == header.at => {}
                            Some(&first) => list.diagnostics.push(Diagnostic {
                                severity: Severity::Info,
                                at: header.at,
                                message: format!(
                                    "`{alias}` is also defined at {}; ssh applies that block first",
                                    set.location(first)
                                ),
                            }),
                            None => {
                                owners.insert(alias.clone(), header.at);
                            }
                        }
                    }
                    list.hosts.push(HostEntry {
                        has_wildcards: positive.clone().any(HostPattern::is_wildcard),
                        aliases,
                        at: header.at,
                        conditional,
                    });
                }
                LineKind::MatchHeader(m) => list.patterns.push(PatternEntry {
                    at: header.at,
                    conditional,
                    kind: PatternKind::Match(m.args.clone()),
                }),
                _ => {}
            }
        }
        list
    }

    /// First host (in evaluation order) with `alias`; matching is
    /// case-sensitive, like ssh's.
    pub fn find(&self, alias: &str) -> Option<&HostEntry> {
        self.hosts
            .iter()
            .find(|h| h.aliases.iter().any(|a| a == alias))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::include::{MAX_INCLUDE_DEPTH, ResolveOptions};

    /// Loads `files` (name, contents) from a temp dir; the first is the main file.
    fn list(files: &[(&str, &str)]) -> (ConfigSet, HostList) {
        let tmp = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(tmp.path().join(name), text).unwrap();
        }
        let opts = ResolveOptions {
            home: tmp.path().to_path_buf(),
            ssh_dir: tmp.path().to_path_buf(),
            max_depth: MAX_INCLUDE_DEPTH,
        };
        let set = ConfigSet::load(&tmp.path().join(files[0].0), &opts).unwrap();
        let hosts = HostList::from_set(&set);
        (set, hosts)
    }

    fn aliases(hosts: &HostList) -> Vec<Vec<&str>> {
        hosts
            .hosts
            .iter()
            .map(|h| h.aliases.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn concrete_blocks_are_hosts_and_wildcards_are_patterns() {
        let (_, hosts) = list(&[(
            "config",
            "Host web\nHost prod-db db1\nHost *\nHost *.prod !bastion.prod\nHost !only\nMatch all\n",
        )]);
        assert_eq!(aliases(&hosts), [vec!["web"], vec!["prod-db", "db1"]]);
        assert_eq!(hosts.patterns.len(), 4);
        assert!(matches!(hosts.patterns[3].kind, PatternKind::Match(_)));
        assert_eq!(hosts.hosts[1].primary(), "prod-db");
    }

    #[test]
    fn mixed_block_keeps_concrete_aliases_and_flags_wildcards() {
        let (_, hosts) = list(&[("config", "Host web1 *.corp !bad web2\nHost plain\n")]);
        assert_eq!(aliases(&hosts), [vec!["web1", "web2"], vec!["plain"]]);
        assert!(hosts.hosts[0].has_wildcards);
        assert!(!hosts.hosts[1].has_wildcards);
        assert!(hosts.patterns.is_empty());
    }

    #[test]
    fn negated_wildcard_does_not_count_as_wildcard() {
        let (_, hosts) = list(&[("config", "Host app !*.local\n")]);
        assert_eq!(aliases(&hosts), [vec!["app"]]);
        assert!(!hosts.hosts[0].has_wildcards);
    }

    #[test]
    fn find_resolves_any_alias_case_sensitively() {
        let (_, hosts) = list(&[("config", "Host prod-db db1\nHost Other\n")]);
        assert_eq!(hosts.find("db1").unwrap().primary(), "prod-db");
        assert_eq!(hosts.find("prod-db").unwrap().primary(), "prod-db");
        assert!(hosts.find("other").is_none());
        assert!(hosts.find("Other").is_some());
        assert!(hosts.find("missing").is_none());
    }

    #[test]
    fn duplicate_alias_is_reported_and_first_block_wins() {
        let (set, hosts) = list(&[("config", "Host a\n  Port 1\nHost b a\n  Port 2\n")]);
        assert_eq!(aliases(&hosts), [vec!["a"], vec!["b", "a"]]);
        assert_eq!(hosts.find("a").unwrap().at.line, 0);
        assert_eq!(hosts.diagnostics.len(), 1);
        let d = &hosts.diagnostics[0];
        assert_eq!(set.line(d.at).number, 3);
        assert!(
            d.message.contains("`a` is also defined at"),
            "{}",
            d.message
        );
    }

    #[test]
    fn hosts_from_includes_carry_source_and_condition() {
        let (set, hosts) = list(&[
            (
                "config",
                "Include extra.conf\nHost bastion\n  Include work.conf\n",
            ),
            ("extra.conf", "Host extra\n"),
            ("work.conf", "Host w1\nMatch user root\n"),
        ]);
        assert_eq!(
            aliases(&hosts),
            [vec!["extra"], vec!["bastion"], vec!["w1"]]
        );
        let file_of = |h: &HostEntry| {
            set.files[h.at.file]
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let sources: Vec<_> = hosts.hosts.iter().map(file_of).collect();
        assert_eq!(sources, ["extra.conf", "config", "work.conf"]);
        let conditional: Vec<_> = hosts.hosts.iter().map(|h| h.conditional).collect();
        assert_eq!(conditional, [false, false, true]);
        assert!(hosts.patterns[0].conditional);
    }

    #[test]
    fn file_included_twice_lists_its_hosts_once() {
        let (_, hosts) = list(&[
            (
                "config",
                "Include shared.conf\nHost x\nInclude shared.conf\n",
            ),
            ("shared.conf", "Host shared\n"),
        ]);
        assert_eq!(aliases(&hosts), [vec!["shared"], vec!["x"]]);
        assert!(hosts.diagnostics.is_empty());
    }
}
