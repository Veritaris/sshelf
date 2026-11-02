//! The system ssh binary (SPEC.md §8): where it is, its version, and whether
//! the agent is reachable.
//!
//! Windows uses the built-in OpenSSH at `%SystemRoot%\System32\OpenSSH`
//! (explicit path, never `PATH`); macOS and Linux use `ssh` from `PATH`. A
//! settings override wins everywhere. Companion tools (`ssh-add`, …) are taken
//! from the same directory as `ssh`.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process::{self, Output};

/// Limit for `ssh -V` and `ssh-add -l`.
pub const QUICK_TIMEOUT: Duration = Duration::from_secs(5);
/// Limit for `ssh -G`, which may run `Match exec` commands from the config.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Effective configuration from `ssh -G <alias>`, in ssh's output order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// `(keyword, value)`; keywords are lowercase, repeated keywords
    /// (e.g. `identityfile`) appear once per value.
    pub options: Vec<(String, String)>,
}

impl Resolved {
    pub fn parse(stdout: &str) -> Self {
        let options = stdout
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(|line| match line.split_once(' ') {
                Some((key, value)) => (key.to_string(), value.to_string()),
                None => (line.to_string(), String::new()),
            })
            .collect();
        Self { options }
    }

    /// First value of `key` (lowercase).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    /// `ssh_path` from settings.
    Override,
    /// Windows' built-in OpenSSH.
    BuiltIn,
    /// Found on `PATH`.
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTools {
    pub ssh: PathBuf,
    pub source: ToolSource,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocateError {
    #[error("the ssh binary set in Settings does not exist: {}", .0.display())]
    OverrideMissing(PathBuf),
    #[error("ssh not found ({0})")]
    NotFound(String),
}

impl SshTools {
    /// Uses `override_path` if given (without falling back), else the platform default.
    pub fn locate(override_path: Option<&Path>) -> Result<Self, LocateError> {
        if let Some(path) = override_path {
            return if path.is_file() {
                Ok(Self {
                    ssh: path.to_path_buf(),
                    source: ToolSource::Override,
                })
            } else {
                Err(LocateError::OverrideMissing(path.to_path_buf()))
            };
        }
        if cfg!(windows) {
            let dir = windows_openssh_dir(env::var_os("SystemRoot").as_deref());
            let ssh = dir.join("ssh.exe");
            return if ssh.is_file() {
                Ok(Self {
                    ssh,
                    source: ToolSource::BuiltIn,
                })
            } else {
                Err(LocateError::NotFound(format!(
                    "{} does not exist; install the \"OpenSSH Client\" optional feature",
                    ssh.display()
                )))
            };
        }
        let path_var = env::var_os("PATH").unwrap_or_default();
        find_in_path("ssh", &path_var)
            .map(|ssh| Self {
                ssh,
                source: ToolSource::Path,
            })
            .ok_or_else(|| LocateError::NotFound("no `ssh` on PATH".to_string()))
    }

    /// A companion tool such as `ssh-add`: next to `ssh`, else (outside
    /// Windows) on `PATH`.
    pub fn companion(&self, name: &str) -> PathBuf {
        let file = format!("{name}{}", env::consts::EXE_SUFFIX);
        let beside = self.ssh.with_file_name(&file);
        if beside.is_file() || cfg!(windows) {
            return beside;
        }
        let path_var = env::var_os("PATH").unwrap_or_default();
        find_in_path(name, &path_var).unwrap_or(beside)
    }

    /// First line of `ssh -V` (printed on stderr).
    pub fn version(&self) -> Result<String, String> {
        let mut cmd = process::command(&self.ssh);
        cmd.arg("-V");
        let out = process::run(cmd, QUICK_TIMEOUT).map_err(|e| e.to_string())?;
        first_line(&out.stderr)
            .or_else(|| first_line(&out.stdout))
            .ok_or_else(|| format!("`ssh -V` printed nothing (exit code {:?})", out.code))
    }

    /// `ssh -G -- <alias>`: the effective configuration as ssh itself
    /// resolves it (SPEC.md §1.6). Never connects.
    pub fn resolve(&self, alias: &str) -> Result<Resolved, String> {
        let mut cmd = process::command(&self.ssh);
        cmd.args(["-G", "--", alias]);
        let out = process::run(cmd, RESOLVE_TIMEOUT).map_err(|e| e.to_string())?;
        if out.code != Some(0) {
            let message = out.stderr.trim();
            return Err(if message.is_empty() {
                format!("`ssh -G {alias}` failed with exit code {:?}", out.code)
            } else {
                message.to_string()
            });
        }
        Ok(Resolved::parse(&out.stdout))
    }

    /// Agent reachability and loaded keys, via `ssh-add -l`.
    pub fn agent_status(&self) -> AgentStatus {
        let mut cmd = process::command(&self.companion("ssh-add"));
        cmd.arg("-l");
        match process::run(cmd, QUICK_TIMEOUT) {
            Ok(out) => parse_agent_list(&out),
            Err(e) => AgentStatus::Error(e.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Reachable, with this many keys loaded.
    Keys(usize),
    /// No agent to talk to (`ssh-add` exit code 2).
    Unreachable(String),
    Error(String),
}

/// Interprets `ssh-add -l`: exit 0 lists one key per line, 1 means no
/// identities, 2 means the agent cannot be reached.
pub fn parse_agent_list(out: &Output) -> AgentStatus {
    let message = || {
        first_line(&out.stderr)
            .or_else(|| first_line(&out.stdout))
            .unwrap_or_default()
    };
    match out.code {
        Some(0) => AgentStatus::Keys(out.stdout.lines().filter(|l| !l.trim().is_empty()).count()),
        Some(1) => AgentStatus::Keys(0),
        Some(2) => AgentStatus::Unreachable(message()),
        code => AgentStatus::Error(format!("ssh-add exited with {code:?}: {}", message())),
    }
}

/// `<SystemRoot>\System32\OpenSSH`, defaulting to `C:\Windows`.
pub fn windows_openssh_dir(system_root: Option<&OsStr>) -> PathBuf {
    let root = system_root
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join("OpenSSH")
}

/// First executable `name` (plus the platform's exe suffix) in `path_var`.
pub fn find_in_path(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    let file = format!("{name}{}", env::consts::EXE_SUFFIX);
    env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn output(code: Option<i32>, stdout: &str, stderr: &str) -> Output {
        Output {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn make_tool(dir: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}{}", env::consts::EXE_SUFFIX));
        fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn resolved_output_parsing() {
        let resolved = Resolved::parse(
            "host web\nhostname web.example.com\nport 2222\nidentityfile ~/.ssh/a\n\
             identityfile ~/.ssh/b\nproxycommand ssh -W %h:%p jump\nforwardagent no\nbare\n\n",
        );
        assert_eq!(resolved.get("hostname"), Some("web.example.com"));
        assert_eq!(resolved.get("proxycommand"), Some("ssh -W %h:%p jump"));
        assert_eq!(resolved.get("bare"), Some(""));
        assert_eq!(resolved.get("missing"), None);
        let identities: Vec<_> = resolved
            .options
            .iter()
            .filter(|(k, _)| k == "identityfile")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(identities, ["~/.ssh/a", "~/.ssh/b"]);
        assert_eq!(resolved.options.len(), 8);
    }

    #[test]
    fn agent_list_parsing() {
        let keys = "256 SHA256:abc me@box (ED25519)\n3072 SHA256:def work (RSA)\n";
        assert_eq!(
            parse_agent_list(&output(Some(0), keys, "")),
            AgentStatus::Keys(2)
        );
        assert_eq!(
            parse_agent_list(&output(Some(1), "The agent has no identities.\n", "")),
            AgentStatus::Keys(0)
        );
        assert_eq!(
            parse_agent_list(&output(
                Some(2),
                "",
                "Error connecting to agent: No such file or directory\n"
            )),
            AgentStatus::Unreachable("Error connecting to agent: No such file or directory".into())
        );
        assert!(matches!(
            parse_agent_list(&output(None, "", "")),
            AgentStatus::Error(_)
        ));
    }

    #[test]
    fn windows_dir_uses_system_root() {
        assert_eq!(
            windows_openssh_dir(Some(OsStr::new(r"D:\Win"))),
            PathBuf::from(r"D:\Win").join("System32").join("OpenSSH")
        );
        assert_eq!(
            windows_openssh_dir(None),
            PathBuf::from(r"C:\Windows")
                .join("System32")
                .join("OpenSSH")
        );
        assert_eq!(
            windows_openssh_dir(Some(OsStr::new(""))),
            windows_openssh_dir(None)
        );
    }

    #[test]
    fn find_in_path_takes_the_first_match() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, empty) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("e"),
        );
        fs::create_dir_all(&empty).unwrap();
        make_tool(&b, "ssh");
        let second = make_tool(&a, "ssh");
        let path_var = env::join_paths([&empty, &a, &b]).unwrap();
        assert_eq!(find_in_path("ssh", &path_var), Some(second));
        assert_eq!(find_in_path("ssh-keygen", &path_var), None);
    }

    #[cfg(unix)]
    #[test]
    fn find_in_path_skips_non_executables() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("ssh"), "").unwrap();
        assert_eq!(find_in_path("ssh", tmp.path().as_os_str()), None);
    }

    #[test]
    fn override_is_used_without_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = make_tool(tmp.path(), "my-ssh");
        let tools = SshTools::locate(Some(&ssh)).unwrap();
        assert_eq!(tools.source, ToolSource::Override);
        assert_eq!(tools.ssh, ssh);

        let missing = tmp.path().join("nope");
        assert_eq!(
            SshTools::locate(Some(&missing)),
            Err(LocateError::OverrideMissing(missing))
        );
    }

    #[test]
    fn companion_sits_next_to_ssh() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = make_tool(tmp.path(), "ssh");
        let add = make_tool(tmp.path(), "ssh-add");
        let tools = SshTools {
            ssh,
            source: ToolSource::Override,
        };
        assert_eq!(tools.companion("ssh-add"), add);
    }
}
