//! `tunnels.toml`: tunnel definitions and groups (SPEC.md §7).
//!
//! A tunnel is one host alias plus N forwards and runs as one `ssh` process.
//! A group is a named set of tunnel ids; forwards live only on tunnels.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage::Versioned;

pub const DEFAULT_BIND: &str = "127.0.0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForwardKind {
    /// Local: listen here, connect from the server to `target`.
    L,
    /// Remote: listen on the server, connect from here to `target`.
    R,
    /// Dynamic: local SOCKS proxy.
    D,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    pub kind: ForwardKind,
    /// Listen address: local for `L`/`D`, on the server for `R`.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Listen port.
    pub listen: u16,
    /// `host:port` to connect to (`L`, `R`); absent for `D`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

fn default_bind() -> String {
    DEFAULT_BIND.to_string()
}

impl Forward {
    /// `-L 127.0.0.1:6443:localhost:6443` as two arguments.
    pub fn ssh_args(&self) -> [String; 2] {
        let bare = self.bind.trim_matches(['[', ']']);
        let bind = if bare.contains(':') {
            format!("[{bare}]")
        } else {
            bare.to_string()
        };
        let listen = if bind.is_empty() {
            self.listen.to_string()
        } else {
            format!("{bind}:{}", self.listen)
        };
        let (flag, spec) = match self.kind {
            ForwardKind::L => ("-L", format!("{listen}:{}", self.target_or_empty())),
            ForwardKind::R => ("-R", format!("{listen}:{}", self.target_or_empty())),
            ForwardKind::D => ("-D", listen),
        };
        [flag.to_string(), spec]
    }

    fn target_or_empty(&self) -> &str {
        self.target.as_deref().unwrap_or_default()
    }

    /// The local `(address, port)` ssh will listen on, for the pre-check.
    pub fn local_listen(&self) -> Option<(String, u16)> {
        match self.kind {
            ForwardKind::L | ForwardKind::D => {
                let addr = match self.bind.as_str() {
                    "" | "localhost" => DEFAULT_BIND,
                    "*" => "0.0.0.0",
                    other => other,
                };
                Some((addr.to_string(), self.listen))
            }
            ForwardKind::R => None,
        }
    }

    /// Listens on a non-loopback address (needs an explicit choice and a warning).
    pub fn is_exposed(&self) -> bool {
        match self.bind.as_str() {
            "" | "localhost" => false,
            other => other
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .map_or(true, |ip| !ip.is_loopback()),
        }
    }

    /// Short human form, e.g. `L 6443 → localhost:6443`.
    pub fn summary(&self) -> String {
        match self.kind {
            ForwardKind::D => format!("D {}:{} (SOCKS)", self.bind, self.listen),
            kind => format!(
                "{kind:?} {}:{} → {}",
                self.bind,
                self.listen,
                self.target_or_empty()
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tunnel {
    /// Stable, generated; never derived from the name.
    pub id: String,
    pub name: String,
    /// ssh_config alias.
    pub host: String,
    pub forwards: Vec<Forward>,
    /// Restart after an unexpected drop, with backoff (SPEC.md §7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_reconnect: bool,
}

impl Tunnel {
    /// Arguments after the ssh binary (SPEC.md §7). `-v` lets the supervisor
    /// see when the connection is up; `--` keeps the alias an alias.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-v",
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "BatchMode=yes",
        ]
        .map(String::from)
        .to_vec();
        for forward in &self.forwards {
            args.extend(forward.ssh_args());
        }
        args.push("--".into());
        args.push(self.host.clone());
        args
    }

    /// Problems that block saving, as user-facing messages.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.name.trim().is_empty() {
            problems.push("the name is empty".to_string());
        }
        if self.host.trim().is_empty() {
            problems.push("no host chosen".to_string());
        }
        if self.forwards.is_empty() {
            problems.push("add at least one forward".to_string());
        }
        let mut seen = Vec::new();
        for (i, f) in self.forwards.iter().enumerate() {
            let n = i + 1;
            if f.listen == 0 {
                problems.push(format!("forward {n}: the port must be 1–65535"));
            }
            if f.bind.chars().any(char::is_whitespace) {
                problems.push(format!("forward {n}: the bind address has spaces"));
            }
            match (f.kind, &f.target) {
                (ForwardKind::D, Some(_)) => {
                    problems.push(format!("forward {n}: a SOCKS forward has no target"))
                }
                (ForwardKind::L | ForwardKind::R, None) => {
                    problems.push(format!("forward {n}: the target is missing"))
                }
                (ForwardKind::L | ForwardKind::R, Some(t)) if !valid_target(t) => {
                    problems.push(format!("forward {n}: the target must be host:port"))
                }
                _ => {}
            }
            let key = (f.kind == ForwardKind::R, f.bind.clone(), f.listen);
            if seen.contains(&key) {
                problems.push(format!("forward {n}: the same port is used twice"));
            }
            seen.push(key);
        }
        problems
    }
}

