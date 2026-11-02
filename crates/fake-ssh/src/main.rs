//! Scriptable stand-in for the system `ssh` binary.
//!
//! Tests spawn this instead of real `ssh` and steer it through arguments and
//! env (SPEC.md §7). Grows with the milestones that need it.
//!
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
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

const VERSION_LINE: &str = "OpenSSH_fake_1.0, fake-ssh";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

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
