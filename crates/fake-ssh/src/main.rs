//! The test binary of [`fake_ssh`]: what the tests spawn instead of `ssh`.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    // A child copy started by fake-ssh itself carries the embedded flag.
    if args.first().map(String::as_str) == Some(fake_ssh::EMBEDDED_FLAG) {
        args.remove(0);
    }
    fake_ssh::run(args)
}
