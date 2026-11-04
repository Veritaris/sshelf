//! The tour's sandbox (SPEC.md §10 *Onboarding tour*): a temporary home and
//! app data dir (`<temp>/sshelf-tour-<pid>`) holding the sample bundle, with
//! sshelf itself as its ssh. Deleted when dropped; leftovers of a crashed run
//! are deleted at the next start.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use sshelf_core::paths::Paths;
use sshelf_core::ssh::Tool;

const SSH_CONFIG: &str = include_str!("sample/ssh_config");
const HOSTS: &str = include_str!("sample/hosts.toml");
const TUNNELS: &str = include_str!("sample/tunnels.toml");
const SCENARIOS: &str = include_str!("sample/scenarios.toml");

/// The sample hosts: alias, host name, user, jump host. `-G` answers are
/// rendered from it, with the `Host *` values of the sample config.
const SAMPLE_HOSTS: [(&str, &str, &str, Option<&str>); 7] = [
    ("web-01", "web-01.example.com", "deploy", None),
    ("web-02", "web-02.example.com", "deploy", None),
    (
        "db-primary",
        "db-primary.internal.example.com",
        "postgres",
        Some("bastion"),
    ),
    ("bastion", "bastion.example.com", "jump", None),
    ("staging-api", "staging-api.example.com", "deploy", None),
    ("ci-runner", "ci-runner.example.com", "ci", None),
    ("backup-nas", "backup-nas.example.com", "backup", None),
];

/// Local ports of the sample tunnels, and of the one the tour makes.
pub const SAMPLE_PORTS: [u16; 5] = [15432, 18080, 19090, 12049, 18443];

/// The folder name's prefix.
const PREFIX: &str = "sshelf-tour-";

/// How the sandbox runs fake-ssh: a program and the arguments before
/// fake-ssh's own (`[sshelf, --fake-ssh]`).
#[derive(Debug, Clone)]
pub struct FakeSsh {
    pub program: PathBuf,
    pub prefix: Vec<OsString>,
}

impl FakeSsh {
    /// This very program as `sshelf --fake-ssh` (SPEC.md §8).
    pub fn this_program() -> io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            prefix: vec![fake_ssh::EMBEDDED_FLAG.into()],
        })
    }
}

pub struct Sandbox {
    pub dir: PathBuf,
    pub paths: Paths,
    pub ssh: Tool,
    /// Sample port → the port it got (moved when busy here).
    pub ports: Vec<(u16, u16)>,
}

impl Sandbox {
    /// Builds a fresh sandbox under `root` (the system temp dir in the app).
    pub fn build(root: &Path, fake: &FakeSsh, reduce_motion: bool) -> io::Result<Self> {
        let dir = root.join(format!("{PREFIX}{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        let paths = Paths::new(&dir.join("home"), dir.join("app"));
        fs::create_dir_all(&paths.ssh_dir)?;
        fs::create_dir_all(&paths.app_dir)?;
        fs::write(&paths.ssh_config, SSH_CONFIG)?;
        fs::write(paths.ssh_dir.join("known_hosts"), "")?;

        let ports: Vec<(u16, u16)> = SAMPLE_PORTS.iter().map(|&p| (p, free_or(p))).collect();
        let mut tunnels = TUNNELS.to_string();
        for (from, to) in &ports {
            tunnels = tunnels.replace(&format!("listen = {from}"), &format!("listen = {to}"));
        }
        fs::write(paths.app_dir.join("tunnels.toml"), tunnels)?;
        fs::write(paths.app_dir.join("hosts.toml"), HOSTS)?;
        fs::write(
            paths.app_dir.join("settings.toml"),
            format!("version = 1\nreduce_motion = {reduce_motion}\ntour = \"done\"\n"),
        )?;

        let fake_dir = dir.join("fake-ssh");
        fs::create_dir_all(fake_dir.join("resolved"))?;
        let scenarios = fake_dir.join("scenarios.toml");
        fs::write(&scenarios, SCENARIOS)?;
        for (alias, hostname, user, jump) in SAMPLE_HOSTS {
            fs::write(
                fake_dir.join("resolved").join(alias),
                resolved(alias, hostname, user, jump),
            )?;
        }

        let ssh = Tool {
            program: fake.program.clone(),
            prefix: fake.prefix.clone(),
            env: vec![("FAKE_SSH_SCENARIOS".into(), scenarios.into_os_string())],
        };
        Ok(Self {
            dir,
            paths,
            ssh,
            ports,
        })
    }

    /// The port sample port `port` got.
    pub fn port(&self, port: u16) -> u16 {
        self.ports
            .iter()
            .find(|(from, _)| *from == port)
            .map_or(port, |(_, to)| *to)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Deletes sandboxes a crashed run left in `root` (any but this process's).
pub fn clean_leftovers(root: &Path) {
    let own = format!("{PREFIX}{}", std::process::id());
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(PREFIX) && name != own {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// `port` if it is free here, else a free one.
fn free_or(port: u16) -> u16 {
    if TcpListener::bind(("127.0.0.1", port)).is_ok() {
        return port;
    }
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map_or(port, |a| a.port())
}

/// What `ssh -G <alias>` prints for a sample host.
fn resolved(alias: &str, hostname: &str, user: &str, jump: Option<&str>) -> String {
    let mut text = format!(
        "host {alias}\nuser {user}\nhostname {hostname}\nport 22\naddressfamily any\n\
         batchmode no\ncompression no\nconnecttimeout none\nforwardagent no\n\
         identitiesonly no\nloglevel INFO\nserveraliveinterval 30\nserveralivecountmax 3\n\
         stricthostkeychecking ask\nuserknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2\n\
         identityfile ~/.ssh/id_ed25519\n"
    );
    if let Some(jump) = jump {
        text.push_str(&format!("proxyjump {jump}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_holds_the_sample_and_is_deleted() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeSsh {
            program: PathBuf::from("sshelf"),
            prefix: vec!["--fake-ssh".into()],
        };
        let sandbox = Sandbox::build(root.path(), &fake, true).unwrap();
        assert!(sandbox.dir.starts_with(root.path()));
        let config = fs::read_to_string(&sandbox.paths.ssh_config).unwrap();
        assert!(config.contains("Host backup-nas"));
        let resolved =
            fs::read_to_string(sandbox.dir.join("fake-ssh/resolved/db-primary")).unwrap();
        assert!(resolved.contains("proxyjump bastion"));
        assert_eq!(sandbox.ssh.prefix, ["--fake-ssh"]);
        assert_eq!(sandbox.ssh.env[0].0, "FAKE_SSH_SCENARIOS");
        let tunnels = fs::read_to_string(sandbox.paths.app_dir.join("tunnels.toml")).unwrap();
        assert!(tunnels.contains(&format!("listen = {}", sandbox.port(15432))));
        let dir = sandbox.dir.clone();
        drop(sandbox);
        assert!(!dir.exists());
    }

    #[test]
    fn a_busy_sample_port_is_moved() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert_ne!(free_or(port), port);
    }

    #[test]
    fn leftovers_of_other_runs_are_deleted() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("sshelf-tour-1");
        let other = root.path().join("unrelated");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&other).unwrap();
        clean_leftovers(root.path());
        assert!(!old.exists());
        assert!(other.exists());
    }
}
