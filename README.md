# sshelf

A cross-platform desktop app (Windows, macOS, Linux) that puts a visual layer over your real `~/.ssh/config`: browse hosts, see their effective configuration, edit them safely, launch SSH sessions, and manage port-forwarding tunnels and groups of tunnels.

Your ssh config stays the single source of truth. sshelf shells out to the system `ssh`, never reads private keys and never stores passwords.

Status: early development (milestone M0, scaffold). See [SPEC.md](SPEC.md) for scope, design and milestones.

## Build and run

Requires stable Rust (edition 2024).

```
cargo run -p sshelf
cargo test --workspace
```

On Linux, install the windowing dev packages first (Debian/Ubuntu):

```
sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev
```

## Stack

Rust, egui/eframe 0.37. Conventions mirror the owner's mp32ogg and atlasmerge projects.

## License

MIT, see [LICENSE](LICENSE).
