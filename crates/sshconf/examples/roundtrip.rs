//! Read-only round-trip check of a real ssh config (SPEC.md M1 "Done when").
//!
//! Usage: `cargo run -p sshconf --example roundtrip -- [--list] [CONFIG]`
//!
//! Loads CONFIG (default `~/.ssh/config`) with every `Include`d file,
//! re-serializes each file in memory and compares it with the bytes on disk.
//! Never writes anything. `--list` also prints hosts and patterns.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use sshconf::{ConfigSet, Eol, HostList, PatternKind, ResolveOptions};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let list = args.iter().any(|a| a == "--list");
    let Some(home) = std::env::home_dir() else {
        eprintln!("error: cannot determine the home directory");
        return ExitCode::from(2);
    };
    let opts = ResolveOptions::new(home);
    let main = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| opts.ssh_dir.join("config"));

    let set = match ConfigSet::load(&main, &opts) {
        Ok(set) => set,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let mut mismatches = 0;
    for file in &set.files {
        let written = file.to_bytes();
        let status = match fs::read(&file.path) {
            Ok(bytes) if bytes == written => "ok".to_string(),
            Ok(bytes) => {
                mismatches += 1;
                format!("MISMATCH at byte {}", first_difference(&bytes, &written))
            }
            Err(e) => {
                mismatches += 1;
                format!("unreadable: {e}")
            }
        };
        let crlf = file.lines.iter().filter(|l| l.eol == Eol::CrLf).count();
        println!(
            "{status:>4}  {}  ({} lines, {crlf} CRLF{})",
            file.path.display(),
            file.lines.len(),
            if file.bom { ", BOM" } else { "" }
        );
    }

    let hosts = HostList::from_set(&set);
    let multi_block = hosts.hosts.iter().filter(|h| !h.also_in.is_empty()).count();
    println!(
        "\n{} hosts ({multi_block} configured in more than one block), {} patterns",
        hosts.hosts.len(),
        hosts.patterns.len()
    );
    if list {
        for h in &hosts.hosts {
            let conditional = if h.conditional { " [conditional]" } else { "" };
            println!(
                "  host     {}{conditional}  ({})",
                h.aliases.join(" "),
                set.location(h.at)
            );
            for &at in &h.also_in {
                println!("             also configured at {}", set.location(at));
            }
        }
        for p in &hosts.patterns {
            let text = match &p.kind {
                PatternKind::Host(patterns) => {
                    let words: Vec<String> = patterns
                        .iter()
                        .map(|p| format!("{}{}", if p.negated { "!" } else { "" }, p.pattern))
                        .collect();
                    format!("Host {}", words.join(" "))
                }
                PatternKind::Match(args) => format!("Match {}", args.join(" ")),
            };
            println!("  pattern  {text}  ({})", set.location(p.at));
        }
    }

    for d in &set.diagnostics {
        println!("{:?}: {}: {}", d.severity, set.location(d.at), d.message);
    }

    if mismatches == 0 {
        println!("\nround-trip: all {} files byte-identical", set.files.len());
        ExitCode::SUCCESS
    } else {
        println!("\nround-trip: {mismatches} file(s) differ");
        ExitCode::FAILURE
    }
}

fn first_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()))
}
