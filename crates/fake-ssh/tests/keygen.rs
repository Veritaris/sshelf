//! Generated keys against the real `ssh-keygen` (SPEC.md §11): it must read
//! each private key back (`-y` prints its public key), which also proves
//! OpenSSH accepts the file's permissions — it refuses keys others can read,
//! on Windows too. Nothing touches the network.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sshelf_core::core::{Command, Core, Event};
use sshelf_core::keygen::{self, KeyAlgorithm, KeyRequest, Secret};
use sshelf_core::paths::Paths;
use sshelf_core::process;
use sshelf_core::ssh::SshTools;

fn ssh_keygen() -> std::path::PathBuf {
    SshTools::locate(None)
        .expect("the system ssh is installed")
        .companion("ssh-keygen")
}

/// `ssh-keygen -y -P <passphrase> -f <key>`: the public key OpenSSH derives.
fn derived_public(key: &Path, passphrase: &str) -> Result<String, String> {
    let mut cmd = process::command(&ssh_keygen());
    cmd.args(["-y", "-P", passphrase, "-f"]).arg(key);
    let out = process::run(cmd, std::time::Duration::from_secs(30)).map_err(|e| e.to_string())?;
    if out.code == Some(0) {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(out.stderr)
    }
}

/// `type base64`, without the comment.
fn key_part(line: &str) -> String {
    line.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn ssh_keygen_reads_every_generated_key() {
    for algorithm in [
        KeyAlgorithm::Ed25519,
        KeyAlgorithm::EcdsaP256,
        KeyAlgorithm::EcdsaP521,
        KeyAlgorithm::Rsa3072,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let key = keygen::generate(&KeyRequest {
            algorithm,
            path: dir.path().join("id_test"),
            comment: "test (sshelf)".into(),
            passphrase: Secret::default(),
            kdf_rounds: keygen::DEFAULT_KDF_ROUNDS,
        })
        .unwrap();
        let derived =
            derived_public(&key.path, "").unwrap_or_else(|e| panic!("{algorithm:?}: {e}"));
        assert_eq!(
            key_part(&derived),
            key_part(&key.public_line),
            "{algorithm:?}"
        );
    }
}

#[test]
fn the_core_generates_keys_in_the_background() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    let path = paths.ssh_dir.join("id_ed25519_web");
    core.send(Command::GenerateKey {
        id: 7,
        request: KeyRequest {
            algorithm: KeyAlgorithm::Ed25519,
            path: path.clone(),
            comment: "web (sshelf)".into(),
            passphrase: Secret::default(),
            kdf_rounds: keygen::DEFAULT_KDF_ROUNDS,
        },
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let result = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left).expect("a generated key") {
            Event::KeyGenerated { id: 7, result } => break result,
            _ => continue,
        }
    };
    let key = result.unwrap();
    assert_eq!(key.path, path);
    assert!(key.public_path.is_file());
    drop(core);
}

#[test]
fn ssh_keygen_opens_an_encrypted_key_only_with_its_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen::generate(&KeyRequest {
        algorithm: KeyAlgorithm::Ed25519,
        path: dir.path().join("id_locked"),
        comment: "locked".into(),
        passphrase: Secret::new("correct horse battery".into()),
        kdf_rounds: 8,
    })
    .unwrap();
    assert!(derived_public(&key.path, "wrong").is_err());
    let derived = derived_public(&key.path, "correct horse battery").unwrap();
    assert_eq!(key_part(&derived), key_part(&key.public_line));
}
