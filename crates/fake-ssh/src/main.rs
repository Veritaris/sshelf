//! Scriptable stand-in for the system `ssh` binary.
//!
//! Tests spawn this instead of real `ssh` and steer it through arguments and
//! env (SPEC.md §7). Grows with the milestones that need it.
//!
//! - `FAKE_SSH_LISTEN=<port>`: hold `127.0.0.1:<port>` while running.
//! - `FAKE_SSH_CHILD_LISTEN=<port>`: start a child copy that holds the port
//!   and sleeps (process-tree tests).
//! - `FAKE_SSH_SLEEP_MS=<n>`: sleep first (timeouts).
//! - `FAKE_SSH_STDERR=<text>`: print to stderr.
//! - `FAKE_SSH_EXIT=<code>`: exit code (default 0).
//! - `-V`: print a version line on stderr, like ssh.
//! - `-G [--] <alias>`: print a resolved config for `alias`; aliases starting
//!   with `fail` exit 255 with an error, like a bad config.
//! - `--record <file>`: append all arguments (one line, `\x1f`-separated) to
//!   `file`, so tests can see how it was launched.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::Duration;

const VERSION_LINE: &str = "OpenSSH_fake_1.0, fake-ssh";

fn env_port(name: &str) -> Option<u16> {
    env::var(name).ok().and_then(|v| v.parse().ok())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    let _held = env_port("FAKE_SSH_LISTEN")
        .map(|port| TcpListener::bind(("127.0.0.1", port)).expect("binding FAKE_SSH_LISTEN"));
    if let Some(port) = env_port("FAKE_SSH_CHILD_LISTEN") {
        // Left running on purpose: tests check that stopping the tree kills it.
        #[expect(clippy::zombie_processes, reason = "killed with the process tree")]
        let _child = Command::new(env::current_exe().expect("own path"))
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
        writeln!(file, "{}", args.join("\x1f")).expect("writing the record file");
    }

    if args.iter().any(|a| a == "-V") {
        eprintln!("{VERSION_LINE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-G") {
        let alias = args.last().map(String::as_str).unwrap_or_default();
        if alias.starts_with("fail") {
            eprintln!("fake-ssh: cannot resolve {alias}: line 1: Bad configuration option");
            return ExitCode::from(255);
        }
        println!("host {alias}\nhostname {alias}.example.com\nport 22\nuser fake");
        println!("identityfile ~/.ssh/id_ed25519\nidentityfile ~/.ssh/id_rsa");
    }

    let code = env::var("FAKE_SSH_EXIT")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0);
    ExitCode::from(code)
}
