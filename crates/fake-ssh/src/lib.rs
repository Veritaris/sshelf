//! Scriptable stand-in for the system `ssh` binary.
//!
//! Tests spawn its thin binary instead of real `ssh`, and the onboarding tour
//! runs it embedded as `sshelf --fake-ssh …` (SPEC.md §8, §10). Both call
//! [`run`]. It is steered through arguments, env and, for the tour, a scenario
//! file.
//!
//! - `--as <name>` (first): act as the companion tool `name` (`ssh-add`,
//!   `ssh-keygen`), as the binary does when copied under that name.
//! - `FAKE_SSH_SCENARIOS=<file>`: friendly host names (see [`Scenarios`]).
//!   A host listed there behaves like the alias prefix of its `scenario`;
//!   every other host still goes by its own prefix.
//! - `FAKE_SSH_LISTEN=<port>`: hold `127.0.0.1:<port>` while running.
//! - `FAKE_SSH_CHILD_LISTEN=<port>`: start a child copy that holds the port
//!   and sleeps (process-tree tests).
//! - `FAKE_SSH_SLEEP_MS=<n>`: sleep first (timeouts).
//! - `FAKE_SSH_STDERR=<text>`: print to stderr.
//! - `FAKE_SSH_EXIT=<code>`: exit code (default 0).
//! - `-V`: print a version line on stderr, like ssh.
//! - `-G [--] <alias>`: print a resolved config for `alias` (from the
//!   scenario bundle's `resolved/<alias>` when there is one); aliases starting
//!   with `fail` exit 255 with an error, like a bad config. With `-F <file>`,
//!   each `Bogus …` line of the file is reported as a bad option (exit 255).
//! - `--record <file>`: append all arguments (one line, `\x1f`-separated) to
//!   `file`, so tests can see how it was launched.
//! - `… <alias> true` (health check): succeeds, except `fail-auth*` (key not
//!   accepted), `changed*`, `unknownkey*` (host key) and `unreachable*`.
//! - `FAKE_SSH_RECORD=<file>`: the same, prefixed with the name it runs
//!   under. As `ssh-add`, it acts like `ssh-add <key>` succeeding, and
//!   `ssh-add -l` reports an empty agent.
//! - `-N … <alias>`: a tunnel. Binds every `-L`/`-D` port (exits like
//!   `ExitOnForwardFailure` if one is taken), logs like `ssh -v`, then:
//!   - `ok*`: stays up after `Entering interactive session.`
//!   - `slow*`: the same after 1.5 s
//!   - `fail-auth*`: `Permission denied (publickey).`, exit 255
//!   - `die-later*`: up, then the connection drops after 1 s (exit 255)
//!   - `noisy*`: 500 log lines first, then up
//!   - `changed*`: the "REMOTE HOST IDENTIFICATION HAS CHANGED" failure
//!   - `unknownkey*`: `Host key verification failed.` (unknown key)
//! - Host keys: `-G` prints `~/.ssh/known_hosts` paths, and a `proxyjump`
//!   `<prefix>-jump` for `<prefix>-via-*` aliases. A key fetch
//!   (`-o UserKnownHostsFile=… -o StrictHostKeyChecking=accept-new … exit`)
//!   writes [`NEW_KEY`] for the host, then fails auth like a real server;
//!   `unreachable*` hosts time out.
//! - Key install (`… -- <alias> "…echo '<key>' >> ~/.ssh/authorized_keys…"`):
//!   appends the key to the scenario bundle's `servers/<alias>/authorized_keys`.
//!   A host with `after_install` behaves like that scenario once its file
//!   holds a key.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::Duration;

/// What `-V` prints.
pub const VERSION_LINE: &str = "OpenSSH_fake_1.0, fake-ssh";
/// The first argument that makes `sshelf` run this instead of its window.
pub const EMBEDDED_FLAG: &str = "--fake-ssh";
/// Public part of a throwaway key: the "server's current" host key.
pub const NEW_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/867Gn6gWVC9L/2TosddbUBIfUwN1FA755XCQA20kG";

fn env_port(name: &str) -> Option<u16> {
    env::var(name).ok().and_then(|v| v.parse().ok())
}

