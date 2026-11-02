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
//! - `-N … <alias>`: a tunnel. Binds every `-L`/`-D` port (exits like
//!   `ExitOnForwardFailure` if one is taken), logs like `ssh -v`, then:
//!   - `ok*`: stays up after `Entering interactive session.`
//!   - `slow*`: the same after 1.5 s
//!   - `fail-auth*`: `Permission denied (publickey).`, exit 255
//!   - `die-later*`: up, then the connection drops after 1 s (exit 255)
//!   - `noisy*`: 500 log lines first, then up

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

    if args.iter().any(|a| a == "-N") {
        return tunnel(&args);
    }

    let code = env::var("FAKE_SSH_EXIT")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0);
    ExitCode::from(code)
}

fn tunnel(args: &[String]) -> ExitCode {
    let alias = args.last().map(String::as_str).unwrap_or_default();
    eprintln!("debug1: Connecting to {alias} [192.0.2.1] port 22.");
    if alias.starts_with("fail-auth") {
        eprintln!("{alias}: Permission denied (publickey).");
        return ExitCode::from(255);
    }
    if alias.starts_with("slow") {
        thread::sleep(Duration::from_millis(1500));
    }
    if alias.starts_with("noisy") {
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

    if alias.starts_with("die-later") {
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
}
