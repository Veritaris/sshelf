//! `history.toml`: sessions launched from sshelf, newest first. Sessions
//! started outside the app cannot be tracked (SPEC.md §10).

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sshconf::HostEntry;

use crate::storage::Versioned;

/// Entries kept in `history.toml`.
pub const HISTORY_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub alias: String,
    /// RFC 3339, UTC, whole seconds.
    pub at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    #[serde(rename = "entry")]
    pub entries: Vec<Launch>,
}

impl Versioned for History {
    const FILE: &'static str = "history.toml";
    const VERSION: u32 = 1;
}

impl History {
    pub fn record(&mut self, alias: &str, at: SystemTime) {
        self.entries.insert(
            0,
            Launch {
                alias: alias.to_string(),
                at: humantime::format_rfc3339_seconds(at).to_string(),
            },
        );
        self.entries.truncate(HISTORY_LIMIT);
    }

    /// When `host` (under any of its aliases) was last launched from sshelf.
    pub fn last_launch(&self, host: &HostEntry) -> Option<SystemTime> {
        self.entries
            .iter()
            .filter(|e| host.aliases.contains(&e.alias))
            .find_map(|e| humantime::parse_rfc3339(&e.at).ok())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::metadata::tests::host;
    use crate::storage::Store;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn record_is_newest_first_and_capped() {
        let mut history = History::default();
        for i in 0..(HISTORY_LIMIT as u64 + 5) {
            history.record(&format!("h{i}"), at(1_700_000_000 + i));
        }
        assert_eq!(history.entries.len(), HISTORY_LIMIT);
        assert_eq!(history.entries[0].alias, format!("h{}", HISTORY_LIMIT + 4));
        assert_eq!(history.entries[0].at, "2023-11-14T22:14:14Z");
    }

    #[test]
    fn last_launch_matches_any_alias() {
        let mut history = History::default();
        history.record("db1", at(100));
        history.record("web", at(200));
        history.record("prod-db", at(300));
        history.record("web", at(400));
        assert_eq!(
            history.last_launch(&host(&["prod-db", "db1"])),
            Some(at(300))
        );
        assert_eq!(history.last_launch(&host(&["web"])), Some(at(400)));
        assert_eq!(history.last_launch(&host(&["never"])), None);
    }

    #[test]
    fn file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let mut history = History::default();
        history.record("a", at(1));
        history.record("b", at(2));
        store.save(&history).unwrap();
        let text = std::fs::read_to_string(store.path::<History>()).unwrap();
        assert!(text.contains("[[entry]]"), "{text}");
        assert_eq!(store.load::<History>().data, history);
    }
}