/// One sample host of a scenario file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostScenario {
    /// Behaves like an alias with this prefix (`ok`, `fail-auth`, …).
    pub scenario: String,
    /// Once `servers/<alias>/authorized_keys` holds a key, this instead.
    pub after_install: Option<String>,
    /// Waits this long before answering a health check or bringing a tunnel
    /// up, so progress is visible.
    pub delay_ms: u64,
}

/// The scenario file (`FAKE_SSH_SCENARIOS`): a small TOML subset,
///
/// ```toml
/// [hosts.backup-nas]
/// scenario = "fail-auth"
/// after_install = "ok"
/// delay_ms = 400
/// ```
///
/// Next to it: `resolved/<alias>` (what `-G` prints) and
/// `servers/<alias>/authorized_keys` (written by a key install).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scenarios {
    /// The file's folder.
    pub dir: PathBuf,
    pub hosts: Vec<(String, HostScenario)>,
}

impl Scenarios {
    pub fn parse(dir: PathBuf, text: &str) -> Self {
        let mut hosts: Vec<(String, HostScenario)> = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if let Some(section) = line
                .strip_prefix("[hosts.")
                .and_then(|l| l.strip_suffix(']'))
            {
                hosts.push((section.trim().to_string(), HostScenario::default()));
                continue;
            }
            let (Some((_, host)), Some((key, value))) = (hosts.last_mut(), line.split_once('='))
            else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_string();
            match key.trim() {
                "scenario" => host.scenario = value,
                "after_install" => host.after_install = Some(value),
                "delay_ms" => host.delay_ms = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        Self { dir, hosts }
    }

    /// From `FAKE_SSH_SCENARIOS`, if set and readable.
    pub fn from_env() -> Option<Self> {
        let path = PathBuf::from(env::var_os("FAKE_SSH_SCENARIOS")?);
        let text = fs::read_to_string(&path).ok()?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        Some(Self::parse(dir, &text))
    }

    pub fn host(&self, alias: &str) -> Option<&HostScenario> {
        self.hosts.iter().find(|(a, _)| a == alias).map(|(_, h)| h)
    }

    /// The fake `authorized_keys` of `alias`.
    pub fn authorized_keys(&self, alias: &str) -> PathBuf {
        self.dir.join("servers").join(alias).join("authorized_keys")
    }

    /// What `alias` behaves like: its scenario (or `after_install` once a key
    /// is installed), else the alias itself (prefix matching).
    pub fn behaviour(&self, alias: &str) -> String {
        let Some(host) = self.host(alias) else {
            return alias.to_string();
        };
        let installed = fs::read_to_string(self.authorized_keys(alias))
            .is_ok_and(|keys| keys.lines().any(|l| !l.trim().is_empty()));
        match &host.after_install {
            Some(after) if installed => after.clone(),
            _ => host.scenario.clone(),
        }
    }

    fn delay(&self, alias: &str) {
        if let Some(host) = self.host(alias).filter(|h| h.delay_ms > 0) {
            thread::sleep(Duration::from_millis(host.delay_ms));
        }
    }
}

/// Runs fake-ssh with `args` (without the program name). The binary and
/// `sshelf --fake-ssh` both end up here.
pub fn run(mut args: Vec<String>) -> ExitCode {
    // `--as <name>`: act as a companion tool.
    let mut name = env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    if args.first().map(String::as_str) == Some("--as") && args.len() >= 2 {
        name = args.remove(1);
        args.remove(0);
    }
    let scenarios = Scenarios::from_env().unwrap_or_default();

    let _held = env_port("FAKE_SSH_LISTEN")
        .map(|port| TcpListener::bind(("127.0.0.1", port)).expect("binding FAKE_SSH_LISTEN"));
    if let Some(port) = env_port("FAKE_SSH_CHILD_LISTEN") {
        // Left running on purpose: tests check that stopping the tree kills it.
        #[expect(clippy::zombie_processes, reason = "killed with the process tree")]
        let _child = Command::new(env::current_exe().expect("own path"))
            // Embedded in sshelf, this is what selects fake-ssh; the test
            // binary skips it.
            .arg(EMBEDDED_FLAG)
            .env_remove("FAKE_SSH_CHILD_LISTEN")
            .env("FAKE_SSH_LISTEN", port.to_string())
            .env("FAKE_SSH_SLEEP_MS", "600000")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawning the child");
    }

    if let Some(ms) = env::var("FAKE_SSH_SLEEP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        thread::sleep(Duration::from_millis(ms));
    }
    if let Ok(text) = env::var("FAKE_SSH_STDERR") {
        eprintln!("{text}");
    }
    if let Some(i) = args.iter().position(|a| a == "--record")
        && let Some(path) = args.get(i + 1)
    {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("opening the record file");
        // One write per line, as below: a reader never sees half of it.
        let line = format!("{}\n", args.join("\x1f"));
        file.write_all(line.as_bytes())
            .expect("writing the record file");
    }
    if let Ok(path) = env::var("FAKE_SSH_RECORD") {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("opening FAKE_SSH_RECORD");
        let words: Vec<&str> = std::iter::once(name.as_str())
            .chain(args.iter().map(String::as_str))
            .collect();
        // One write per line: runs that record at the same time (an
        // `ssh-add -l` beside an `ssh-add <key>`) must not interleave, and
        // `writeln!` writes the text and the newline separately.
        let line = format!("{}\n", words.join("\x1f"));
        file.write_all(line.as_bytes())
            .expect("writing FAKE_SSH_RECORD");
    }
    if name == "ssh-add" {
        return ssh_add(&args);
    }
    if name == "ssh-keygen" {
        return ssh_keygen(&args);
    }

    if args.iter().any(|a| a == "-V") {
        eprintln!("{VERSION_LINE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-G") {
        let alias = args.last().map(String::as_str).unwrap_or_default();
        if let Some(code) = check_config(&args) {
            return code;
        }
        let behaviour = scenarios.behaviour(alias);
        if behaviour.starts_with("fail") && scenarios.host(alias).is_none() {
            eprintln!("fake-ssh: cannot resolve {alias}: line 1: Bad configuration option");
            return ExitCode::from(255);
        }
        match fs::read_to_string(scenarios.dir.join("resolved").join(alias)) {
            Ok(text) if scenarios.host(alias).is_some() => print!("{text}"),
            _ => {
                println!("host {alias}\nhostname {alias}.example.com\nport 22\nuser fake");
                println!("identityfile ~/.ssh/id_ed25519\nidentityfile ~/.ssh/id_rsa");
                println!("userknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2");
                println!("globalknownhostsfile ~/.ssh/global_known_hosts");
                if let Some((prefix, _)) = alias.split_once("-via-") {
                    println!("proxyjump {prefix}-jump");
                }
            }
        }
    }
    if let Some(code) = fetch_key(&args) {
        return code;
    }
    if let Some(code) = install_key(&args, &scenarios) {
        return code;
    }

    if args.iter().any(|a| a == "-N") {
        return tunnel(&args, &scenarios);
    }
    if args.last().map(String::as_str) == Some("true") {
        return health(&args, &scenarios);
    }

    let code = env::var("FAKE_SSH_EXIT")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0);
    ExitCode::from(code)
}

/// `ssh-add`: `-l` reports an empty agent (exit 1, like ssh-add); anything
/// else adds the key named last.
fn ssh_add(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-l") {
        println!("The agent has no identities.");
        return ExitCode::from(1);
    }
    let key = args.last().map(String::as_str).unwrap_or_default();
    eprintln!("Identity added: {key} (fake)");
    ExitCode::SUCCESS
}

