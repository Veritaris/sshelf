//! `tunnels.toml`: tunnel definitions and groups (SPEC.md §7).
//!
//! An ssh tunnel is one host alias plus N forwards and runs as one `ssh`
//! process; a kubectl tunnel (M13) is one context, namespace and resource
//! plus N forwards and runs as one `kubectl port-forward`. A group is a named
//! set of tunnel ids of either type; forwards live only on tunnels.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage::{Migration, Versioned};

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
        exposed(&self.bind)
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

/// A non-loopback listen address (`""` and `localhost` are loopback).
fn exposed(address: &str) -> bool {
    match address {
        "" | "localhost" => false,
        other => other
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .map_or(true, |ip| !ip.is_loopback()),
    }
}

/// What runs a tunnel (SPEC.md §7; kubectl from M13).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum TunnelType {
    #[default]
    Ssh,
    Kubectl,
}

impl TunnelType {
    /// The program's name, as in logs and `running.toml`.
    pub fn program(self) -> &'static str {
        match self {
            TunnelType::Ssh => "ssh",
            TunnelType::Kubectl => "kubectl",
        }
    }

    pub fn from_program(name: &str) -> Option<Self> {
        [TunnelType::Ssh, TunnelType::Kubectl]
            .into_iter()
            .find(|t| t.program() == name)
    }
}

/// What a kubectl tunnel forwards to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceKind {
    Pod,
    #[default]
    Service,
    Deployment,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 3] = [
        ResourceKind::Pod,
        ResourceKind::Service,
        ResourceKind::Deployment,
    ];

    /// As kubectl writes it: `pod`, `service`, `deployment`.
    pub fn word(self) -> &'static str {
        match self {
            ResourceKind::Pod => "pod",
            ResourceKind::Service => "service",
            ResourceKind::Deployment => "deployment",
        }
    }

    /// kubectl's plural, for `kubectl get`.
    pub fn plural(self) -> &'static str {
        match self {
            ResourceKind::Pod => "pods",
            ResourceKind::Service => "services",
            ResourceKind::Deployment => "deployments",
        }
    }
}

/// `<kind>/<name>`, e.g. `service/web`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resource {
    pub kind: ResourceKind,
    pub name: String,
}

impl Resource {
    /// Parses `pod/<name>`, `service/<name>` or `deployment/<name>`.
    pub fn parse(text: &str) -> Option<Self> {
        let (kind, name) = text.split_once('/')?;
        let kind = ResourceKind::ALL.into_iter().find(|k| k.word() == kind)?;
        (!name.is_empty() && !name.contains(['/', ' '])).then(|| Resource {
            kind,
            name: name.to_string(),
        })
    }
}

impl std::fmt::Display for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.kind.word(), self.name)
    }
}

/// One kubectl forward: a local port and a port on the resource (a number
/// or a port name, passed as written).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KubeForward {
    pub listen: u16,
    pub target: String,
}

/// The kubectl part of a tunnel (SPEC.md §7 *kubectl tunnels*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kube {
    /// Empty: the current context, looked up at each start.
    pub context: String,
    pub namespace: String,
    pub resource: Resource,
    /// `kubectl --address`, for every forward.
    pub address: String,
    pub forwards: Vec<KubeForward>,
}

impl Default for Kube {
    fn default() -> Self {
        Kube {
            context: String::new(),
            namespace: String::new(),
            resource: Resource::default(),
            address: DEFAULT_BIND.to_string(),
            forwards: Vec::new(),
        }
    }
}

impl Kube {
    /// Listens on a non-loopback address (the same warning as an ssh bind).
    pub fn is_exposed(&self) -> bool {
        exposed(&self.address)
    }
}

