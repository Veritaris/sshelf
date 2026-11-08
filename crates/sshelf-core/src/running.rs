//! `running.toml` (SPEC.md §7 *Surviving a crash*, M12): the tunnels whose
//! ssh runs right now, written by the supervisor whenever that changes.
//! After a crash it lets the next start take those processes over (Unix;
//! on Windows they die with sshelf). A clean quit leaves it empty.

use serde::{Deserialize, Serialize};

use crate::storage::Versioned;
use crate::supervisor::Owner;
use crate::tunnels::TunnelType;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunningFile {
    #[serde(rename = "tunnel", skip_serializing_if = "Vec::is_empty")]
    pub tunnels: Vec<RunningTunnel>,
}

impl Versioned for RunningFile {
    const FILE: &'static str = "running.toml";
    const VERSION: u32 = 1;
}

impl RunningTunnel {
    /// The tunnel type that ran it; `None` for a program this sshelf doesn't
    /// know.
    pub fn tunnel_type(&self) -> Option<TunnelType> {
        TunnelType::from_program(self.program.as_deref().unwrap_or("ssh"))
    }
}

/// One running ssh or kubectl: enough to tell it apart from a process that only got
/// its pid later, and to see whether the tunnel changed since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningTunnel {
    pub id: String,
    /// Also its process group (it leads one).
    pub pid: u32,
    /// The process's start time as the OS reports it (`child::start_time`).
    pub started: u64,
    /// `ssh` or `kubectl` (M13; missing means `ssh`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// The program's arguments (`Tunnel::args`), without the program.
    pub args: Vec<String>,
    /// `manual`, or `group:<id>`.
    pub owners: Vec<String>,
}

/// An owner as `running.toml` stores it.
pub fn owner_key(owner: &Owner) -> String {
    match owner {
        Owner::Manual => "manual".to_string(),
        Owner::Group(id) => format!("group:{id}"),
        Owner::NeededBy(id) => format!("needed-by:{id}"),
    }
}

/// The owner a stored key names, if any.
pub fn owner_from_key(key: &str) -> Option<Owner> {
    match key.split_once(':') {
        Some(("group", id)) if !id.is_empty() => Some(Owner::Group(id.to_string())),
        Some(("needed-by", id)) if !id.is_empty() => Some(Owner::NeededBy(id.to_string())),
        None if key == "manual" => Some(Owner::Manual),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_round_trip_and_unknown_keys_are_dropped() {
        for owner in [
            Owner::Manual,
            Owner::Group("g-1".into()),
            Owner::NeededBy("t-1".into()),
        ] {
            assert_eq!(owner_from_key(&owner_key(&owner)), Some(owner));
        }
        assert_eq!(owner_from_key("group:"), None);
        assert_eq!(owner_from_key("someone"), None);
    }

    #[test]
    fn the_file_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = crate::storage::Store::new(tmp.path());
        let file = RunningFile {
            tunnels: vec![RunningTunnel {
                id: "t-1".into(),
                pid: 4242,
                started: 1_234_567,
                program: Some("kubectl".into()),
                args: vec!["-N".into(), "--".into(), "web".into()],
                owners: vec!["manual".into(), "group:g-1".into()],
            }],
        };
        store.save(&file).unwrap();
        assert_eq!(store.load::<RunningFile>().data, file);
        assert_eq!(file.tunnels[0].tunnel_type(), Some(TunnelType::Kubectl));
    }

    /// M12's records have no `program`: they are ssh.
    #[test]
    fn records_without_a_program_are_ssh() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = crate::storage::Store::new(tmp.path());
        std::fs::write(
            store.path::<RunningFile>(),
            "version = 1\n[[tunnel]]\nid = \"t\"\npid = 1\nstarted = 2\nargs = []\nowners = []\n",
        )
        .unwrap();
        let file = store.load::<RunningFile>().data;
        assert_eq!(file.tunnels[0].tunnel_type(), Some(TunnelType::Ssh));
        let unknown = RunningTunnel {
            program: Some("helm".into()),
            ..file.tunnels[0].clone()
        };
        assert_eq!(unknown.tunnel_type(), None);
    }
}