/// `ssh-keygen`: `-F <host> -f <file>` looks the host up in `file` like the
/// real one (a line whose first field names it); `-lf` prints a fake
/// fingerprint; `-R` removes nothing and succeeds.
fn ssh_keygen(args: &[String]) -> ExitCode {
    let value = |flag: &str| {
        args.windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].as_str())
    };
    if let Some(host) = value("-F") {
        let text = value("-f")
            .and_then(|f| fs::read_to_string(f).ok())
            .unwrap_or_default();
        let found: Vec<(usize, &str)> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                l.split_whitespace()
                    .next()
                    .is_some_and(|names| names.split(',').any(|n| n == host))
            })
            .collect();
        for (i, line) in &found {
            println!("# Host {host} found: line {}", i + 1);
            println!("{line}");
        }
        return ExitCode::from(if found.is_empty() { 1 } else { 0 });
    }
    if value("-lf").is_some() {
        println!("256 SHA256:fakefakefakefakefakefakefakefakefakefakefake fake-ssh (ED25519)");
    }
    ExitCode::SUCCESS
}

/// `-G -F <file>`: every line whose first word is `Bogus` is reported like
/// ssh reports an unknown option, and ssh's exit code 255 follows.
fn check_config(args: &[String]) -> Option<ExitCode> {
    let file = args
        .iter()
        .position(|a| a == "-F")
        .and_then(|i| args.get(i + 1))?;
    let text = fs::read_to_string(file).unwrap_or_default();
    let bad: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.split_whitespace().next() == Some("Bogus"))
        .map(|(i, _)| i + 1)
        .collect();
    for line in &bad {
        eprintln!("{file}: line {line}: Bad configuration option: bogus");
    }
    if bad.is_empty() {
        return None;
    }
    eprintln!(
        "{file}: terminating, {} bad configuration options",
        bad.len()
    );
    Some(ExitCode::from(255))
}

