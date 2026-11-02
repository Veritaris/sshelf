//! `hosts.toml`: per-host tags, notes, favorite and color, keyed by alias.
//!
//! An entry links to a host through any of its aliases (SPEC.md §4); new
//! entries are keyed by the primary alias, existing keys are never renamed.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sshconf::HostEntry;

use crate::storage::Versioned;

/// An sRGB color stored as `"#rrggbb"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color(pub [u8; 3]);

impl Color {
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self([byte(0)?, byte(2)?, byte(4)?]))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}")
    }
}

impl TryFrom<String> for Color {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        Self::parse(&text).ok_or_else(|| format!("invalid color {text:?}, expected \"#rrggbb\""))
    }
}

impl From<Color> for String {
    fn from(color: Color) -> String {
        color.to_string()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostMeta {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub favorite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}

impl HostMeta {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostsFile {
    pub hosts: BTreeMap<String, HostMeta>,
}

impl Versioned for HostsFile {
    const FILE: &'static str = "hosts.toml";
    const VERSION: u32 = 1;
}

impl HostsFile {
    /// The key holding `host`'s metadata: its primary alias if present, else
    /// the first of its other aliases that has an entry.
    pub fn key_for<'a>(&self, host: &'a HostEntry) -> Option<&'a str> {
        host.aliases
            .iter()
            .map(String::as_str)
            .find(|alias| self.hosts.contains_key(*alias))
    }

    pub fn get(&self, host: &HostEntry) -> Option<&HostMeta> {
        self.key_for(host).and_then(|key| self.hosts.get(key))
    }

    /// Stores `meta` under the host's existing key (or its primary alias);
    /// empty metadata removes the entry.
    pub fn set(&mut self, host: &HostEntry, meta: HostMeta) {
        let key = self.key_for(host).unwrap_or(host.primary()).to_string();
        if meta.is_empty() {
            self.hosts.remove(&key);
        } else {
            self.hosts.insert(key, meta);
        }
    }

    /// Every distinct tag, sorted.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .hosts
            .values()
            .flat_map(|m| m.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use sshconf::LineRef;

    use super::*;
    use crate::storage::{LoadStatus, Store};

    pub(crate) fn host(aliases: &[&str]) -> HostEntry {
        HostEntry {
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            at: LineRef { file: 0, line: 0 },
            also_in: Vec::new(),
            conditional: false,
            has_wildcards: false,
        }
    }

    fn meta(note: &str) -> HostMeta {
        HostMeta {
            notes: note.into(),
            ..HostMeta::default()
        }
    }

    #[test]
    fn color_parsing() {
        assert_eq!(Color::parse("#e06c75"), Some(Color([0xe0, 0x6c, 0x75])));
        assert_eq!(Color::parse("#E06C75").unwrap().to_string(), "#e06c75");
        for bad in ["e06c75", "#e06c7", "#e06c7g", "#ééé", ""] {
            assert_eq!(Color::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn file_round_trip_skips_empty_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let mut file = HostsFile::default();
        file.hosts.insert(
            "web.prod".into(),
            HostMeta {
                tags: vec!["prod".into(), "web".into()],
                notes: "line 1\nline 2".into(),
                favorite: true,
                color: Color::parse("#98c379"),
            },
        );
        file.hosts.insert("db".into(), meta("only notes"));
        store.save(&file).unwrap();

        let text = std::fs::read_to_string(store.path::<HostsFile>()).unwrap();
        assert!(text.starts_with("version = 1\n"), "{text}");
        assert!(!text.contains("favorite = false"), "{text}");
        assert_eq!(store.load::<HostsFile>().data, file);
    }

    #[test]
    fn invalid_color_makes_the_file_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        std::fs::write(
            store.path::<HostsFile>(),
            "version = 1\n[hosts.a]\ncolor = \"red\"\n",
        )
        .unwrap();
        assert!(matches!(
            store.load::<HostsFile>().status,
            LoadStatus::ReadOnly(_)
        ));
    }

    #[test]
    fn lookup_by_any_alias_prefers_the_primary() {
        let mut file = HostsFile::default();
        let h = host(&["prod-db", "db1"]);
        assert_eq!(file.get(&h), None);

        file.hosts.insert("db1".into(), meta("by second alias"));
        assert_eq!(file.key_for(&h), Some("db1"));
        assert_eq!(file.get(&h).unwrap().notes, "by second alias");

        file.hosts.insert("prod-db".into(), meta("by primary"));
        assert_eq!(file.get(&h).unwrap().notes, "by primary");
    }

    #[test]
    fn set_keeps_the_existing_key_and_removes_empty_entries() {
        let mut file = HostsFile::default();
        let h = host(&["prod-db", "db1"]);
        file.hosts.insert("db1".into(), meta("old"));
        file.set(&h, meta("new"));
        assert_eq!(file.hosts.keys().collect::<Vec<_>>(), ["db1"]);
        assert_eq!(file.hosts["db1"].notes, "new");

        file.set(&h, HostMeta::default());
        assert!(file.hosts.is_empty());

        file.set(&h, meta("fresh"));
        assert_eq!(file.hosts.keys().collect::<Vec<_>>(), ["prod-db"]);
    }

    #[test]
    fn all_tags_are_sorted_and_unique() {
        let mut file = HostsFile::default();
        for (alias, tags) in [("a", vec!["web", "prod"]), ("b", vec!["prod", "db"])] {
            file.hosts.insert(
                alias.into(),
                HostMeta {
                    tags: tags.into_iter().map(String::from).collect(),
                    ..HostMeta::default()
                },
            );
        }
        assert_eq!(file.all_tags(), ["db", "prod", "web"]);
    }
}
