//! Shared by the test binaries that open tunnel ports.

use std::net::TcpListener;
use std::sync::Mutex;

/// The ports the OS hands out on its own (to any outgoing connection, or
/// `bind(0)` anywhere on the machine): Linux says which in
/// `ip_local_port_range` (CI runners may widen it); macOS and Windows use
/// 49152–65535.
fn ephemeral() -> (u16, u16) {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        let mut ports = text.split_whitespace().filter_map(|p| p.parse().ok());
        if let (Some(low), Some(high)) = (ports.next(), ports.next()) {
            return (low, high);
        }
    }
    (49_152, 65_535)
}

/// Where test ports come from: outside the ephemeral range, so a port a
/// test released can't be handed to some other socket before the tunnel
/// binds it. 20000–32000 when that is clear of it, else the larger span
/// below or above it.
fn test_range() -> (u16, u16) {
    const FIRST: u16 = 20_000;
    const LAST: u16 = 32_000;
    let (low, high) = ephemeral();
    if LAST <= low || FIRST > high {
        return (FIRST, LAST);
    }
    let below = (1_024, low);
    let above = (high.saturating_add(1), u16::MAX);
    let span = |(a, b): (u16, u16)| b.saturating_sub(a);
    let range = if span(below) >= span(above) {
        below
    } else {
        above
    };
    assert!(
        span(range) >= 200,
        "no room for test ports outside the ephemeral range {low}–{high}"
    );
    range
}

/// A listener on a free port that no other test in this binary got, and the
/// port. Drop the listener to free the port for the tunnel that uses it.
/// Ports that won't bind (held by something, or reserved by Windows) are
/// skipped.
pub fn hold_port() -> (TcpListener, u16) {
    // (range, next port): the start is spread by process id, so binaries
    // run one after another don't start on ports a previous one may still
    // be releasing.
    static NEXT: Mutex<Option<((u16, u16), u16)>> = Mutex::new(None);
    let mut state = NEXT.lock().unwrap_or_else(|e| e.into_inner());
    let ((first, last), mut port) = state.unwrap_or_else(|| {
        let (first, last) = test_range();
        let start = first + (std::process::id() % u32::from(last - first)) as u16;
        ((first, last), start)
    });
    for _ in first..last {
        let candidate = port;
        port = if port + 1 >= last { first } else { port + 1 };
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", candidate)) {
            *state = Some(((first, last), port));
            return (listener, candidate);
        }
    }
    panic!("no free test port in {first}..{last}");
}