/// A health check, `… -- <alias> true` (SPEC.md §12, M8), answered by alias.
fn health(args: &[String], scenarios: &Scenarios) -> ExitCode {
    let alias = args
        .len()
        .checked_sub(2)
        .and_then(|i| args.get(i))
        .map(String::as_str)
        .unwrap_or_default();
    scenarios.delay(alias);
    let behaviour = scenarios.behaviour(alias);
    if behaviour.starts_with("fail-auth") {
        eprintln!("{alias}.example.com: Permission denied (publickey).");
    } else if behaviour.starts_with("changed") {
        eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
        eprintln!("@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @");
        eprintln!("Host key verification failed.");
    } else if behaviour.starts_with("unknownkey") {
        eprintln!("Host key verification failed.");
    } else if behaviour.starts_with("unreachable") {
        eprintln!("ssh: connect to host {alias}.example.com port 22: Connection timed out");
    } else {
        return ExitCode::SUCCESS;
    }
    ExitCode::from(255)
}

/// Value of `-o Name=value` (quotes removed).
fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .filter(|w| w[0] == "-o")
        .find_map(|w| w[1].strip_prefix(name)?.strip_prefix('='))
        .map(|v| v.trim_matches('"'))
}

/// The destination after `--`.
fn destination(args: &[String]) -> Option<&String> {
    args.iter()
        .position(|a| a == "--")
        .and_then(|i| args.get(i + 1))
}

/// A host key fetch (SPEC.md §9): writes the "server's" key to the temp file.
fn fetch_key(args: &[String]) -> Option<ExitCode> {
    if option(args, "StrictHostKeyChecking") != Some("accept-new") {
        return None;
    }
    let file = option(args, "UserKnownHostsFile")?;
    let destination = destination(args)?;
    let host = destination.rsplit('@').next().unwrap_or(destination);
    if host.starts_with("unreachable") {
        eprintln!("ssh: connect to host {host}.example.com port 22: Connection timed out");
        return Some(ExitCode::from(255));
    }
    {
        let port = args
            .windows(2)
            .find(|w| w[0] == "-p")
            .and_then(|w| w[1].parse::<u16>().ok())
            .unwrap_or(22);
        let name = if port == 22 {
            format!("{host}.example.com")
        } else {
            format!("[{host}.example.com]:{port}")
        };
        fs::write(file, format!("{name} {NEW_KEY}\n")).expect("writing the fetched key");
        eprintln!("Warning: Permanently added '{name}' (ED25519) to the list of known hosts.");
    }
    eprintln!("{destination}: Permission denied (publickey).");
    Some(ExitCode::from(255))
}

/// The key in a key install command (`… echo '<key>' >> …authorized_keys…`).
fn installed_key(command: &str) -> Option<&str> {
    if !command.contains("authorized_keys") {
        return None;
    }
    let start = command.find("echo '")? + "echo '".len();
    let len = command[start..].find('\'')?;
    Some(&command[start..start + len])
}

