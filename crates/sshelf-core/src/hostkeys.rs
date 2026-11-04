//! Host keys (SPEC.md §9): where ssh looks a host's key up, whether it is
//! known, fetching a server's key through ssh itself, fingerprints, and the
//! only writes sshelf makes to `known_hosts` (append, `ssh-keygen -R`).
//!
//! Behavior verified against OpenSSH_for_Windows 9.5p1: `ssh -G` prints
//! `userknownhostsfile` already expanded and `globalknownhostsfile` with a
//! literal `__PROGRAMDATA__`; `ssh-keygen -F` exits 0 (found), 1 (not found)
//! or 255 (no such file); `ssh-keygen -R` keeps a `.old` copy.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::process;
use crate::ssh::{Resolved, SshTools, Tool};
use crate::storage;

/// Nested `ProxyJump` chains followed at most this deep.
const MAX_JUMP_DEPTH: usize = 8;

/// Limit for fetching a key (connect, key exchange, maybe a login).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const KEYGEN_TIMEOUT: Duration = Duration::from_secs(10);

/// One key as `ssh-keygen -lf` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    pub bits: String,
    /// `SHA256:…`
    pub fingerprint: String,
    /// e.g. `ED25519`, `RSA`.
    pub kind: String,
}

/// Parses `ssh-keygen -lf` output (`256 SHA256:… name (ED25519)`).
pub fn parse_fingerprints(stdout: &str) -> Vec<KeyInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let bits = words.next()?.to_string();
            let fingerprint = words.next()?.to_string();
            let kind = line
                .rsplit_once('(')
                .and_then(|(_, k)| k.strip_suffix(')'))?
                .to_string();
            fingerprint.contains(':').then_some(KeyInfo {
                bits,
                fingerprint,
                kind,
            })
        })
        .collect()
}

/// Where and under which name ssh looks up a destination's host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTarget {
    /// What is passed to ssh: an alias or `[user@]host[:port]`.
    pub destination: String,
    /// Host name ssh connects to (for display).
    pub hostname: String,
    pub port: u16,
    /// `HostKeyAlias` or host name, as `[name]:port` for non-22 ports.
    pub lookup: String,
    /// `UserKnownHostsFile` entries; new keys go to the first.
    pub user_files: Vec<PathBuf>,
    /// `GlobalKnownHostsFile` entries (read only).
    pub global_files: Vec<PathBuf>,
    /// `ProxyJump` hops, first hop first.
    pub jumps: Vec<String>,
}

impl KeyTarget {
    pub fn from_resolved(destination: &str, resolved: &Resolved, home: &Path) -> Self {
        let hostname = resolved.get("hostname").unwrap_or(destination).to_string();
        let port = resolved
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);
        let name = resolved
            .get("hostkeyalias")
            .filter(|a| !a.is_empty() && *a != "none")
            .unwrap_or(&hostname);
        let files = |key: &str| {
            resolved
                .get(key)
                .map(|v| v.split_whitespace().map(|f| expand_path(f, home)).collect())
                .unwrap_or_default()
        };
        Self {
            destination: destination.to_string(),
            lookup: lookup_name(name, port),
            hostname,
            port,
            user_files: files("userknownhostsfile"),
            global_files: files("globalknownhostsfile"),
            jumps: resolved
                .get("proxyjump")
                .map(parse_proxyjump)
                .unwrap_or_default(),
        }
    }

    /// `host` or `host:port` for messages.
    pub fn display(&self) -> String {
        if self.port == 22 {
            self.hostname.clone()
        } else {
            format!("{}:{}", self.hostname, self.port)
        }
    }
}

/// The name a key is stored under: `name`, or `[name]:port` for non-22 ports.
pub fn lookup_name(name: &str, port: u16) -> String {
    if port == 22 {
        name.to_string()
    } else {
        format!("[{name}]:{port}")
    }
}

/// `ProxyJump` value as destinations, first hop first (`none` → no hops).
pub fn parse_proxyjump(value: &str) -> Vec<String> {
    if value.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    value
        .split(',')
        .map(|hop| hop.trim().trim_start_matches("ssh://").to_string())
        .filter(|hop| !hop.is_empty())
        .collect()
}

