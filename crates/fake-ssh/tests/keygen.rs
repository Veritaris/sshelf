//! Generated keys against the real `ssh-keygen` (SPEC.md §11): it must read
//! each private key back (`-y` prints its public key), which also proves
//! OpenSSH accepts the file's permissions — it refuses keys others can read,
//! on Windows too. Nothing touches the network.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sshelf_core::core::{Command, Core, Event};
use sshelf_core::keygen::{self, KeyAlgorithm, KeyRequest, Secret};
use sshelf_core::paths::Paths;
use sshelf_core::process;
use sshelf_core::settings::Settings;
use sshelf_core::ssh::SshTools;
use sshelf_core::storage::Store;

fn ssh_keygen() -> std::path::PathBuf {
    SshTools::locate(None)
        .expect("the system ssh is installed")
        .companion("ssh-keygen")
        .program
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

/// Arguments fake-ssh recorded (one launch per line, `\x1f`-separated).
fn recorded(file: &Path) -> Vec<Vec<String>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && !text.is_empty()
        {
            return text
                .lines()
                .map(|l| l.split('\x1f').map(str::to_string).collect())
                .collect();
        }
        assert!(Instant::now() < deadline, "nothing was launched");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn installing_a_key_opens_the_terminal_with_ssh_and_the_script() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let record = tmp.path().join("launches.txt");
    let fake = PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"));
    // The "terminal" is fake-ssh recording what it was asked to run.
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(fake.clone()),
            terminal_command: format!(
                "\"{}\" --record \"{}\" {{ssh}} {{alias}}",
                fake.display(),
                record.display()
            ),
            ..Settings::default()
        })
        .unwrap();
    let (core, events) = Core::start(paths, Arc::new(|| {}));
    let line =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/867Gn6gWVC9L/2TosddbUBIfUwN1FA755XCQA20kG web";
    core.send(Command::InstallKey {
        alias: "web".into(),
        public_line: line.into(),
    });
    let launches = recorded(&record);
    let args = &launches[0];
    let at = args.iter().position(|a| a == "-t").expect("ssh -t");
    assert_eq!(args[at - 1], fake.display().to_string(), "{args:?}");
    assert_eq!(&args[at..at + 3], ["-t", "--", "web"]);
    assert!(args[at + 3].contains(&format!("grep -qxF '{line}'")));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Event::Notice(text) = events.recv_timeout(left).expect("a notice") {
            assert_eq!(text, "Opened a terminal to install the key on web");
            break;
        }
    }
    drop(core);
}

#[test]
fn security_keys_run_ssh_keygen_in_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&tmp.path().join("home"), tmp.path().join("app"));
    let record = tmp.path().join("launches.txt");
    let fake = PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"));
    Store::new(&paths.app_dir)
        .save(&Settings {
            ssh_path: Some(fake.clone()),
            terminal_command: format!(
                "\"{}\" --record \"{}\" {{ssh}} {{alias}}",
                fake.display(),
                record.display()
            ),
            ..Settings::default()
        })
        .unwrap();
    let (core, events) = Core::start(paths.clone(), Arc::new(|| {}));
    let path = paths.ssh_dir.join("id_ed25519_sk_web");
    core.send(Command::GenerateHardwareKey(KeyRequest {
        algorithm: KeyAlgorithm::Ed25519Sk,
        path: path.clone(),
        comment: "web (sshelf)".into(),
        passphrase: Secret::default(),
        kdf_rounds: keygen::DEFAULT_KDF_ROUNDS,
    }));
    let launches = recorded(&record);
    let args = &launches[0];
    let at = args.iter().position(|a| a == "-t").expect("ssh-keygen -t");
    assert!(args[at - 1].contains("ssh-keygen"), "{args:?}");
    assert_eq!(&args[at..at + 2], ["-t", "ed25519-sk"]);
    assert_eq!(args.last().unwrap(), &path.display().to_string());
    assert!(
        !args.iter().any(|a| a == "-N" || a == "-P"),
        "no secrets: {args:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Event::Notice(text) = events.recv_timeout(left).expect("a notice") {
            assert!(text.contains("touch your security key"), "{text}");
            break;
        }
    }
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
