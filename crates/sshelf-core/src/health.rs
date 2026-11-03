//! Connection health per host (SPEC.md §12, M8): one non-interactive login,
//! `ssh -o BatchMode=yes -o ConnectTimeout=3 -- <alias> true`, through the
//! user's own config (jump hosts included). Only run when the user asks:
//! checking every host on its own would mean connecting to every server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::process::{self, RunError};
use crate::supervisor::{FailureKind, classify_lines};

/// `ConnectTimeout` per connection (each jump host has its own).
pub const CONNECT_TIMEOUT_SECS: u32 = 3;
/// The whole check, jump chains included.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// Checks running at once.
pub const MAX_PARALLEL: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Logged in with a key; how long the whole login took.
    Ok {
        millis: u64,
    },
    /// Reachable, but no offered key was accepted.
    AuthFailed,
    HostKeyUnknown,
    HostKeyChanged,
    /// Not reached; ssh's reason.
    Unreachable(String),
}

impl Health {
    pub fn describe(&self) -> String {
        match self {
            Health::Ok { millis } => format!("Auth OK, login took {millis} ms"),
            Health::AuthFailed => "Reachable, but no key was accepted".into(),
            Health::HostKeyUnknown => "Reachable, host key not known yet".into(),
            Health::HostKeyChanged => "Host key changed".into(),
            Health::Unreachable(why) => format!("Unreachable: {why}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    pub health: Health,
    pub checked_at: SystemTime,
}

/// Every result of this session (kept in memory only) and the aliases
/// queued or being checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub reports: BTreeMap<String, HealthReport>,
    pub checking: BTreeSet<String>,
}

pub fn args(alias: &str) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
        "--".into(),
        alias.into(),
        "true".into(),
    ]
}

/// What a finished check means.
pub fn classify(code: Option<i32>, stderr: &str, elapsed: Duration) -> Health {
    if code == Some(0) {
        return Health::Ok {
            millis: elapsed.as_millis() as u64,
        };
    }
    match classify_lines(stderr.lines()) {
        FailureKind::HostKeyChanged => Health::HostKeyChanged,
        FailureKind::HostKeyUnknown => Health::HostKeyUnknown,
        FailureKind::Auth => Health::AuthFailed,
        FailureKind::Other => Health::Unreachable(
            stderr
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("ssh exited with code {code:?}")),
        ),
    }
}

/// Runs one check (blocks up to [`CHECK_TIMEOUT`]).
pub fn check(ssh: &Path, alias: &str) -> HealthReport {
    let mut cmd = process::command(ssh);
    cmd.args(args(alias));
    let started = Instant::now();
    let health = match process::run(cmd, CHECK_TIMEOUT) {
        Ok(out) => classify(out.code, &out.stderr, started.elapsed()),
        Err(RunError::Timeout { .. }) => {
            Health::Unreachable(format!("no answer within {} s", CHECK_TIMEOUT.as_secs()))
        }
        Err(e) => Health::Unreachable(e.to_string()),
    };
    HealthReport {
        health,
        checked_at: SystemTime::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_are_classified_like_tunnel_failures() {
        let ms = Duration::from_millis(240);
        assert_eq!(classify(Some(0), "", ms), Health::Ok { millis: 240 });
        assert_eq!(
            classify(Some(255), "web: Permission denied (publickey).\n", ms),
            Health::AuthFailed
        );
        assert_eq!(
            classify(Some(255), "Host key verification failed.\n", ms),
            Health::HostKeyUnknown
        );
        assert_eq!(
            classify(
                Some(255),
                "@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\nHost key verification failed.\n",
                ms
            ),
            Health::HostKeyChanged
        );
        assert_eq!(
            classify(
                Some(255),
                "ssh: connect to host w port 22: Connection timed out\r\n\n",
                ms
            ),
            Health::Unreachable("ssh: connect to host w port 22: Connection timed out".into())
        );
        assert_eq!(
            classify(Some(1), "", ms),
            Health::Unreachable("ssh exited with code Some(1)".into())
        );
    }

    #[test]
    fn the_check_never_prompts() {
        let args = args("web");
        assert!(args.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
        assert_eq!(&args[args.len() - 3..], ["--", "web", "true"]);
    }
}