/// ssh arguments for a destination: `user@host:port` → `-p port -- user@host`.
pub fn destination_args(destination: &str) -> Vec<String> {
    let (rest, port) = match destination.rsplit_once(':') {
        // Keep IPv6 literals (`[::1]`) and plain aliases intact.
        Some((host, port)) if !host.ends_with(':') && port.parse::<u16>().is_ok() => {
            (host, Some(port))
        }
        _ => (destination, None),
    };
    let mut args = Vec::new();
    if let Some(port) = port {
        args.extend(["-p".to_string(), port.to_string()]);
    }
    args.extend(["--".to_string(), rest.to_string()]);
    args
}

/// Expands `~` and Windows' `__PROGRAMDATA__` placeholder in a path from `ssh -G`.
pub fn expand_path(raw: &str, home: &Path) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home.join(rest);
    }
    if let Some(rest) = raw.strip_prefix("__PROGRAMDATA__") {
        let data = env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
        return PathBuf::from(data).join(rest.trim_start_matches(['\\', '/']));
    }
    PathBuf::from(raw)
}

/// Algorithms to request a key of the same type as a `known_hosts` line.
pub fn host_key_algorithms(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first = words.next()?;
    let kind = if first.starts_with('@') {
        words.nth(1)?
    } else {
        words.next()?
    };
    Some(match kind {
        "ssh-rsa" => "rsa-sha2-512,rsa-sha2-256,ssh-rsa".to_string(),
        other => other.to_string(),
    })
}

/// `ssh-keygen` operations on `known_hosts` files.
#[derive(Debug, Clone)]
pub struct Keygen {
    pub tool: Tool,
}

impl Keygen {
    fn run(&self, args: &[&str]) -> Result<process::Output, String> {
        let mut cmd = self.tool.command();
        cmd.args(args);
        process::run(cmd, KEYGEN_TIMEOUT).map_err(|e| e.to_string())
    }

    /// Key lines stored for `lookup` in `file` (hashed entries included).
    pub fn find(&self, lookup: &str, file: &Path) -> Result<Vec<String>, String> {
        if !file.is_file() {
            return Ok(Vec::new());
        }
        let out = self.run(&["-F", lookup, "-f", &file.to_string_lossy()])?;
        match out.code {
            Some(0) => Ok(out
                .stdout
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()),
            Some(1) => Ok(Vec::new()),
            _ => Err(format!(
                "ssh-keygen -F failed for {}: {}",
                file.display(),
                out.stderr.trim()
            )),
        }
    }

    /// Fingerprints of `known_hosts` lines.
    pub fn fingerprints(&self, lines: &[String]) -> Result<Vec<KeyInfo>, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let file = dir.path().join("keys");
        fs::write(&file, lines.join("\n") + "\n").map_err(|e| e.to_string())?;
        let out = self.run(&["-lf", &file.to_string_lossy()])?;
        if out.code != Some(0) {
            return Err(format!("ssh-keygen -lf failed: {}", out.stderr.trim()));
        }
        Ok(parse_fingerprints(&out.stdout))
    }

    /// Removes every key for `lookup` from `file` (ssh-keygen keeps `<file>.old`).
    pub fn remove(&self, lookup: &str, file: &Path) -> Result<(), String> {
        let out = self.run(&["-R", lookup, "-f", &file.to_string_lossy()])?;
        if out.code == Some(0) {
            Ok(())
        } else {
            Err(format!(
                "ssh-keygen -R failed for {}: {}",
                file.display(),
                out.stderr.trim()
            ))
        }
    }

    /// Stored key lines per file (user and global files).
    pub fn known(&self, target: &KeyTarget) -> Result<Vec<(PathBuf, Vec<String>)>, String> {
        let mut found = Vec::new();
        for file in target.user_files.iter().chain(&target.global_files) {
            let lines = self.find(&target.lookup, file)?;
            if !lines.is_empty() {
                found.push((file.clone(), lines));
            }
        }
        Ok(found)
    }
}

