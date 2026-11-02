//! Host list semantics (SPEC.md §4): blocks with concrete aliases are hosts;
//! wildcard-only, negation-only and `Match` blocks are patterns.
//!
//! Every concrete alias belongs to exactly one host: the first block (in ssh's
//! evaluation order) that names it. A later block naming an already-owned alias
//! does not create a second entry for it; it is recorded in the owner's
//! [`HostEntry::also_in`] (ssh merges such blocks, first value wins). A block
//! mixing concrete aliases and wildcards (`Host web1 *.corp`) is a host with
//! only its concrete aliases and `has_wildcards` set.

use std::collections::{HashMap, HashSet};

use crate::include::{ConfigSet, LineRef};
use crate::line::{HostPattern, LineKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// Concrete aliases this host owns, in written order: the block's
    /// non-wildcard, non-negated patterns not owned by an earlier block.
    pub aliases: Vec<String>,
    /// The `Host` line that defines the entry.
    pub at: LineRef,
    /// Later `Host` lines that also name one of [`Self::aliases`].
    pub also_in: Vec<LineRef>,
    /// Reached through an `Include` inside a `Host`/`Match` block, so ssh
    /// applies it only when the enclosing block matches too.
    pub conditional: bool,
    /// The defining block also has wildcard patterns.
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
}

impl HostList {
    pub fn from_set(set: &ConfigSet) -> Self {
        let mut list = HostList::default();
        // A file included twice yields its headers twice; list each block once.
        let mut seen = HashSet::new();
        // Alias → index into `list.hosts`.
        let mut owners: HashMap<String, usize> = HashMap::new();

        for header in &set.headers {
            if !seen.insert(header.at) {
                continue;
            }
            let conditional = !header.conditions.is_empty();
            match &set.line(header.at).kind {
                LineKind::HostHeader(h) => {
                    let mut concrete: Vec<&str> = Vec::new();
                    for p in h.patterns.iter().filter(|p| !p.negated && !p.is_wildcard()) {
                        if !concrete.contains(&p.pattern.as_str()) {
                            concrete.push(&p.pattern);
                        }
                    }
                    if concrete.is_empty() {
                        list.patterns.push(PatternEntry {
                            at: header.at,
                            conditional,
                            kind: PatternKind::Host(h.patterns.clone()),
                        });
                        continue;
                    }

                    let mut fresh = Vec::new();
                    for alias in concrete {
                        match owners.get(alias) {
                            Some(&owner) => {
                                let also_in = &mut list.hosts[owner].also_in;
                                if also_in.last() != Some(&header.at) {
                                    also_in.push(header.at);
                                }
                            }
                            None => fresh.push(alias.to_string()),
                        }
                    }
                    if fresh.is_empty() {
                        continue;
                    }
                    let index = list.hosts.len();
                    for alias in &fresh {
                        owners.insert(alias.clone(), index);
                    }
                    list.hosts.push(HostEntry {
                        aliases: fresh,
                        at: header.at,
                        also_in: Vec::new(),
                        conditional,
                        has_wildcards: h.patterns.iter().any(|p| !p.negated && p.is_wildcard()),
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

    /// The host owning `alias`; matching is case-sensitive, like ssh's.
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

    /// Line numbers of `also_in` for each host.
    fn also_in(set: &ConfigSet, hosts: &HostList) -> Vec<Vec<usize>> {
        hosts
            .hosts
            .iter()
            .map(|h| h.also_in.iter().map(|&at| set.line(at).number).collect())
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
    fn repeated_block_merges_into_the_first() {
        let (set, hosts) = list(&[("config", "Host a\n  Port 1\nHost a\n  Port 2\n")]);
        assert_eq!(aliases(&hosts), [vec!["a"]]);
        assert_eq!(hosts.hosts[0].at.line, 0);
        assert_eq!(also_in(&set, &hosts), [vec![3]]);
    }

    #[test]
    fn later_block_keeps_only_its_new_aliases() {
        let (set, hosts) = list(&[("config", "Host a\nHost b a\nHost a c\nHost a b\n")]);
        assert_eq!(aliases(&hosts), [vec!["a"], vec!["b"], vec!["c"]]);
        assert_eq!(hosts.find("a").unwrap().at.line, 0);
        assert_eq!(hosts.find("b").unwrap().at.line, 1);
        // `Host a b` adds nothing new: it is recorded once on each owner.
        assert_eq!(also_in(&set, &hosts), [vec![2, 3, 4], vec![4], vec![]]);
    }

    #[test]
    fn alias_repeated_within_a_block_is_not_a_duplicate() {
        let (_, hosts) = list(&[("config", "Host a a b a\n")]);
        assert_eq!(aliases(&hosts), [vec!["a", "b"]]);
        assert!(hosts.hosts[0].also_in.is_empty());
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
        assert!(hosts.hosts[0].also_in.is_empty());
    }
}
