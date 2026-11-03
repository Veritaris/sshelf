//! Renaming a host alias (SPEC.md §5): which names are allowed, and the app
//! data that follows the alias — metadata, tunnels and history.

use sshconf::HostList;

use crate::history::History;
use crate::metadata::HostsFile;
use crate::tunnels::TunnelsFile;

/// What a rename changes besides the config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Renamed {
    pub metadata: bool,
    pub tunnels: usize,
    pub history: usize,
}

impl Renamed {
    /// E.g. "metadata, 2 tunnels and 3 history entries"; `None` if nothing.
    pub fn describe(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.metadata {
            parts.push("metadata".to_string());
        }
        match self.tunnels {
            0 => {}
            1 => parts.push("1 tunnel".into()),
            n => parts.push(format!("{n} tunnels")),
        }
        match self.history {
            0 => {}
            1 => parts.push("1 history entry".into()),
            n => parts.push(format!("{n} history entries")),
        }
        let last = parts.pop()?;
        Some(if parts.is_empty() {
            last
        } else {
            format!("{} and {last}", parts.join(", "))
        })
    }
}

/// Why `to` cannot be the new alias: it must be one plain word, not a
/// pattern, and not an alias some host already has.
pub fn check_new_alias(to: &str, hosts: &HostList) -> Result<(), String> {
    if to.is_empty() {
        return Err("the new name is empty".into());
    }
    if to.contains(char::is_whitespace) {
        return Err("a host name cannot contain spaces".into());
    }
    if to.contains(['*', '?', '!', ',', '"', '\'']) {
        return Err("a host name cannot contain * ? ! , or quotes".into());
    }
    if to.starts_with(['-', '#']) {
        return Err("a host name cannot start with - or #".into());
    }
    if hosts.find(to).is_some() {
        return Err(format!("`{to}` is already a host"));
    }
    Ok(())
}

/// Moves everything stored under `from` to `to`.
pub fn rename_in(
    hosts: &mut HostsFile,
    tunnels: &mut TunnelsFile,
    history: &mut History,
    from: &str,
    to: &str,
) -> Renamed {
    let mut renamed = Renamed::default();
    if let Some(meta) = hosts.hosts.remove(from) {
        hosts.hosts.insert(to.to_string(), meta);
        renamed.metadata = true;
    }
    for tunnel in tunnels.tunnels.iter_mut().filter(|t| t.host == from) {
        tunnel.host = to.to_string();
        renamed.tunnels += 1;
    }
    for entry in history.entries.iter_mut().filter(|e| e.alias == from) {
        entry.alias = to.to_string();
        renamed.history += 1;
    }
    renamed
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use sshconf::{ConfigSet, ResolveOptions};

    use super::*;
    use crate::metadata::HostMeta;
    use crate::tunnels::Tunnel;

    fn hosts(text: &str) -> HostList {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        std::fs::write(&config, text).unwrap();
        let set = ConfigSet::load(&config, &ResolveOptions::new(tmp.path())).unwrap();
        HostList::from_set(&set)
    }

    #[test]
    fn new_names_must_be_plain_and_unused() {
        let list = hosts("Host web w1\nHost *.corp\n");
        assert_eq!(check_new_alias("web2", &list), Ok(()));
        assert_eq!(check_new_alias("x.corp", &list), Ok(()));
        for bad in [
            "", "a b", "a*", "?", "!a", "a,b", "-a", "#a", "\"a\"", "w1", "web",
        ] {
            assert!(check_new_alias(bad, &list).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn data_follows_the_alias() {
        let mut meta = HostsFile::default();
        meta.hosts.insert(
            "web".into(),
            HostMeta {
                notes: "n".into(),
                ..HostMeta::default()
            },
        );
        meta.hosts.insert("db".into(), HostMeta::default());
        let mut tunnels = TunnelsFile::default();
        for (id, host) in [("t-1", "web"), ("t-2", "db"), ("t-3", "web")] {
            tunnels.tunnels.push(Tunnel {
                id: id.into(),
                name: id.into(),
                host: host.into(),
                forwards: Vec::new(),
                auto_reconnect: false,
            });
        }
        let mut history = History::default();
        history.record("web", SystemTime::now());
        history.record("db", SystemTime::now());

        let renamed = rename_in(&mut meta, &mut tunnels, &mut history, "web", "web2");
        assert_eq!(
            renamed,
            Renamed {
                metadata: true,
                tunnels: 2,
                history: 1
            }
        );
        assert_eq!(meta.hosts["web2"].notes, "n");
        assert!(!meta.hosts.contains_key("web"));
        let hosts: Vec<_> = tunnels.tunnels.iter().map(|t| t.host.as_str()).collect();
        assert_eq!(hosts, ["web2", "db", "web2"]);
        assert_eq!(history.entries[1].alias, "web2");
        assert_eq!(
            renamed.describe().as_deref(),
            Some("metadata, 2 tunnels and 1 history entry")
        );
        assert_eq!(Renamed::default().describe(), None);
    }
}