/// A key install (SPEC.md §11) on a scenario host: the key goes into its fake
/// `authorized_keys`, once.
fn install_key(args: &[String], scenarios: &Scenarios) -> Option<ExitCode> {
    let alias = destination(args)?;
    let key = args.last().and_then(|c| installed_key(c))?;
    scenarios.host(alias)?;
    let file = scenarios.authorized_keys(alias);
    let existing = fs::read_to_string(&file).unwrap_or_default();
    if !existing.lines().any(|l| l == key) {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).expect("creating the fake server");
        }
        let mut out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .expect("opening the fake authorized_keys");
        writeln!(out, "{key}").expect("writing the fake authorized_keys");
    }
    eprintln!("The key is installed on {alias}.");
    Some(ExitCode::SUCCESS)
}

fn tunnel(args: &[String], scenarios: &Scenarios) -> ExitCode {
    let alias = args.last().map(String::as_str).unwrap_or_default();
    let behaviour = scenarios.behaviour(alias);
    eprintln!("debug1: Connecting to {alias} [192.0.2.1] port 22.");
    scenarios.delay(alias);
    if behaviour.starts_with("changed") {
        eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
        eprintln!("@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @");
        eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
        eprintln!("IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!");
        eprintln!(
            "Host key for {alias}.example.com has changed and you have requested strict checking."
        );
        eprintln!("Host key verification failed.");
        return ExitCode::from(255);
    }
    if behaviour.starts_with("unknownkey") {
        eprintln!("Host key verification failed.");
        return ExitCode::from(255);
    }
    if behaviour.starts_with("fail-auth") {
        eprintln!("{alias}: Permission denied (publickey).");
        return ExitCode::from(255);
    }
    if behaviour.starts_with("unreachable") {
        eprintln!("ssh: connect to host {alias}.example.com port 22: Connection timed out");
        return ExitCode::from(255);
    }
    if behaviour.starts_with("slow") {
        thread::sleep(Duration::from_millis(1500));
    }
    if behaviour.starts_with("noisy") {
        for i in 0..500 {
            eprintln!("debug1: noisy line {i}");
        }
    }
    eprintln!("Authenticated to {alias} ([192.0.2.1]:22) using \"publickey\".");

    let mut listeners = Vec::new();
    for (flag, spec) in args.iter().zip(args.iter().skip(1)) {
        let listen = match flag.as_str() {
            "-L" => listen_of(spec, 2),
            "-D" => listen_of(spec, 0),
            "-R" => {
                eprintln!("debug1: remote forward success for: listen {spec}");
                continue;
            }
            _ => continue,
        };
        let Some((addr, port)) = listen else {
            eprintln!("Bad forwarding specification '{spec}'");
            return ExitCode::from(255);
        };
        match TcpListener::bind((addr.as_str(), port)) {
            Ok(listener) => {
                eprintln!("debug1: Local forwarding listening on {addr} port {port}.");
                listeners.push(listener);
            }
            Err(_) => {
                eprintln!("bind [{addr}]:{port}: Address already in use");
                eprintln!("channel_setup_fwd_listener_tcpip: cannot listen to port: {port}");
                eprintln!("Could not request local forwarding.");
                return ExitCode::from(255);
            }
        }
    }
    eprintln!("debug1: Entering interactive session.");

    if behaviour.starts_with("die-later") {
        thread::sleep(Duration::from_millis(1000));
        eprintln!("Connection to {alias} closed by remote host.");
        return ExitCode::from(255);
    }
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// Listen address and port of a `-L`/`-D` spec whose last `target_parts`
/// `:`-separated parts are the target.
fn listen_of(spec: &str, target_parts: usize) -> Option<(String, u16)> {
    let (bind, rest) = match spec.strip_prefix('[') {
        Some(inner) => {
            let (bind, rest) = inner.split_once("]:")?;
            (Some(bind.to_string()), rest)
        }
        None => (None, spec),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let listen = parts.len().checked_sub(target_parts)?;
    let (bind, port) = match (bind, &parts[..listen]) {
        (Some(bind), [port]) => (bind, *port),
        (None, [port]) => ("127.0.0.1".to_string(), *port),
        (None, [bind, port]) => (bind.to_string(), *port),
        _ => return None,
    };
    Some((bind, port.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(addr: &str, port: u16) -> Option<(String, u16)> {
        Some((addr.to_string(), port))
    }

    #[test]
    fn forward_specs() {
        assert_eq!(
            listen_of("127.0.0.1:6443:localhost:6443", 2),
            at("127.0.0.1", 6443)
        );
        assert_eq!(listen_of("6443:localhost:6443", 2), at("127.0.0.1", 6443));
        assert_eq!(listen_of("[::1]:6443:localhost:6443", 2), at("::1", 6443));
        assert_eq!(listen_of("1080", 0), at("127.0.0.1", 1080));
        assert_eq!(listen_of("0.0.0.0:1080", 0), at("0.0.0.0", 1080));
        assert_eq!(listen_of("[::1]:1080", 0), at("::1", 1080));
        assert_eq!(listen_of("x:y:z:w:v", 2), None);
        assert_eq!(listen_of("abc", 0), None);
    }

    const FILE: &str = "# sample\n\
        [hosts.web-01]\nscenario = \"ok\"\ndelay_ms = 5\n\n\
        [hosts.backup-nas]\nscenario = \"fail-auth\"  # until a key\nafter_install = \"ok\"\n";

    #[test]
    fn scenario_file() {
        let s = Scenarios::parse(PathBuf::from("d"), FILE);
        assert_eq!(
            s.host("web-01"),
            Some(&HostScenario {
                scenario: "ok".into(),
                after_install: None,
                delay_ms: 5,
            })
        );
        assert_eq!(
            s.host("backup-nas").unwrap().after_install.as_deref(),
            Some("ok")
        );
        // Not listed: prefix matching as before.
        assert_eq!(s.behaviour("fail-auth-x"), "fail-auth-x");
        assert_eq!(s.behaviour("web-01"), "ok");
        assert_eq!(
            s.authorized_keys("web-01"),
            PathBuf::from("d")
                .join("servers")
                .join("web-01")
                .join("authorized_keys")
        );
    }

    #[test]
    fn after_install_switches_once_a_key_is_installed() {
        let dir = std::env::temp_dir().join(format!("fake-ssh-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let s = Scenarios::parse(dir.clone(), FILE);
        assert_eq!(s.behaviour("backup-nas"), "fail-auth");

        let key = "ssh-ed25519 AAAAC3Nza test (sshelf)";
        let command = format!(
            "umask 077 && mkdir -p ~/.ssh && (grep -qxF '{key}' ~/.ssh/authorized_keys || \
             echo '{key}' >> ~/.ssh/authorized_keys)"
        );
        let args: Vec<String> = ["-t", "--", "backup-nas", &command]
            .map(String::from)
            .to_vec();
        assert_eq!(install_key(&args, &s), Some(ExitCode::SUCCESS));
        // Twice adds it once.
        install_key(&args, &s);
        let keys = fs::read_to_string(s.authorized_keys("backup-nas")).unwrap();
        assert_eq!(keys, format!("{key}\n"));
        assert_eq!(s.behaviour("backup-nas"), "ok");
        // Hosts without a scenario aren't servers of the bundle.
        let other: Vec<String> = ["-t", "--", "elsewhere", &command]
            .map(String::from)
            .to_vec();
        assert_eq!(install_key(&other, &s), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_command_key() {
        assert_eq!(
            installed_key("x && echo 'ssh-ed25519 AAA c' >> ~/.ssh/authorized_keys"),
            Some("ssh-ed25519 AAA c")
        );
        assert_eq!(installed_key("echo 'hi'"), None);
    }

    #[test]
    fn as_ssh_add_lists_an_empty_agent() {
        assert_eq!(
            run(vec!["--as".into(), "ssh-add".into(), "-l".into()]),
            ExitCode::from(1)
        );
        assert_eq!(
            run(vec!["--as".into(), "ssh-add".into(), "key".into()]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn as_ssh_keygen_finds_hosts() {
        let dir = std::env::temp_dir().join(format!("fake-ssh-kh-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("known_hosts");
        fs::write(&file, "a.example.com,b ssh-ed25519 AAAA\n").unwrap();
        let path = file.to_string_lossy().into_owned();
        let run_keygen = |host: &str| {
            run(["--as", "ssh-keygen", "-F", host, "-f", &path]
                .map(String::from)
                .to_vec())
        };
        assert_eq!(run_keygen("b"), ExitCode::SUCCESS);
        assert_eq!(run_keygen("c"), ExitCode::from(1));
        fs::remove_dir_all(&dir).unwrap();
    }
}
