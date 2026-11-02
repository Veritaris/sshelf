//! Versioned TOML files in the app data dir (SPEC.md §3).
//!
//! Every file has a top-level `version`. Older files are migrated step by step
//! on the raw TOML (the original is backed up first); files with a newer,
//! missing or unreadable version are loaded read-only and never written. All
//! writes are atomic and confined to the app data dir.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upgrades the raw TOML of version `n` to version `n + 1`.
pub type Migration = fn(toml::Table) -> Result<toml::Table, String>;

/// A type stored as one versioned TOML file.
pub trait Versioned: Serialize + DeserializeOwned + Default {
    /// File name inside the app data dir.
    const FILE: &'static str;
    /// Schema version written by this build.
    const VERSION: u32;
    /// `migrations()[i]` upgrades version `i + 1` to `i + 2`; its length must
    /// be `VERSION - 1`.
    fn migrations() -> &'static [Migration] {
        &[]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    /// No file yet; defaults are used.
    Missing,
    Loaded,
    /// Upgraded from an older version and written back; the original is in `backup`.
    Migrated {
        from: u32,
        backup: PathBuf,
    },
    /// Shown but never written; the reason is user-facing.
    ReadOnly(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loaded<T> {
    pub data: T,
    pub status: LoadStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{} is read-only: {reason}", path.display())]
    ReadOnly { path: PathBuf, reason: String },
    #[error("refusing to write {} outside the app data dir", path.display())]
    OutsideAppDir { path: PathBuf },
    #[error("serializing {}: {source}", path.display())]
    Serialize {
        path: PathBuf,
        source: toml::ser::Error,
    },
    #[error("writing {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// The app data dir and which of its files are read-only this session.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    read_only: HashSet<&'static str>,
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    version: u32,
    #[serde(flatten)]
    data: &'a T,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            read_only: HashSet::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path<T: Versioned>(&self) -> PathBuf {
        self.dir.join(T::FILE)
    }

    /// Loads `T`, migrating older files. Never fails: problems make the file
    /// read-only and fall back to whatever could be read (or defaults).
    pub fn load<T: Versioned>(&mut self) -> Loaded<T> {
        debug_assert_eq!(T::migrations().len() as u32 + 1, T::VERSION);
        let path = self.path::<T>();
        let result = self.load_inner::<T>(&path);
        match &result.status {
            LoadStatus::ReadOnly(_) => self.read_only.insert(T::FILE),
            _ => self.read_only.remove(T::FILE),
        };
        result
    }

    fn load_inner<T: Versioned>(&self, path: &Path) -> Loaded<T> {
        let read_only = |reason: String| Loaded {
            data: T::default(),
            status: LoadStatus::ReadOnly(reason),
        };
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Loaded {
                    data: T::default(),
                    status: LoadStatus::Missing,
                };
            }
            Err(e) => return read_only(format!("cannot read {}: {e}", T::FILE)),
        };
        let mut table: toml::Table = match toml::from_str(&text) {
            Ok(table) => table,
            Err(e) => return read_only(format!("{} is not valid TOML: {e}", T::FILE)),
        };
        let version = match table.remove("version") {
            Some(toml::Value::Integer(v)) if v >= 1 => v as u64,
            Some(_) => {
                return read_only(format!("{}: `version` is not a positive integer", T::FILE));
            }
            None => return read_only(format!("{}: no `version` field", T::FILE)),
        };

        if version > T::VERSION as u64 {
            // Show what we understand; unknown fields are ignored.
            return Loaded {
                data: table.try_into().unwrap_or_default(),
                status: LoadStatus::ReadOnly(format!(
                    "{} has version {version}; this build understands up to {}",
                    T::FILE,
                    T::VERSION
                )),
            };
        }

        let from = version as u32;
        for (i, migrate) in T::migrations().iter().enumerate().skip(from as usize - 1) {
            table = match migrate(table) {
                Ok(table) => table,
                Err(e) => {
                    return read_only(format!(
                        "{}: migrating version {} to {} failed: {e}",
                        T::FILE,
                        i + 1,
                        i + 2
                    ));
                }
            };
        }
        let data: T = match table.try_into() {
            Ok(data) => data,
            Err(e) => return read_only(format!("{}: {e}", T::FILE)),
        };
        if from == T::VERSION {
            return Loaded {
                data,
                status: LoadStatus::Loaded,
            };
        }