/// Fetches the server's host key through ssh itself (SPEC.md §9), which also
/// works through `ProxyJump`/`ProxyCommand`. Key exchange happens before
/// authentication, so the key arrives even if the login then fails. Returns
/// the `known_hosts` lines ssh wrote (in the user's `HashKnownHosts` format).
pub fn fetch(
    ssh: &Tool,
    destination: &str,
    algorithms: Option<&str>,
) -> Result<Vec<String>, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let file = dir.path().join("known_hosts");
    // ssh splits the value on spaces; quote it and use forward slashes.
    let quoted = file.to_string_lossy().replace('\\', "/");
    let mut cmd = ssh.command();
    cmd.args([
        "-o",
        &format!("UserKnownHostsFile=\"{quoted}\""),
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
    ]);
    if let Some(algorithms) = algorithms {
        cmd.args(["-o", &format!("HostKeyAlgorithms={algorithms}")]);
    }
    cmd.args(destination_args(destination)).arg("exit");
    let out = process::run(cmd, FETCH_TIMEOUT).map_err(|e| e.to_string())?;
    let lines: Vec<String> = fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    if lines.is_empty() {
        let reason = out
            .stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("ssh did not receive a host key")
            .to_string();
        return Err(reason);
    }
    Ok(lines)
}

/// A host whose key is not stored yet, with the key the server presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    pub target: KeyTarget,
    /// `known_hosts` lines to append if the user accepts.
    pub lines: Vec<String>,
    pub keys: Vec<KeyInfo>,
    /// Set when this is a jump host on the way to that destination.
    pub hop_of: Option<String>,
}

/// A host whose stored key no longer matches the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedKey {
    pub target: KeyTarget,
    /// User files holding the old key (what Replace changes).
    pub user_files: Vec<PathBuf>,
    pub old: Vec<KeyInfo>,
    pub new: Vec<KeyInfo>,
    pub new_lines: Vec<String>,
    pub hop_of: Option<String>,
}

/// Every host on the way to `destinations`, jump hosts first (SPEC.md §9:
/// hops are verified in order), each once.
pub fn chain(
    tools: &SshTools,
    home: &Path,
    destinations: &[String],
) -> Result<Vec<(KeyTarget, Option<String>)>, String> {
    fn walk(
        tools: &SshTools,
        home: &Path,
        destination: &str,
        hop_of: Option<&str>,
        depth: usize,
        out: &mut Vec<(KeyTarget, Option<String>)>,
    ) -> Result<(), String> {
        if depth > MAX_JUMP_DEPTH {
            return Err(format!("ProxyJump chain to {destination} is too deep"));
        }
        if out.iter().any(|(t, _)| t.destination == destination) {
            return Ok(());
        }
        let resolved = tools.resolve(destination)?;
        let target = KeyTarget::from_resolved(destination, &resolved, home);
        for jump in &target.jumps {
            walk(
                tools,
                home,
                jump,
                Some(hop_of.unwrap_or(destination)),
                depth + 1,
                out,
            )?;
        }
        out.push((target, hop_of.map(String::from)));
        Ok(())
    }
    let mut out = Vec::new();
    for destination in destinations {
        walk(tools, home, destination, None, 0, &mut out)?;
    }
    Ok(out)
}

/// The first host on the way to `destinations` whose key is not stored,
/// with the key the server presents (fetched through ssh, SPEC.md §9).
pub fn first_unknown(
    tools: &SshTools,
    keygen: &Keygen,
    home: &Path,
    destinations: &[String],
) -> Result<Option<UnknownKey>, String> {
    for (target, hop_of) in chain(tools, home, destinations)? {
        if !keygen.known(&target)?.is_empty() {
            continue;
        }
        let lines = fetch(&tools.ssh, &target.destination, None)
            .map_err(|e| format!("cannot get the host key of {}: {e}", target.display()))?;
        let keys = keygen.fingerprints(&lines)?;
        return Ok(Some(UnknownKey {
            target,
            lines,
            keys,
            hop_of,
        }));
    }
    Ok(None)
}