/// A port on a pod: `1`–`65535`, or an IANA-style port name (lowercase
/// letters, digits and `-`, at most 15, at least one letter).
pub fn valid_kube_port(target: &str) -> bool {
    if target.bytes().all(|b| b.is_ascii_digit()) {
        return target.parse::<u16>().is_ok_and(|p| p > 0);
    }
    target.len() <= 15
        && target
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !target.starts_with('-')
        && !target.ends_with('-')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawTunnel", into = "RawTunnel")]
pub struct Tunnel {
    /// Stable, generated; never derived from the name.
    pub id: String,
    pub name: String,
    pub kind: TunnelType,
    /// ssh_config alias (ssh tunnels).
    pub host: String,
    /// ssh forwards (ssh tunnels).
    pub forwards: Vec<Forward>,
    /// kubectl's fields (kubectl tunnels). The type not in use keeps its
    /// fields only in memory (the editor's switch); they are never saved.
    pub kube: Kube,
    /// Restart after an unexpected drop, with backoff (SPEC.md §7).
    pub auto_reconnect: bool,
    /// Tunnel ids that must be *Running* before this one starts (SPEC.md §7
    /// *Order and dependencies*, M15).
    pub depends_on: Vec<String>,
    /// Starting it alone starts its dependencies without asking ("Don't ask
    /// again").
    pub start_deps_without_asking: bool,
}

impl Tunnel {
    /// An ssh tunnel.
    pub fn ssh(id: &str, name: &str, host: &str, forwards: Vec<Forward>) -> Self {
        Tunnel {
            id: id.to_string(),
            name: name.to_string(),
            kind: TunnelType::Ssh,
            host: host.to_string(),
            forwards,
            kube: Kube::default(),
            auto_reconnect: false,
            depends_on: Vec::new(),
            start_deps_without_asking: false,
        }
    }

    /// A kubectl tunnel.
    pub fn kubectl(id: &str, name: &str, kube: Kube) -> Self {
        Tunnel {
            kind: TunnelType::Kubectl,
            kube,
            ..Tunnel::ssh(id, name, "", Vec::new())
        }
    }

    pub fn is_kubectl(&self) -> bool {
        self.kind == TunnelType::Kubectl
    }

    /// The tunnel with the other type's fields reset, as it is saved.
    pub fn cleaned(mut self) -> Self {
        match self.kind {
            TunnelType::Ssh => self.kube = Kube::default(),
            TunnelType::Kubectl => {
                self.host.clear();
                self.forwards.clear();
            }
        }
        self
    }

    /// The program's arguments for this tunnel's type.
    pub fn args(&self) -> Vec<String> {
        match self.kind {
            TunnelType::Ssh => self.ssh_args(),
            TunnelType::Kubectl => self.kubectl_args(),
        }
    }

    /// The local `(address, port)`s the tunnel listens on, for the pre-check.
    pub fn local_ports(&self) -> Vec<(String, u16)> {
        match self.kind {
            TunnelType::Ssh => self
                .forwards
                .iter()
                .filter_map(|f| f.local_listen())
                .collect(),
            TunnelType::Kubectl => {
                let addr = match self.kube.address.as_str() {
                    "" | "localhost" => DEFAULT_BIND,
                    other => other,
                };
                self.kube
                    .forwards
                    .iter()
                    .map(|f| (addr.to_string(), f.listen))
                    .collect()
            }
        }
    }

    /// The local ports, whatever the type.
    pub fn listen_ports(&self) -> Vec<u16> {
        match self.kind {
            TunnelType::Ssh => self.forwards.iter().map(|f| f.listen).collect(),
            TunnelType::Kubectl => self.kube.forwards.iter().map(|f| f.listen).collect(),
        }
    }

    /// Listens on a non-loopback address.
    pub fn is_exposed(&self) -> bool {
        match self.kind {
            TunnelType::Ssh => self.forwards.iter().any(Forward::is_exposed),
            TunnelType::Kubectl => self.kube.is_exposed(),
        }
    }

    /// `kubectl [--context c] --namespace ns port-forward kind/name l:t… --address a`
    /// (SPEC.md §7 *kubectl tunnels*).
    pub fn kubectl_args(&self) -> Vec<String> {
        let kube = &self.kube;
        let mut args = Vec::new();
        if !kube.context.is_empty() {
            args.extend(["--context".to_string(), kube.context.clone()]);
        }
        args.extend([
            "--namespace".to_string(),
            kube.namespace.clone(),
            "port-forward".to_string(),
            kube.resource.to_string(),
        ]);
        args.extend(
            kube.forwards
                .iter()
                .map(|f| format!("{}:{}", f.listen, f.target)),
        );
        args.extend(["--address".to_string(), kube.address.clone()]);
        args
    }

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
        if self.is_kubectl() {
            self.validate_kube(&mut problems);
            return problems;
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

    fn validate_kube(&self, problems: &mut Vec<String>) {
        let kube = &self.kube;
        let kind = kube.resource.kind.word();
        if kube.context.chars().any(char::is_whitespace) {
            problems.push("the context has spaces".to_string());
        }
        if kube.namespace.trim().is_empty() {
            problems.push("no namespace chosen".to_string());
        } else if kube.namespace.chars().any(char::is_whitespace) {
            problems.push("the namespace has spaces".to_string());
        }
        if kube.resource.name.trim().is_empty() {
            problems.push(format!("no {kind} chosen"));
        } else if kube.resource.name.contains(['/', ' ']) {
            problems.push(format!("the {kind} name has spaces or a `/`"));
        }
        if kube.address.trim().is_empty() {
            problems.push("the listen address is empty".to_string());
        } else if kube.address.chars().any(char::is_whitespace) {
            problems.push("the listen address has spaces".to_string());
        }
        if kube.forwards.is_empty() {
            problems.push("add at least one forward".to_string());
        }
        let mut seen = Vec::new();
        for (i, f) in kube.forwards.iter().enumerate() {
            let n = i + 1;
            if f.listen == 0 {
                problems.push(format!("forward {n}: the local port must be 1–65535"));
            }
            if f.target.is_empty() {
                problems.push(format!("forward {n}: the port on the {kind} is missing"));
            } else if !valid_kube_port(&f.target) {
                problems.push(format!(
                    "forward {n}: the port on the {kind} must be a number or a port name"
                ));
            }
            if f.listen != 0 && seen.contains(&f.listen) {
                problems.push(format!("forward {n}: the same local port is used twice"));
            }
            seen.push(f.listen);
        }
    }

    /// What a kubectl tunnel still lacks before it can be saved, in short
    /// words for the editor's footer ("Missing: namespace, service, a port").
    pub fn missing(&self) -> Vec<String> {
        let kube = &self.kube;
        let mut missing = Vec::new();
        if self.name.trim().is_empty() {
            missing.push("name".to_string());
        }
        if self.is_kubectl() {
            if kube.namespace.trim().is_empty() {
                missing.push("namespace".to_string());
            }
            if kube.resource.name.trim().is_empty() {
                missing.push(kube.resource.kind.word().to_string());
            }
            if !kube
                .forwards
                .iter()
                .any(|f| f.listen != 0 && !f.target.is_empty())
            {
                missing.push("a port".to_string());
            }
        } else {
            if self.host.trim().is_empty() {
                missing.push("host".to_string());
            }
            if self.forwards.is_empty() {
                missing.push("a forward".to_string());
            }
        }
        missing
    }
}

/// `tunnels.toml`'s form of a tunnel: both types' keys, optional, sorted out
/// by `type` (missing means ssh).
#[derive(Serialize, Deserialize)]
struct RawTunnel {
    id: String,
    name: String,
    #[serde(rename = "type", default, skip_serializing_if = "is_ssh")]
    kind: TunnelType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    forwards: Vec<RawForward>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    auto_reconnect: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    start_deps_without_asking: bool,
}

fn is_ssh(kind: &TunnelType) -> bool {
    *kind == TunnelType::Ssh
}

#[derive(Serialize, Deserialize)]
struct RawForward {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<ForwardKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bind: Option<String>,
    listen: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<PortOrText>,
}

/// A forward's target: ssh's `host:port`, or kubectl's port, which may be
/// written as a number by hand.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PortOrText {
    Port(u16),
    Text(String),
}

impl PortOrText {
    fn into_text(self) -> String {
        match self {
            PortOrText::Port(port) => port.to_string(),
            PortOrText::Text(text) => text,
        }
    }
}

impl TryFrom<RawTunnel> for Tunnel {
    type Error = String;

    fn try_from(raw: RawTunnel) -> Result<Self, String> {
        let RawTunnel {
            id,
            name,
            kind,
            host,
            context,
            namespace,
            resource,
            address,
            forwards,
            auto_reconnect,
            depends_on,
            start_deps_without_asking,
        } = raw;
        let mut tunnel = match kind {
            TunnelType::Ssh => {
                let host = host.ok_or_else(|| format!("tunnel `{id}`: missing `host`"))?;
                let forwards = forwards
                    .into_iter()
                    .map(|f| {
                        Ok(Forward {
                            kind: f
                                .kind
                                .ok_or_else(|| format!("tunnel `{id}`: a forward has no `kind`"))?,
                            bind: f.bind.unwrap_or_else(default_bind),
                            listen: f.listen,
                            target: f.target.map(PortOrText::into_text),
                        })
                    })
                    .collect::<Result<_, String>>()?;
                Tunnel::ssh(&id, &name, &host, forwards)
            }
            TunnelType::Kubectl => {
                let namespace =
                    namespace.ok_or_else(|| format!("tunnel `{id}`: missing `namespace`"))?;
                let resource =
                    resource.ok_or_else(|| format!("tunnel `{id}`: missing `resource`"))?;
                let resource = Resource::parse(&resource).ok_or_else(|| {
                    format!(
                        "tunnel `{id}`: `resource` must be pod/<name>, service/<name> or \
                         deployment/<name>, not `{resource}`"
                    )
                })?;
                let forwards = forwards
                    .into_iter()
                    .map(|f| KubeForward {
                        listen: f.listen,
                        target: f.target.map(PortOrText::into_text).unwrap_or_default(),
                    })
                    .collect();
                let kube = Kube {
                    context: context.unwrap_or_default(),
                    namespace,
                    resource,
                    address: address.unwrap_or_else(default_bind),
                    forwards,
                };
                Tunnel::kubectl(&id, &name, kube)
            }
        };
        tunnel.auto_reconnect = auto_reconnect;
        tunnel.depends_on = depends_on;
        tunnel.start_deps_without_asking = start_deps_without_asking;
        Ok(tunnel)
    }
}

impl From<Tunnel> for RawTunnel {
    fn from(tunnel: Tunnel) -> Self {
        let Tunnel {
            id,
            name,
            kind,
            host,
            forwards,
            kube,
            auto_reconnect,
            depends_on,
            start_deps_without_asking,
        } = tunnel;
        let mut raw = RawTunnel {
            id,
            name,
            kind,
            host: None,
            context: None,
            namespace: None,
            resource: None,
            address: None,
            forwards: Vec::new(),
            auto_reconnect,
            depends_on,
            start_deps_without_asking,
        };
        match kind {
            TunnelType::Ssh => {
                raw.host = Some(host);
                raw.forwards = forwards
                    .into_iter()
                    .map(|f| RawForward {
                        kind: Some(f.kind),
                        bind: Some(f.bind),
                        listen: f.listen,
                        target: f.target.map(PortOrText::Text),
                    })
                    .collect();
            }
            TunnelType::Kubectl => {
                raw.context = (!kube.context.is_empty()).then_some(kube.context);
                raw.namespace = Some(kube.namespace);
                raw.resource = Some(kube.resource.to_string());
                raw.address = Some(kube.address);
                raw.forwards = kube
                    .forwards
                    .into_iter()
                    .map(|f| RawForward {
                        kind: None,
                        bind: None,
                        listen: f.listen,
                        target: Some(PortOrText::Text(f.target)),
                    })
                    .collect();
            }
        }
        raw
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
    /// Tunnel ids, in start order (M15: every tunnel below the ones it
    /// depends on, see [`crate::deps::ordered`]).
    pub tunnels: Vec<String>,
    /// Started when sshelf starts (SPEC.md §7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_on_launch: bool,
    /// Starting the group starts its outside dependencies without asking
    /// ("Don't ask again", M15).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_deps_without_asking: bool,
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
    /// 2 from M13 (kubectl tunnels), 3 from M15 (dependencies): an older
    /// sshelf treats the file as newer, and read-only, rather than dropping
    /// what it doesn't know when it rewrites it.
    const VERSION: u32 = 3;

    fn migrations() -> &'static [Migration] {
        &[same, same]
    }
}

/// Nothing but the version changes (SPEC.md §7 *kubectl tunnels*, *Order
/// and dependencies*).
fn same(table: toml::Table) -> Result<toml::Table, String> {
    Ok(table)
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

    /// Removes the tunnel and its references from every group and every
    /// other tunnel's dependencies.
    pub fn remove_tunnel(&mut self, id: &str) -> bool {
        let before = self.tunnels.len();
        self.tunnels.retain(|t| t.id != id);
        for group in &mut self.groups {
            group.tunnels.retain(|t| t != id);
        }
        for tunnel in &mut self.tunnels {
            tunnel.depends_on.retain(|d| d != id);
        }
        self.tunnels.len() != before
    }

    /// Every group in start order ([`crate::deps::ordered`]): a hand-edited
    /// file that breaks the rule is fixed in memory on load, not rewritten.
    pub fn normalized(mut self) -> Self {
        let ordered: Vec<Vec<String>> = self
            .groups
            .iter()
            .map(|g| crate::deps::ordered(&g.tunnels, &self))
            .collect();
        for (group, tunnels) in self.groups.iter_mut().zip(ordered) {
            group.tunnels = tunnels;
        }
        self
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
        Tunnel::ssh("t-1", "k3s API", "k3s-master", forwards)
    }

    fn kube_tunnel(context: &str, forwards: &[(u16, &str)]) -> Tunnel {
        Tunnel::kubectl(
            "t-2",
            "web api",
            Kube {
                context: context.into(),
                namespace: "api".into(),
                resource: Resource::parse("service/web").unwrap(),
                address: DEFAULT_BIND.into(),
                forwards: forwards
                    .iter()
                    .map(|&(listen, target)| KubeForward {
                        listen,
                        target: target.into(),
                    })
                    .collect(),
            },
        )
    }

    #[test]
    fn kubectl_command_line() {
        let t = kube_tunnel("dev-k3s", &[(18080, "80"), (19090, "metrics")]);
        assert_eq!(
            t.args(),
            [
                "--context",
                "dev-k3s",
                "--namespace",
                "api",
                "port-forward",
                "service/web",
                "18080:80",
                "19090:metrics",
                "--address",
                "127.0.0.1",
            ]
        );
        // An empty context follows the current one: no `--context`.
        let current = kube_tunnel("", &[(18080, "80")]);
        assert_eq!(current.args()[..2], ["--namespace", "api"]);
        assert_eq!(current.local_ports(), [("127.0.0.1".to_string(), 18080)]);
        assert_eq!(TunnelType::Kubectl.program(), "kubectl");
        assert_eq!(TunnelType::from_program("ssh"), Some(TunnelType::Ssh));
    }

    #[test]
    fn resources_parse() {
        for kind in ResourceKind::ALL {
            let text = format!("{}/x-1", kind.word());
            let parsed = Resource::parse(&text).unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.to_string(), text);
        }
        for bad in ["svc/web", "service/", "web", "service/a/b", "pods/web", ""] {
            assert_eq!(Resource::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn kube_ports() {
        for ok in ["80", "65535", "http", "metrics", "web-1", "a"] {
            assert!(valid_kube_port(ok), "{ok}");
        }
        for bad in [
            "0",
            "65536",
            "Http",
            "-a",
            "a-",
            "a_b",
            "very-long-port-name",
        ] {
            assert!(!valid_kube_port(bad), "{bad}");
        }
    }

    #[test]
    fn kubectl_validation_and_missing_parts() {
        let ok = kube_tunnel("", &[(18080, "80")]);
        assert!(ok.validate().is_empty(), "{:?}", ok.validate());
        assert!(ok.missing().is_empty());

        let mut empty = Tunnel::kubectl("t-3", "", Kube::default());
        assert_eq!(empty.missing(), ["name", "namespace", "service", "a port"]);
        empty.kube.forwards.push(KubeForward::default());
        assert_eq!(empty.missing(), ["name", "namespace", "service", "a port"]);
        let problems = empty.validate();
        for expected in [
            "the name is empty",
            "no namespace chosen",
            "no service chosen",
            "forward 1: the local port must be 1–65535",
            "forward 1: the port on the service is missing",
        ] {
            assert!(
                problems.iter().any(|p| p == expected),
                "{expected}: {problems:?}"
            );
        }

        let mut bad = kube_tunnel("", &[(18080, "Bad"), (18080, "80")]);
        bad.kube.address = "0.0.0.0".into();
        assert!(bad.is_exposed());
        let problems = bad.validate();
        assert!(problems.contains(
            &"forward 1: the port on the service must be a number or a port name".to_string()
        ));
        assert!(problems.contains(&"forward 2: the same local port is used twice".to_string()));
    }

    #[test]
    fn cleaning_drops_the_other_types_fields() {
        let mut t = kube_tunnel("dev", &[(1, "80")]);
        t.host = "bastion".into();
        t.forwards
            .push(forward(ForwardKind::D, DEFAULT_BIND, 1080, None));
        let cleaned = t.clone().cleaned();
        assert!(cleaned.host.is_empty() && cleaned.forwards.is_empty());
        assert_eq!(cleaned.kube, t.kube);
        t.kind = TunnelType::Ssh;
        let cleaned = t.clone().cleaned();
        assert_eq!(cleaned.kube, Kube::default());
        assert_eq!(cleaned.host, "bastion");
    }

    #[test]
    fn both_types_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let mut file = TunnelsFile::default();
        file.upsert_tunnel(tunnel(vec![forward(
            ForwardKind::L,
            DEFAULT_BIND,
            6443,
            Some("localhost:6443"),
        )]));
        let mut kube = kube_tunnel("dev-k3s", &[(18080, "80"), (19090, "metrics")]);
        kube.auto_reconnect = true;
        file.upsert_tunnel(kube);
        file.upsert_tunnel(kube_tunnel("", &[(1, "2")]));
        store.save(&file).unwrap();
        let text = std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap();
        assert!(text.starts_with("version = 3\n"), "{text}");
        assert!(text.contains("type = \"kubectl\""), "{text}");
        assert!(text.contains("resource = \"service/web\""), "{text}");
        assert!(!text.contains("type = \"ssh\""), "{text}");
        assert_eq!(store.load::<TunnelsFile>().data, file);

        // By hand: a numeric target, no address, no context.
        std::fs::write(
            store.path::<TunnelsFile>(),
            "version = 3\n[[tunnel]]\nid = \"t-9\"\nname = \"x\"\ntype = \"kubectl\"\n\
             namespace = \"n\"\nresource = \"pod/p\"\nforwards = [{ listen = 1, target = 80 }]\n",
        )
        .unwrap();
        let loaded = store.load::<TunnelsFile>();
        assert_eq!(loaded.status, crate::storage::LoadStatus::Loaded);
        let t = &loaded.data.tunnels[0];
        assert_eq!(t.kube.forwards[0].target, "80");
        assert_eq!(t.kube.address, DEFAULT_BIND);
        assert_eq!(t.kube.resource.kind, ResourceKind::Pod);
    }

    #[test]
    fn broken_kubectl_tunnels_make_the_file_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        for body in [
            "type = \"kubectl\"\nresource = \"service/web\"",
            "type = \"kubectl\"\nnamespace = \"n\"\nresource = \"svc/web\"",
            "type = \"helm\"\nhost = \"h\"",
        ] {
            std::fs::write(
                store.path::<TunnelsFile>(),
                format!(
                    "version = 2\n[[tunnel]]\nid = \"t\"\nname = \"x\"\n{body}\nforwards = []\n"
                ),
            )
            .unwrap();
            assert!(
                matches!(
                    store.load::<TunnelsFile>().status,
                    crate::storage::LoadStatus::ReadOnly(_)
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn version_1_migrates_with_a_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let v1 = "version = 1\n[[tunnel]]\nid = \"t-1\"\nname = \"x\"\nhost = \"h\"\n\
                  forwards = [{ kind = \"L\", bind = \"127.0.0.1\", listen = 1, target = \"a:1\" }]\n";
        std::fs::write(store.path::<TunnelsFile>(), v1).unwrap();
        let loaded = store.load::<TunnelsFile>();
        assert!(
            matches!(
                loaded.status,
                crate::storage::LoadStatus::Migrated { from: 1, .. }
            ),
            "{:?}",
            loaded.status
        );
        assert_eq!(loaded.data.tunnels[0].host, "h");
        let backup = tmp.path().join("tunnels.toml.bak-v1");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), v1);
        let text = std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap();
        assert!(text.starts_with("version = 3\n"), "{text}");
    }

    /// M15: v2 → v3 changes only the version, with the backup; the new keys
    /// round-trip and are written only when set.
    #[test]
    fn version_2_migrates_and_dependencies_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let v2 =
            "version = 2\n[[tunnel]]\nid = \"t-1\"\nname = \"x\"\nhost = \"h\"\nforwards = []\n";
        std::fs::write(store.path::<TunnelsFile>(), v2).unwrap();
        let mut loaded = store.load::<TunnelsFile>();
        assert!(matches!(
            loaded.status,
            crate::storage::LoadStatus::Migrated { from: 2, .. }
        ));
        let backup = tmp.path().join("tunnels.toml.bak-v2");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), v2);
        let text = std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap();
        assert!(
            !text.contains("depends_on") && !text.contains("without_asking"),
            "{text}"
        );

        let mut dependent = Tunnel::ssh("t-2", "y", "h", Vec::new());
        dependent.depends_on = vec!["t-1".into()];
        dependent.start_deps_without_asking = true;
        loaded.data.upsert_tunnel(dependent);
        loaded.data.upsert_group(Group {
            id: "g-1".into(),
            name: "Dev".into(),
            tunnels: vec!["t-1".into(), "t-2".into()],
            start_on_launch: false,
            start_deps_without_asking: true,
        });
        store.save(&loaded.data).unwrap();
        let text = std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap();
        assert!(text.contains("depends_on = [\"t-1\"]"), "{text}");
        assert_eq!(store.load::<TunnelsFile>().data, loaded.data);
    }

    #[test]
    fn a_newer_version_stays_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::new(tmp.path());
        let v3 =
            "version = 4\n[[tunnel]]\nid = \"t-1\"\nname = \"x\"\nhost = \"h\"\nforwards = []\n";
        std::fs::write(store.path::<TunnelsFile>(), v3).unwrap();
        let loaded = store.load::<TunnelsFile>();
        assert!(matches!(
            loaded.status,
            crate::storage::LoadStatus::ReadOnly(_)
        ));
        assert!(store.save(&loaded.data).is_err());
        assert_eq!(
            std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap(),
            v3
        );
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
            start_deps_without_asking: false,
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
            start_deps_without_asking: false,
        });
        store.save(&file).unwrap();
        let text = std::fs::read_to_string(store.path::<TunnelsFile>()).unwrap();
        assert!(text.starts_with("version = 3\n"), "{text}");
        assert_eq!(store.load::<TunnelsFile>().data, file);

        // The bind address defaults when omitted by hand.
        std::fs::write(
            store.path::<TunnelsFile>(),
            "version = 2\n[[tunnel]]\nid = \"t-9\"\nname = \"x\"\nhost = \"h\"\n\
             forwards = [{ kind = \"L\", listen = 1, target = \"a:1\" }]\n",
        )
        .unwrap();
        let loaded = store.load::<TunnelsFile>().data;
        assert_eq!(loaded.tunnels[0].forwards[0].bind, DEFAULT_BIND);
    }
}
