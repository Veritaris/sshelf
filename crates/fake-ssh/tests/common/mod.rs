//! Shared by the test binaries that open tunnel ports.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::sync::Mutex;

/// A listener on a free port that no other test in this binary got, and the
/// port. Tests run in parallel, and the OS hands a just-released ephemeral
/// port out again (macOS quickly), so two tests asking for "any free port"
/// could otherwise get the same number. Drop the listener to free the port
/// for the tunnel that uses it.
pub fn hold_port() -> (TcpListener, u16) {
    static GIVEN: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());
    loop {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        if GIVEN.lock().unwrap_or_else(|e| e.into_inner()).insert(port) {
            return (listener, port);
        }
    }
}