/// `host:port` or `[v6]:port`, with a non-zero port.
fn valid_target(target: &str) -> bool {
    let Some((host, port)) = target.rsplit_once(':') else {
        return false;
    };
    let host_ok = if let Some(inner) = host.strip_prefix('[') {
        inner.strip_suffix(']').is_some_and(|h| !h.is_empty())
    } else {
        !host.is_empty() && !host.contains(':') && !host.chars().any(char::is_whitespace)
    };
    host_ok && port.parse::<u16>().is_ok_and(|p| p > 0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// Tunnel ids.
    pub tunnels: Vec<String>,
    /// Started when sshelf starts (SPEC.md §7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_on_launch: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TunnelsFile {
    #[serde(rename = "tunnel", skip_serializing_if = "Vec::is_empty")]
    pub tunnels: Vec<Tunnel>,
    #[serde(rename = "group", skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
}

impl Versioned for TunnelsFile {
    const FILE: &'static str = "tunnels.toml";
    const VERSION: u32 = 1;
}

impl TunnelsFile {
    pub fn tunnel(&self, id: &str) -> Option<&Tunnel> {
        self.tunnels.iter().find(|t| t.id == id)
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn groups_using(&self, tunnel_id: &str) -> Vec<&Group> {
        self.groups
            .iter()
            .filter(|g| g.tunnels.iter().any(|t| t == tunnel_id))
            .collect()
    }

    /// Inserts or replaces by id.
    pub fn upsert_tunnel(&mut self, tunnel: Tunnel) {
        match self.tunnels.iter_mut().find(|t| t.id == tunnel.id) {
            Some(existing) => *existing = tunnel,
            None => self.tunnels.push(tunnel),
        }
    }

    /// Removes the tunnel and its references from every group.
    pub fn remove_tunnel(&mut self, id: &str) -> bool {
        let before = self.tunnels.len();
        self.tunnels.retain(|t| t.id != id);
        for group in &mut self.groups {
            group.tunnels.retain(|t| t != id);
        }
        self.tunnels.len() != before
    }

    pub fn upsert_group(&mut self, group: Group) {
        match self.groups.iter_mut().find(|g| g.id == group.id) {
            Some(existing) => *existing = group,
            None => self.groups.push(group),
        }
    }

    pub fn remove_group(&mut self, id: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        self.groups.len() != before
    }

    pub fn new_tunnel_id(&self) -> String {
        new_id("t", |id| self.tunnel(id).is_some())
    }

    pub fn new_group_id(&self) -> String {
        new_id("g", |id| self.group(id).is_some())
    }
}

/// `<prefix>-xxxxxxxx` from a randomly seeded hash; never derived from a name.
fn new_id(prefix: &str, taken: impl Fn(&str) -> bool) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    (0u32..)
        .map(|n| {
            let hash = RandomState::new().hash_one((nanos, n));
            format!("{prefix}-{:08x}", hash as u32)
        })
        .find(|id| !taken(id))
        .expect("an unused id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;

    fn forward(kind: ForwardKind, bind: &str, listen: u16, target: Option<&str>) -> Forward {
        Forward {
            kind,
            bind: bind.into(),
            listen,
            target: target.map(String::from),
        }
    }

    fn tunnel(forwards: Vec<Forward>) -> Tunnel {
        Tunnel {
            id: "t-1".into(),
            name: "k3s API".into(),
            host: "k3s-master".into(),
            forwards,
            auto_reconnect: false,
        }
    }

    #[test]
    fn ssh_command_line() {
        let t = tunnel(vec![
            forward(ForwardKind::L, "127.0.0.1", 6443, Some("localhost:6443")),
            forward(ForwardKind::R, "", 8080, Some("localhost:80")),
            forward(ForwardKind::D, "::1", 1080, None),
        ]);
        assert_eq!(
            t.ssh_args(),
            [
                "-v",
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "BatchMode=yes",
                "-L",
                "127.0.0.1:6443:localhost:6443",
                "-R",
                "8080:localhost:80",
                "-D",
                "[::1]:1080",
                "--",
                "k3s-master",
            ]
        );
    }

    #[test]
    fn local_listen_only_for_local_kinds() {
        let l = forward(ForwardKind::L, "localhost", 1, Some("a:1"));
        assert_eq!(l.local_listen(), Some(("127.0.0.1".into(), 1)));
        let d = forward(ForwardKind::D, "*", 2, None);
        assert_eq!(d.local_listen(), Some(("0.0.0.0".into(), 2)));
        let r = forward(ForwardKind::R, "127.0.0.1", 3, Some("a:1"));
        assert_eq!(r.local_listen(), None);
    }

    #[test]
    fn exposed_binds() {
        for bind in ["127.0.0.1", "127.0.0.5", "::1", "[::1]", "localhost", ""] {
            assert!(
                !forward(ForwardKind::L, bind, 1, Some("a:1")).is_exposed(),
                "{bind}"
            );
        }
        for bind in ["0.0.0.0", "*", "192.168.1.10", "::", "myhost"] {
            assert!(
                forward(ForwardKind::L, bind, 1, Some("a:1")).is_exposed(),
                "{bind}"
            );
        }
    }

    #[test]
    fn validation() {
        let ok = tunnel(vec![forward(
            ForwardKind::L,
            DEFAULT_BIND,
            5432,
            Some("db.internal:5432"),
        )]);
        assert!(ok.validate().is_empty(), "{:?}", ok.validate());

        let bad = Tunnel {
            name: " ".into(),
            host: "".into(),
            ..tunnel(vec![
                forward(ForwardKind::L, DEFAULT_BIND, 0, Some("nohost")),
                forward(ForwardKind::R, DEFAULT_BIND, 80, None),
                forward(ForwardKind::D, DEFAULT_BIND, 1080, Some("x:1")),
                forward(ForwardKind::L, DEFAULT_BIND, 1080, Some("a:0")),
            ])
        };
        let problems = bad.validate();
        for expected in [
            "the name is empty",
            "no host chosen",
            "forward 1: the port must be 1–65535",
            "forward 1: the target must be host:port",
            "forward 2: the target is missing",
            "forward 3: a SOCKS forward has no target",
            "forward 4: the target must be host:port",
            "forward 4: the same port is used twice",
        ] {
            assert!(
                problems.iter().any(|p| p == expected),
                "{expected}: {problems:?}"
            );
        }
        assert!(
            tunnel(Vec::new())
                .validate()
                .contains(&"add at least one forward".to_string())
        );
    }

    #[test]
    fn targets() {
        for ok in [
            "localhost:6443",
            "db.internal:5432",
            "[::1]:80",
            "10.0.0.1:1",
        ] {
            assert!(valid_target(ok), "{ok}");
        }
        for bad in [
            "localhost",
            ":80",
            "host:",
            "host:0",
            "host:99999",
            "::1:80",
            "[]:80",
            "a b:1",
        ] {
            assert!(!valid_target(bad), "{bad}");
        }
    }

    #[test]
    fn removing_a_tunnel_cleans_groups() {
        let mut file = TunnelsFile::default();
        file.upsert_tunnel(tunnel(vec![]));
        file.upsert_group(Group {
            id: "g-1".into(),
            name: "Work".into(),
            tunnels: vec!["t-1".into(), "t-2".into()],
            start_on_launch: false,
        });
        assert_eq!(file.groups_using("t-1").len(), 1);
        assert!(file.remove_tunnel("t-1"));
        assert!(!file.remove_tunnel("t-1"));
        assert_eq!(file.groups[0].tunnels, ["t-2"]);
    }

    #[test]
    fn ids_are_unique_and_prefixed() {
        let mut file = TunnelsFile::default();
        for _ in 0..50 {
            let id = file.new_tunnel_id();
            assert!(id.starts_with("t-") && id.len() == 10, "{id}");
            assert!(file.tunnel(&id).is_none());
            file.upsert_tunnel(Tunnel {
                id,
                ..tunnel(vec![])
            });
        }
        assert!(file.new_group_id().starts_with("g-"));
    }

    #[test]
    fn file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let mut file = TunnelsFile::default();
        file.upsert_tunnel(tunnel(vec![
            forward(ForwardKind::L, DEFAULT_BIND, 6443, Some("localhost:6443")),
            forward(ForwardKind::D, DEFAULT_BIND, 1080, None),
        ]));
        file.upsert_group(Group {
            id: "g-1".into(),
            name: "Work on prod".into(),
            tunnels: vec!["t-1".into()],
            start_on_launch: true,
        });
        store.save(&file).unwrap();
        let text = std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap();
        assert!(text.starts_with("version = 1\n"), "{text}");
        assert_eq!(store.load::<TunnelsFile>().data, file);

        // The bind address defaults when omitted by hand.
        std::fs::write(
            store.path::<TunnelsFile>(),
            "version = 1\n[[tunnel]]\nid = \"t-9\"\nname = \"x\"\nhost = \"h\"\n\
             forwards = [{ kind = \"L\", listen = 1, target = \"a:1\" }]\n",
        )
        .unwrap();
        let loaded = store.load::<TunnelsFile>().data;
        assert_eq!(loaded.tunnels[0].forwards[0].bind, DEFAULT_BIND);
    }
}
