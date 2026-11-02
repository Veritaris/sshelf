//! Plain-language hints for well-known ssh failure messages, shown under the
//! raw message (which is always kept).

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
    fn unknown_failures_have_no_hint() {
        assert_eq!(failure_hint("something else entirely"), None);
        // Auth and host key failures get dedicated handling (SPEC.md §8, §9).
        assert_eq!(failure_hint("web: Permission denied (publickey)."), None);
    }
}