        let backup = match backup_path(path, from) {
            Some(backup) => backup,
            None => return read_only(format!("{}: no free backup file name", T::FILE)),
        };
        if let Err(e) = fs::copy(path, &backup) {
            return read_only(format!("backing up {}: {e}", T::FILE));
        }
        if let Err(e) = self.write(path, &data) {
            return read_only(e.to_string());
        }
        Loaded {
            data,
            status: LoadStatus::Migrated { from, backup },
        }
    }

    /// Writes `data` atomically with the current version.
    pub fn save<T: Versioned>(&self, data: &T) -> Result<(), StorageError> {
        let path = self.path::<T>();
        if self.read_only.contains(T::FILE) {
            return Err(StorageError::ReadOnly {
                path,
                reason: "it could not be loaded safely".to_string(),
            });
        }
        self.write(&path, data)
    }

    fn write<T: Versioned>(&self, path: &Path, data: &T) -> Result<(), StorageError> {
        let text = toml::to_string(&Envelope {
            version: T::VERSION,
            data,
        })
        .map_err(|source| StorageError::Serialize {
            path: path.to_path_buf(),
            source,
        })?;
        atomic_write(&self.dir, path, text.as_bytes())
    }
}

/// `hosts.toml.bak-v1`, or `hosts.toml.bak-v1-2`, … if that exists.
fn backup_path(path: &Path, version: u32) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    (1..1000)
        .map(|n| {
            let suffix = if n == 1 {
                format!("{name}.bak-v{version}")
            } else {
                format!("{name}.bak-v{version}-{n}")
            };
            path.with_file_name(suffix)
        })
        .find(|p| !p.exists())
}

