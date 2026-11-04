//! Process trees: stopping a tunnel stops everything it started.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use sshelf_core::child::{self, KILL_GRACE};

mod common;

fn fake_ssh() -> Command {
    Command::new(PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh")))
}

/// A free port no other test in this binary got.
fn free_port() -> u16 {
    common::hold_port().1
}

fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn wait_until(what: &str, limit: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn kill_stops_the_process() {
    let mut cmd = fake_ssh();
    cmd.env("FAKE_SSH_SLEEP_MS", "600000");
    let mut spawned = child::spawn_tree(cmd).unwrap();
    let started = Instant::now();
    spawned.killer.kill();
    spawned.child.wait().unwrap();
    assert!(
        started.elapsed() < KILL_GRACE * 3,
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn kill_stops_grandchildren_too() {
    let port = free_port();
    let mut cmd = fake_ssh();
    cmd.env("FAKE_SSH_CHILD_LISTEN", port.to_string())
        .env("FAKE_SSH_SLEEP_MS", "600000");
    let mut spawned = child::spawn_tree(cmd).unwrap();
    wait_until(
        "the grandchild to hold the port",
        Duration::from_secs(10),
        || !port_is_free(port),
    );

    spawned.killer.kill();
    spawned.child.wait().unwrap();
    wait_until("the grandchild to exit", KILL_GRACE * 5, || {
        port_is_free(port)
    });
}

#[test]
fn dropping_the_killer_stops_the_tree() {
    let port = free_port();
    let mut cmd = fake_ssh();
    cmd.env("FAKE_SSH_CHILD_LISTEN", port.to_string())
        .env("FAKE_SSH_SLEEP_MS", "600000");
    let child::Spawned { mut child, killer } = child::spawn_tree(cmd).unwrap();
    wait_until(
        "the grandchild to hold the port",
        Duration::from_secs(10),
        || !port_is_free(port),
    );
    drop(killer);
    child.wait().unwrap();
    wait_until("the grandchild to exit", KILL_GRACE * 5, || {
        port_is_free(port)
    });
}
