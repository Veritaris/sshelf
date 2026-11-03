//! Orphans (SPEC.md §6): app data that no longer links to what it refers to:
//! metadata or tunnels whose host alias is gone, and groups listing missing
//! tunnels. They are surfaced to the user and never deleted automatically.

use sshconf::HostList;

use crate::metadata::HostsFile;
use crate::tunnels::TunnelsFile;

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
    #[error("no tunnel `{0}`")]
    UnknownTunnel(String),
    #[error("no group `{0}`")]
    UnknownGroup(String),
    #[error("the group already contains tunnel `{0}`")]
    AlreadyInGroup(String),
}

/// A tunnel whose host alias is not in the ssh config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelOrphan {
    pub tunnel_id: String,
    pub name: String,
    pub host: String,
}

impl TunnelOrphan {
    pub fn describe(&self) -> String {
        format!("tunnel \"{}\": no host `{}`", self.name, self.host)
    }
}

/// A group entry naming a tunnel that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupOrphan {
    pub group_id: String,
    pub group_name: String,
    pub tunnel_id: String,
}

impl GroupOrphan {
    pub fn describe(&self) -> String {
        format!(
            "group \"{}\": no tunnel `{}`",
            self.group_name, self.tunnel_id
        )
    }
}

pub fn find_tunnel_orphans(tunnels: &TunnelsFile, hosts: &HostList) -> Vec<TunnelOrphan> {
    tunnels
        .tunnels
        .iter()
        .filter(|t| hosts.find(&t.host).is_none())
        .map(|t| TunnelOrphan {
            tunnel_id: t.id.clone(),
            name: t.name.clone(),
            host: t.host.clone(),
        })
        .collect()
}

pub fn find_group_orphans(tunnels: &TunnelsFile) -> Vec<GroupOrphan> {
    tunnels
        .groups
        .iter()
        .flat_map(|g| {
            g.tunnels
                .iter()
                .filter(|id| tunnels.tunnel(id).is_none())
                .map(|id| GroupOrphan {
                    group_id: g.id.clone(),
                    group_name: g.name.clone(),
                    tunnel_id: id.clone(),
                })
        })
        .collect()
}

/// Points a tunnel at the host owning `to` (any host; tunnels may share one).
pub fn relink_tunnel(
    tunnels: &mut TunnelsFile,
    hosts: &HostList,
    tunnel_id: &str,
    to: &str,
) -> Result<(), RelinkError> {
    let host = hosts
        .find(to)
        .ok_or_else(|| RelinkError::UnknownAlias(to.to_string()))?;
    let tunnel = tunnels
        .tunnels
        .iter_mut()
        .find(|t| t.id == tunnel_id)
        .ok_or_else(|| RelinkError::UnknownTunnel(tunnel_id.to_string()))?;
    tunnel.host = host.primary().to_string();
    Ok(())
}

/// Replaces a group's reference to `from` with the existing tunnel `to`.
pub fn relink_group_member(
    tunnels: &mut TunnelsFile,
    group_id: &str,
    from: &str,
    to: &str,
) -> Result<(), RelinkError> {
    if tunnels.tunnel(to).is_none() {
        return Err(RelinkError::UnknownTunnel(to.to_string()));
    }
    let group = tunnels
        .groups
        .iter_mut()
        .find(|g| g.id == group_id)
        .ok_or_else(|| RelinkError::UnknownGroup(group_id.to_string()))?;
    if group.tunnels.iter().any(|t| t == to) {
        return Err(RelinkError::AlreadyInGroup(to.to_string()));
    }
    let slot = group
        .tunnels
        .iter_mut()
        .find(|t| *t == from)
        .ok_or_else(|| RelinkError::UnknownTunnel(from.to_string()))?;
    *slot = to.to_string();
    Ok(())
}

/// Drops a group's reference to `tunnel_id`; `false` if there was none.
pub fn remove_group_member(tunnels: &mut TunnelsFile, group_id: &str, tunnel_id: &str) -> bool {
    let Some(group) = tunnels.groups.iter_mut().find(|g| g.id == group_id) else {
        return false;
    };
    let before = group.tunnels.len();
    group.tunnels.retain(|t| t != tunnel_id);
    group.tunnels.len() != before
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

    fn tunnels_file() -> TunnelsFile {
        use crate::tunnels::{Group, Tunnel};
        let tunnel = |id: &str, host: &str| Tunnel {
            id: id.into(),
            name: format!("tunnel {id}"),
            host: host.into(),
            forwards: Vec::new(),
            auto_reconnect: false,
        };
        TunnelsFile {
            tunnels: vec![tunnel("t1", "db1"), tunnel("t2", "gone-host")],
            groups: vec![Group {
                id: "g".into(),
                name: "Work".into(),
                tunnels: vec!["t1".into(), "t-missing".into()],
                start_on_launch: false,
            }],
        }
    }

    #[test]
    fn tunnel_and_group_orphans() {
        let hosts = hosts(&[&["prod-db", "db1"], &["web"]]);
        let file = tunnels_file();
        let tunnel_orphans = find_tunnel_orphans(&file, &hosts);
        assert_eq!(tunnel_orphans.len(), 1);
        assert_eq!(tunnel_orphans[0].tunnel_id, "t2");
        assert!(tunnel_orphans[0].describe().contains("gone-host"));
        let group_orphans = find_group_orphans(&file);
        assert_eq!(group_orphans.len(), 1);
        assert_eq!(group_orphans[0].tunnel_id, "t-missing");
        assert!(group_orphans[0].describe().contains("Work"));
    }

    #[test]
    fn relink_tunnel_to_any_host_by_primary_alias() {
        let hosts = hosts(&[&["prod-db", "db1"], &["web"]]);
        let mut file = tunnels_file();
        relink_tunnel(&mut file, &hosts, "t2", "db1").unwrap();
        assert_eq!(file.tunnel("t2").unwrap().host, "prod-db");
        assert_eq!(
            relink_tunnel(&mut file, &hosts, "t2", "nope"),
            Err(RelinkError::UnknownAlias("nope".into()))
        );
        assert_eq!(
            relink_tunnel(&mut file, &hosts, "t9", "web"),
            Err(RelinkError::UnknownTunnel("t9".into()))
        );
        assert!(find_tunnel_orphans(&file, &hosts).is_empty());
    }

    #[test]
    fn group_member_relink_and_removal() {
        let mut file = tunnels_file();
        assert_eq!(
            relink_group_member(&mut file, "g", "t-missing", "t1"),
            Err(RelinkError::AlreadyInGroup("t1".into()))
        );
        assert_eq!(
            relink_group_member(&mut file, "g", "t-missing", "t9"),
            Err(RelinkError::UnknownTunnel("t9".into()))
        );
        relink_group_member(&mut file, "g", "t-missing", "t2").unwrap();
        assert_eq!(file.groups[0].tunnels, ["t1", "t2"]);
        assert!(find_group_orphans(&file).is_empty());

        assert!(remove_group_member(&mut file, "g", "t2"));
        assert!(!remove_group_member(&mut file, "g", "t2"));
        assert!(!remove_group_member(&mut file, "nope", "t1"));
        assert_eq!(file.groups[0].tunnels, ["t1"]);
    }
}
