//! Scriptable stand-in for the system `ssh` binary.
//!
//! Supervisor tests (milestone M3) spawn this instead of real `ssh` and steer
//! its behavior through arguments/env: sleep forever, exit with a code after a
//! delay, print given stderr, simulate host key failures. See SPEC.md §7.
//!
//! M0 stub: accepts any arguments and exits successfully.

fn main() {}
