//! Plain-language hints for well-known ssh failure messages, shown under the
//! raw message (which is always kept).

use crate::agent_service::AgentService;
use crate::ssh::AgentStatus;

/// Explanation for a tunnel whose key was not accepted (SPEC.md §8), using
/// what is known about the agent.
pub fn auth_hint(agent: Option<&AgentStatus>, service: Option<&AgentService>) -> String {
    let mut text = String::from(
        "Key not accepted. Tunnels run without a terminal (BatchMode), so ssh can only use \
         keys loaded in ssh-agent or key files without a passphrase. Load your key with \
         `ssh-add <key file>`.",
    );
    let agent_note = match (agent, service) {
        (_, Some(service)) if !service.is_running() => {
            Some("The ssh-agent service is not running; see Settings.".to_string())
        }
        (Some(AgentStatus::Keys(0)), _) => {
            Some("The agent is running but has no keys loaded.".to_string())
        }
        (Some(AgentStatus::Keys(n)), _) => Some(format!(
            "The agent has {n} key(s), but the server accepted none of them: \
             check User and IdentityFile for this host."
        )),
        (Some(AgentStatus::Unreachable(_)), _) => Some("ssh-agent is not reachable.".to_string()),
        _ => None,
    };
    if let Some(note) = agent_note {
        text.push(' ');
        text.push_str(&note);
    }
    text
}

/// A hint for ssh's failure `message`, if it is a well-known one.
pub fn failure_hint(message: &str) -> Option<&'static str> {
    const HINTS: &[(&str, &str)] = &[
        (
            "remote port forwarding failed",
            "The server could not listen on that port: it is probably already in use there, \
             or the server does not allow forwarding (AllowTcpForwarding, GatewayPorts).",
        ),
        (
            "Could not request local forwarding",
            "A local port is already in use by another program.",
        ),
        (
            "Address already in use",
            "A local port is already in use by another program.",
        ),
        (
            "Could not resolve hostname",
            "The host name could not be resolved: check HostName or your DNS/VPN.",
        ),
        (
            "timed out",
            "The server did not answer: it may be down, or a firewall or VPN is in the way.",
        ),
        (
            "Connection refused",
            "Nothing accepts ssh connections at that address and port.",
        ),
        (
            "Network is unreachable",
            "No route to the server: check your network or VPN.",
        ),
    ];
    HINTS
        .iter()
        .find(|(needle, _)| message.contains(needle))
        .map(|(_, hint)| *hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_failures_get_hints() {
        let remote = "Error: remote port forwarding failed for listen port 6443";
        assert!(
            failure_hint(remote)
                .unwrap()
                .contains("already in use there")
        );
        let local = "bind [127.0.0.1]:5432: Address already in use";
        assert!(failure_hint(local).unwrap().contains("local port"));
        let dns = "ssh: Could not resolve hostname nope: Name or service not known";
        assert!(failure_hint(dns).unwrap().contains("HostName"));
        let timeout = "ssh: connect to host 10.0.0.1 port 22: Connection timed out";
        assert!(failure_hint(timeout).unwrap().contains("did not answer"));
    }

    #[test]
    fn auth_hint_reflects_the_agent() {
        use crate::agent_service::StartType;

        let stopped = AgentService::Installed {
            running: false,
            start_type: StartType::Disabled,
        };
        let running = AgentService::Installed {
            running: true,
            start_type: StartType::Automatic,
        };
        assert!(auth_hint(None, None).contains("ssh-add"));
        assert!(
            auth_hint(Some(&AgentStatus::Keys(2)), Some(&stopped))
                .contains("service is not running")
        );
        assert!(auth_hint(Some(&AgentStatus::Keys(0)), Some(&running)).contains("no keys loaded"));
        assert!(auth_hint(Some(&AgentStatus::Keys(2)), None).contains("accepted none"));
        assert!(
            auth_hint(Some(&AgentStatus::Unreachable("x".into())), None).contains("not reachable")
        );
    }

    #[test]
    fn unknown_failures_have_no_hint() {
        assert_eq!(failure_hint("something else entirely"), None);
        // Auth and host key failures get dedicated handling (SPEC.md §8, §9).
        assert_eq!(failure_hint("web: Permission denied (publickey)."), None);
    }
}
