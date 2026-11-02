//! Host key plumbing (SPEC.md §9): the real `ssh-keygen` on temp
//! `known_hosts` files (it never touches the network for `-F`/`-lf`/`-R`),
//! and key fetching against `fake-ssh`.

use std::fs;
use std::path::PathBuf;

use sshelf_core::hostkeys::{self, KeyTarget, Keygen};
use sshelf_core::ssh::SshTools;

/// Public parts of two throwaway keys (fake-ssh serves `NEW`).
const OLD: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID/hisleXY3lBCSP34IG2qVLK1Fupesz68s+RrGuiBgg";
const NEW: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/867Gn6gWVC9L/2TosddbUBIfUwN1FA755XCQA20kG";

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"))
}

/// The system `ssh-keygen` (built-in OpenSSH on Windows, `PATH` elsewhere).
fn keygen() -> Keygen {
    let tools = SshTools::locate(None).expect("the system ssh is installed");
    Keygen {
        path: tools.companion("ssh-keygen"),
    }
}

#[test]
fn keygen_finds_fingerprints_and_removes_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("known_hosts");
    fs::write(
        &file,
        format!("web.example.com {OLD}\n[odd.example.com]:2222 {NEW}\n"),
    )
    .unwrap();
    let keygen = keygen();

    assert_eq!(
        keygen.find("web.example.com", &file).unwrap(),
        [format!("web.example.com {OLD}")]
    );
    assert_eq!(
        keygen.find("[odd.example.com]:2222", &file).unwrap().len(),
        1
    );
    assert!(keygen.find("nope.example.com", &file).unwrap().is_empty());
    assert!(
        keygen
            .find("web", &tmp.path().join("missing"))
            .unwrap()
            .is_empty()
    );

    let old = keygen.fingerprints(&[format!("x {OLD}")]).unwrap();
    let new = keygen.fingerprints(&[format!("x {NEW}")]).unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].kind, "ED25519");
    assert!(old[0].fingerprint.starts_with("SHA256:"));
    assert_ne!(old[0].fingerprint, new[0].fingerprint);

    keygen.remove("web.example.com", &file).unwrap();
    assert!(keygen.find("web.example.com", &file).unwrap().is_empty());
    assert_eq!(
        keygen.find("[odd.example.com]:2222", &file).unwrap().len(),
        1
    );
    assert!(tmp.path().join("known_hosts.old").is_file());
}

#[test]
fn target_comes_from_ssh_g_and_finds_keys_in_any_file() {
    let home = tempfile::tempdir().unwrap();
    let tools = SshTools::locate(Some(&fake_ssh())).unwrap();
    let resolved = tools.resolve("newkey-via-x").unwrap();
    let target = KeyTarget::from_resolved("newkey-via-x", &resolved, home.path());
    assert_eq!(target.lookup, "newkey-via-x.example.com");
    assert_eq!(target.jumps, ["newkey-jump"]);
    assert_eq!(
        target.user_files[0],
        home.path().join(".ssh").join("known_hosts")
    );

    let keygen = keygen();
    assert!(keygen.known(&target).unwrap().is_empty());
    fs::create_dir_all(home.path().join(".ssh")).unwrap();
    fs::write(
        home.path().join(".ssh").join("global_known_hosts"),
        format!("newkey-via-x.example.com {OLD}\n"),
    )
    .unwrap();
    let known = keygen.known(&target).unwrap();
    assert_eq!(known.len(), 1);
    assert!(known[0].0.ends_with("global_known_hosts"));
}

#[test]
fn fetch_returns_the_server_key_even_when_login_fails() {
    let lines = hostkeys::fetch(&fake_ssh(), "newkey-a", None).unwrap();
    assert_eq!(lines, [format!("newkey-a.example.com {NEW}")]);

    let lines = hostkeys::fetch(&fake_ssh(), "admin@newkey-b:2200", Some("ssh-ed25519")).unwrap();
    assert_eq!(lines, [format!("[newkey-b.example.com]:2200 {NEW}")]);
}

#[test]
fn fetch_reports_why_no_key_arrived() {
    let err = hostkeys::fetch(&fake_ssh(), "unreachable-x", None).unwrap_err();
    assert!(err.contains("timed out"), "{err}");
}