/// The host on the way to `destination` whose server key differs from the
/// stored one; `None` when every stored key matches its server. Fetches a
/// key of the stored type where the server has one.
pub fn find_changed(
    tools: &SshTools,
    keygen: &Keygen,
    home: &Path,
    destination: &str,
) -> Result<Option<ChangedKey>, String> {
    for (target, hop_of) in chain(tools, home, &[destination.to_string()])? {
        let known = keygen.known(&target)?;
        let Some(first) = known.first().and_then(|(_, lines)| lines.first()) else {
            continue;
        };
        let old_lines: Vec<String> = known.iter().flat_map(|(_, l)| l.clone()).collect();
        let algorithms = host_key_algorithms(first);
        let new_lines = fetch(&tools.ssh, &target.destination, algorithms.as_deref())
            .or_else(|_| fetch(&tools.ssh, &target.destination, None))
            .map_err(|e| format!("cannot get the host key of {}: {e}", target.display()))?;
        let old = keygen.fingerprints(&old_lines)?;
        let new = keygen.fingerprints(&new_lines)?;
        if new
            .iter()
            .any(|n| old.iter().any(|o| o.fingerprint == n.fingerprint))
        {
            continue;
        }
        let user_files = known
            .iter()
            .map(|(file, _)| file.clone())
            .filter(|file| target.user_files.contains(file))
            .collect();
        return Ok(Some(ChangedKey {
            target,
            user_files,
            old,
            new,
            new_lines,
            hop_of,
        }));
    }
    Ok(None)
}

/// The only writer for `known_hosts`: atomic appends, with a timestamped
/// backup of each file before its first change in a session.
#[derive(Debug, Default)]
pub struct KnownHostsWriter {
    backed_up: HashSet<PathBuf>,
}

