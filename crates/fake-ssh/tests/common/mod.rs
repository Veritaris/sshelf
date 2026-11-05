//! Shared by the test binaries that open tunnel ports.

use std::net::TcpListener;
use std::sync::Mutex;

/// Test ports come from below every OS's ephemeral range (Linux starts at
/// 32768, macOS and Windows at 49152). The OS hands ephemeral ports out on
/// its own (to any outgoing connection, or `bind(0)` anywhere on the
/// machine), so a just-released one can be taken again before the tunnel
/// binds it; ports here are only ever taken on purpose.
const FIRST: u16 = 20_000;
const LAST: u16 = 32_000;

/// A listener on a free port that no other test in this binary got, and the
/// port. Drop the listener to free the port for the tunnel that uses it.
/// Ports that won't bind (held by something, or reserved by Windows) are
/// skipped.
pub fn hold_port() -> (TcpListener, u16) {
    // Where this binary starts: spread by process id, so binaries run one
    // after another don't start on ports a previous one may still be
    // releasing.
    static NEXT: Mutex<Option<u16>> = Mutex::new(None);
    let mut next = NEXT.lock().unwrap_or_else(|e| e.into_inner());
    let span = LAST - FIRST;
    let mut port = next.unwrap_or_else(|| FIRST + (std::process::id() % u32::from(span)) as u16);
    for _ in 0..span {
        let candidate = port;
        port = if port + 1 >= LAST { FIRST } else { port + 1 };
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", candidate)) {
            *next = Some(port);
            return (listener, candidate);
        }
    }
    panic!("no free test port in {FIRST}..{LAST}");
}
