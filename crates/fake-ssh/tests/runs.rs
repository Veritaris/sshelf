//! Smoke test: the `fake-ssh` binary builds and can be spawned on every OS.

use std::process::Command;

#[test]
fn exits_successfully_with_ssh_like_args() {
    let status = Command::new(env!("CARGO_BIN_EXE_fake-ssh"))
        // No `-N`: that makes fake-ssh act as a long-running tunnel.
        .args(["-o", "BatchMode=yes", "example-host"])
        .status()
        .expect("spawning fake-ssh");
    assert!(status.success());
}
