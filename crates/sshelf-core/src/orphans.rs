//! Orphans (SPEC.md §6): app data that no longer links to a host. They are
//! surfaced to the user and never deleted automatically.
//!
//! M2 covers `hosts.toml`; tunnels and groups join in M3.

use sshconf::HostList;

use crate::metadata::HostsFile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanReason {
    /// No host has this alias any more.
    MissingAlias,
    /// The alias resolves to `host`, but that host's metadata is stored under
    /// another of its aliases (`used`), so this entry is ignored.
    Shadowed { host: String, used: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    /// Key in `hosts.toml`.
    pub key: String,
    pub reason: OrphanReason,
}

impl Orphan {
    /// One-line description for tooltips.
    pub fn describe(&self) -> String {
        match &self.reason {
            OrphanReason::MissingAlias => format!("metadata for `{}`: no such host", self.key),
            OrphanReason::Shadowed { host, used } => format!(
                "metadata for `{}`: host `{host}` already uses the entry for `{used}`",
                self.key
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelinkError {
    #[error("no metadata entry `{0}`")]
    NoEntry(String),
    #[error("no host has the alias `{0}`")]
    UnknownAlias(String),
    #[error("host `{0}` already has metadata")]
    TargetHasMetadata(String),
}

pub fn find_orphans(meta: &HostsFile, hosts: &HostList) -> Vec<Orphan> {
    meta.hosts
        .keys()
        .filter_map(|key| {
            let reason = match hosts.find(key) {
                None => OrphanReason::MissingAlias,
                Some(host) => match meta.key_for(host) {
                    Some(used) if used != key => OrphanReason::Shadowed {
                        host: host.primary().to_string(),
                        used: used.to_string(),
                    },
                    _ => return None,
                },
            };
            Some(Orphan {
                key: key.clone(),
                reason,
            })
        })
        .collect()
}

/// Hosts an orphan can be relinked to: those without metadata of their own,
/// by primary alias. Relinking never merges or overwrites.
pub fn relink_targets(meta: &HostsFile, hosts: &HostList) -> Vec<String> {
    hosts
        .hosts
        .iter()
        .filter(|h| meta.get(h).is_none())
        .map(|h| h.primary().to_string())
        .collect()
}

/// Moves the entry `from` to the host owning `to` (stored under its primary alias).
pub fn relink(
    meta: &mut HostsFile,
    hosts: &HostList,
    from: &str,
    to: &str,
) -> Result<(), RelinkError> {
    if !meta.hosts.contains_key(from) {
        return Err(RelinkError::NoEntry(from.to_string()));
    }
    let host = hosts
        .find(to)
        .ok_or_else(|| RelinkError::UnknownAlias(to.to_string()))?;
    if meta.get(host).is_some() {
        return Err(RelinkError::TargetHasMetadata(host.primary().to_string()));
    }
    let entry = meta.hosts.remove(from).expect("checked above");
    meta.hosts.insert(host.primary().to_string(), entry);
    Ok(())
}

/// Removes an entry on the user's explicit request; `false` if it did not exist.
pub fn delete(meta: &mut HostsFile, key: &str) -> bool {
    meta.hosts.remove(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::HostMeta;
    use crate::metadata::tests::host;

    fn hosts(list: &[&[&str]]) -> HostList {
        HostList {
            hosts: list.iter().map(|aliases| host(aliases)).collect(),
            patterns: Vec::new(),
        }
    }

    fn meta(keys: &[&str]) -> HostsFile {
        let mut file = HostsFile::default();
        for key in keys {
            file.hosts.insert(
                key.to_string(),
                HostMeta {
                    notes: format!("notes for {key}"),
                    ..HostMeta::default()
                },
            );
        }
        file
    }

    #[test]
    fn detects_missing_and_shadowed_entries() {
        let hosts = hosts(&[&["prod-db", "db1"], &["web"]]);
        let meta = meta(&["db1", "gone", "prod-db", "web"]);
        let orphans = find_orphans(&meta, &hosts);
        assert_eq!(
            orphans,
            [
                Orphan {
                    key: "db1".into(),
                    reason: OrphanReason::Shadowed {
                        host: "prod-db".into(),
                        used: "prod-db".into()
                    },
                },
                Orphan {
                    key: "gone".into(),
                    reason: OrphanReason::MissingAlias,
                },
            ]
        );
        assert!(orphans[1].describe().contains("no such host"));
    }

    #[test]
    fn entry_under_a_secondary_alias_is_not_an_orphan() {
        let hosts = hosts(&[&["prod-db", "db1"]]);
        assert!(find_orphans(&meta(&["db1"]), &hosts).is_empty());
    }

    #[test]
    fn relink_targets_are_hosts_without_metadata() {
        let hosts = hosts(&[&["a"], &["b", "b2"], &["c"]]);
        let meta = meta(&["b2", "gone"]);
        assert_eq!(relink_targets(&meta, &hosts), ["a", "c"]);
    }

    #[test]
    fn relink_moves_the_entry_to_the_primary_alias() {
        let hosts = hosts(&[&["new-name", "alt"]]);
        let mut meta = meta(&["old-name"]);
        relink(&mut meta, &hosts, "old-name", "alt").unwrap();
        assert_eq!(meta.hosts.keys().collect::<Vec<_>>(), ["new-name"]);
        assert_eq!(meta.hosts["new-name"].notes, "notes for old-name");
        assert!(find_orphans(&meta, &hosts).is_empty());
    }

    #[test]
    fn relink_never_overwrites() {
        let hosts = hosts(&[&["a"]]);
        let mut meta = meta(&["a", "gone"]);
        let before = meta.clone();
        assert_eq!(
            relink(&mut meta, &hosts, "gone", "a"),
            Err(RelinkError::TargetHasMetadata("a".into()))
        );
        assert_eq!(
            relink(&mut meta, &hosts, "gone", "nope"),
            Err(RelinkError::UnknownAlias("nope".into()))
        );
        assert_eq!(
            relink(&mut meta, &hosts, "missing", "a"),
            Err(RelinkError::NoEntry("missing".into()))
        );
        assert_eq!(meta, before);
    }

    #[test]
    fn delete_removes_only_the_named_entry() {
        let mut meta = meta(&["a", "gone"]);
        assert!(delete(&mut meta, "gone"));
        assert!(!delete(&mut meta, "gone"));
        assert_eq!(meta.hosts.keys().collect::<Vec<_>>(), ["a"]);
    }
}