impl KnownHostsWriter {
    /// Backs `file` up once per session (if it exists).
    pub fn backup_once(&mut self, file: &Path) -> Result<(), String> {
        if !file.is_file() || !self.backed_up.insert(file.to_path_buf()) {
            return Ok(());
        }
        let stamp = humantime::format_rfc3339_seconds(SystemTime::now())
            .to_string()
            .replace(':', "");
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "known_hosts".into());
        let backup = file.with_file_name(format!("{name}.sshelf-{stamp}.bak"));
        fs::copy(file, &backup)
            .map(|_| ())
            .map_err(|e| format!("backing up {}: {e}", file.display()))
    }

    /// Appends `lines`, keeping the file's line endings.
    pub fn append(&mut self, file: &Path, lines: &[String]) -> Result<(), String> {
        self.backup_once(file)?;
        let mut bytes = fs::read(file).unwrap_or_default();
        let eol: &[u8] = if bytes.windows(2).any(|w| w == b"\r\n") {
            b"\r\n"
        } else {
            b"\n"
        };
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.extend_from_slice(eol);
        }
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.extend_from_slice(eol);
        }
        storage::replace_file(file, &bytes).map_err(|e| format!("writing {}: {e}", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_from_keygen_output() {
        let out = "256 SHA256:9Xt6/QL43 web.example.com (ED25519)\n\
                   3072 SHA256:abc |1|hashed|name= (RSA)\nnot a key line\n";
        assert_eq!(
            parse_fingerprints(out),
            [
                KeyInfo {
                    bits: "256".into(),
                    fingerprint: "SHA256:9Xt6/QL43".into(),
                    kind: "ED25519".into()
                },
                KeyInfo {
                    bits: "3072".into(),
                    fingerprint: "SHA256:abc".into(),
                    kind: "RSA".into()
                },
            ]
        );
    }

    #[test]
    fn lookup_names() {
        assert_eq!(lookup_name("web.example.com", 22), "web.example.com");
        assert_eq!(
            lookup_name("odd.example.com", 2222),
            "[odd.example.com]:2222"
        );
    }

    #[test]
    fn proxyjump_chains() {
        assert_eq!(
            parse_proxyjump("admin@bastion.example.com:2200,ssh://hop2, hop3"),
            ["admin@bastion.example.com:2200", "hop2", "hop3"]
        );
        assert!(parse_proxyjump("none").is_empty());
    }

    #[test]
    fn destinations_to_ssh_arguments() {
        assert_eq!(destination_args("web"), ["--", "web"]);
        assert_eq!(
            destination_args("admin@bastion:2200"),
            ["-p", "2200", "--", "admin@bastion"]
        );
        assert_eq!(destination_args("[::1]"), ["--", "[::1]"]);
        assert_eq!(
            destination_args("[::1]:2222"),
            ["-p", "2222", "--", "[::1]"]
        );
        assert_eq!(destination_args("host:notaport"), ["--", "host:notaport"]);
    }

    #[test]
    fn paths_from_ssh_g() {
        let home = Path::new("/home/me");
        assert_eq!(
            expand_path("~/.ssh/known_hosts", home),
            home.join(".ssh/known_hosts")
        );
        assert_eq!(
            expand_path("/etc/ssh/ssh_known_hosts", home),
            PathBuf::from("/etc/ssh/ssh_known_hosts")
        );
        let global = expand_path(r"__PROGRAMDATA__\ssh/ssh_known_hosts", home);
        assert!(global.ends_with("ssh/ssh_known_hosts"), "{global:?}");
        assert!(!global.to_string_lossy().contains("__PROGRAMDATA__"));
    }

    #[test]
    fn target_from_ssh_g_output() {
        let resolved = Resolved::parse(
            "hostname 10.0.0.5\nport 2222\nhostkeyalias none\n\
             userknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2\n\
             globalknownhostsfile /etc/ssh/ssh_known_hosts\n\
             proxyjump admin@bastion:2200,hop2\n",
        );
        let home = Path::new("/home/me");
        let target = KeyTarget::from_resolved("jumped", &resolved, home);
        assert_eq!(target.lookup, "[10.0.0.5]:2222");
        assert_eq!(target.display(), "10.0.0.5:2222");
        assert_eq!(
            target.user_files,
            [
                home.join(".ssh/known_hosts"),
                home.join(".ssh/known_hosts2")
            ]
        );
        assert_eq!(target.jumps, ["admin@bastion:2200", "hop2"]);

        let aliased = Resolved::parse("hostname real.example.com\nport 22\nhostkeyalias stable\n");
        assert_eq!(
            KeyTarget::from_resolved("a", &aliased, home).lookup,
            "stable"
        );
    }

    #[test]
    fn algorithms_follow_the_stored_key_type() {
        assert_eq!(
            host_key_algorithms("web ssh-ed25519 AAAA").as_deref(),
            Some("ssh-ed25519")
        );
        assert_eq!(
            host_key_algorithms("|1|x|y= ssh-rsa AAAA").as_deref(),
            Some("rsa-sha2-512,rsa-sha2-256,ssh-rsa")
        );
        assert_eq!(
            host_key_algorithms("@cert-authority *.corp ssh-ed25519 AAAA").as_deref(),
            Some("ssh-ed25519")
        );
    }

    #[test]
    fn append_keeps_line_endings_and_backs_up_once() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("known_hosts");
        fs::write(&file, "a ssh-ed25519 AAAA\r\nb ssh-ed25519 BBBB").unwrap();
        let mut writer = KnownHostsWriter::default();
        writer
            .append(&file, &["c ssh-ed25519 CCCC".into()])
            .unwrap();
        writer
            .append(&file, &["d ssh-ed25519 DDDD".into()])
            .unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "a ssh-ed25519 AAAA\r\nb ssh-ed25519 BBBB\r\nc ssh-ed25519 CCCC\r\nd ssh-ed25519 DDDD\r\n"
        );
        let backups: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".sshelf-"))
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        let backup = fs::read_to_string(tmp.path().join(&backups[0])).unwrap();
        assert_eq!(backup, "a ssh-ed25519 AAAA\r\nb ssh-ed25519 BBBB");
    }

    #[test]
    fn append_creates_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(".ssh").join("known_hosts");
        KnownHostsWriter::default()
            .append(&file, &["x ssh-ed25519 XXXX".into()])
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "x ssh-ed25519 XXXX\n");
    }
}