/// Writes `bytes` to `path` via a temp file in the same directory and a rename.
/// Refuses any `path` outside `dir`, so app code can never write elsewhere.
pub fn atomic_write(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if path.parent() != Some(dir) {
        return Err(StorageError::OutsideAppDir {
            path: path.to_path_buf(),
        });
    }
    replace_file(path, bytes).map_err(|source| StorageError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Atomically replaces `path` (temp file in the same directory, then rename),
/// creating its directory if needed. Only for files outside the app dir that
/// sshelf is allowed to change (`known_hosts`, SPEC.md §9).
pub(crate) fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no directory"))?;
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    /// Test schema: v1 had `title`, v2 renamed it to `name`, v3 added `count`.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Thing {
        name: String,
        count: u32,
        #[serde(default)]
        items: Vec<Item>,
    }

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Item {
        id: String,
    }

    fn v1_to_v2(mut t: toml::Table) -> Result<toml::Table, String> {
        let title = t.remove("title").ok_or("missing `title`")?;
        t.insert("name".into(), title);
        Ok(t)
    }

    fn v2_to_v3(mut t: toml::Table) -> Result<toml::Table, String> {
        t.insert("count".into(), toml::Value::Integer(1));
        Ok(t)
    }

    impl Versioned for Thing {
        const FILE: &'static str = "thing.toml";
        const VERSION: u32 = 3;
        fn migrations() -> &'static [Migration] {
            &[v1_to_v2, v2_to_v3]
        }
    }

    fn setup(contents: Option<&str>) -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        if let Some(text) = contents {
            fs::write(tmp.path().join("thing.toml"), text).unwrap();
        }
        let store = Store::new(tmp.path());
        (tmp, store)
    }

    fn file(store: &Store) -> String {
        fs::read_to_string(store.path::<Thing>()).unwrap()
    }

    fn sample() -> Thing {
        Thing {
            name: "x".into(),
            count: 2,
            items: vec![Item { id: "a".into() }],
        }
    }

    #[test]
    fn missing_file_gives_defaults() {
        let (_tmp, mut store) = setup(None);
        let loaded = store.load::<Thing>();
        assert_eq!(loaded.status, LoadStatus::Missing);
        assert_eq!(loaded.data, Thing::default());
        store.save(&sample()).unwrap();
    }

    #[test]
    fn save_then_load_round_trips_with_version_first() {
        let (_tmp, mut store) = setup(None);
        store.save(&sample()).unwrap();
        let text = file(&store);
        assert!(text.starts_with("version = 3\n"), "{text}");
        let loaded = store.load::<Thing>();
        assert_eq!(loaded.status, LoadStatus::Loaded);
        assert_eq!(loaded.data, sample());
    }

    #[test]
    fn old_version_is_migrated_and_backed_up() {
        let original = "version = 1\ntitle = \"old\"\n";
        let (_tmp, mut store) = setup(Some(original));
        let loaded = store.load::<Thing>();
        let LoadStatus::Migrated { from, backup } = &loaded.status else {
            panic!("not migrated: {:?}", loaded.status);
        };
        assert_eq!(*from, 1);
        assert_eq!(backup.file_name().unwrap(), "thing.toml.bak-v1");
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        assert_eq!(loaded.data.name, "old");
        assert_eq!(loaded.data.count, 1);
        assert!(file(&store).starts_with("version = 3\n"));
    }

    #[test]
    fn existing_backup_is_not_overwritten() {
        let (tmp, mut store) = setup(Some("version = 2\nname = \"n\"\n"));
        fs::write(tmp.path().join("thing.toml.bak-v2"), "keep").unwrap();
        let loaded = store.load::<Thing>();
        let LoadStatus::Migrated { backup, .. } = &loaded.status else {
            panic!("not migrated: {:?}", loaded.status);
        };
        assert_eq!(backup.file_name().unwrap(), "thing.toml.bak-v2-2");
        assert_eq!(
            fs::read_to_string(tmp.path().join("thing.toml.bak-v2")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn failed_migration_is_read_only() {
        let original = "version = 1\nnot_title = 1\n";
        let (_tmp, mut store) = setup(Some(original));
        let loaded = store.load::<Thing>();
        assert!(matches!(loaded.status, LoadStatus::ReadOnly(_)));
        assert!(store.save(&sample()).is_err());
        assert_eq!(file(&store), original);
    }

    #[test]
    fn newer_version_is_shown_but_never_written() {
        let original = "version = 9\nname = \"future\"\ncount = 5\nnew_field = true\n";
        let (_tmp, mut store) = setup(Some(original));
        let loaded = store.load::<Thing>();
        let LoadStatus::ReadOnly(reason) = &loaded.status else {
            panic!("not read-only: {:?}", loaded.status);
        };
        assert!(reason.contains("version 9"), "{reason}");
        assert_eq!(loaded.data.name, "future");
        let err = store.save(&sample()).unwrap_err();
        assert!(matches!(err, StorageError::ReadOnly { .. }), "{err}");
        assert_eq!(file(&store), original);
    }

    #[test]
    fn invalid_or_unversioned_files_are_read_only() {
        for original in [
            "this is = = not toml",
            "name = \"no version\"\ncount = 1\n",
            "version = \"1\"\n",
            "version = 0\n",
        ] {
            let (_tmp, mut store) = setup(Some(original));
            let loaded = store.load::<Thing>();
            assert!(
                matches!(loaded.status, LoadStatus::ReadOnly(_)),
                "{original:?}: {:?}",
                loaded.status
            );
            assert!(store.save(&sample()).is_err(), "{original:?}");
            assert_eq!(file(&store), original);
        }
    }

    #[test]
    fn reloading_a_fixed_file_clears_read_only() {
        let (tmp, mut store) = setup(Some("garbage ="));
        store.load::<Thing>();
        assert!(store.save(&sample()).is_err());
        fs::write(
            tmp.path().join("thing.toml"),
            "version = 3\nname = \"n\"\ncount = 0\n",
        )
        .unwrap();
        assert_eq!(store.load::<Thing>().status, LoadStatus::Loaded);
        store.save(&sample()).unwrap();
    }

    #[test]
    fn atomic_write_leaves_no_temp_files_and_creates_the_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("app");
        atomic_write(&dir, &dir.join("a.toml"), b"one").unwrap();
        atomic_write(&dir, &dir.join("a.toml"), b"two").unwrap();
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.toml"]);
        assert_eq!(fs::read(dir.join("a.toml")).unwrap(), b"two");
    }

    #[test]
    fn atomic_write_refuses_paths_outside_the_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("app");
        for path in [
            tmp.path().join("config"),
            dir.join("sub").join("x.toml"),
            tmp.path().join(".ssh").join("config"),
        ] {
            let err = atomic_write(&dir, &path, b"x").unwrap_err();
            assert!(matches!(err, StorageError::OutsideAppDir { .. }), "{err}");
            assert!(!path.exists());
        }
    }
}
