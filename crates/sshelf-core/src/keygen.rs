//! Key generation (SPEC.md §11), in-process with RustCrypto `ssh-key`:
//! standard OpenSSH private key files, encrypted with bcrypt-pbkdf and
//! AES-256-CTR when a passphrase is given. The passphrase never appears in
//! process arguments; it and the key material are wiped from memory after
//! use. Existing files are never overwritten, and the private key is only
//! readable by its owner (mode 600, or an owner-only ACL on Windows) before
//! anything is written into it.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use ssh_key::private::{EcdsaKeypair, Ed25519Keypair, KeypairData, RsaKeypair};
use ssh_key::rand_core::{OsRng, RngCore};
use ssh_key::{Cipher, EcdsaCurve, HashAlg, Kdf, LineEnding, PrivateKey};
use zeroize::Zeroizing;

/// `ssh-keygen`'s default bcrypt-pbkdf rounds.
pub const DEFAULT_KDF_ROUNDS: u32 = 16;
/// More rounds make every unlock slower; this keeps a typo from taking hours.
pub const MAX_KDF_ROUNDS: u32 = 1000;
const SALT_LEN: usize = 16;

/// A passphrase: wiped on drop, never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_empty() {
            "Secret(empty)"
        } else {
            "Secret(***)"
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyAlgorithm {
    #[default]
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa3072,
    Rsa4096,
}

impl KeyAlgorithm {
    pub const ALL: [KeyAlgorithm; 6] = [
        KeyAlgorithm::Ed25519,
        KeyAlgorithm::EcdsaP256,
        KeyAlgorithm::EcdsaP384,
        KeyAlgorithm::EcdsaP521,
        KeyAlgorithm::Rsa3072,
        KeyAlgorithm::Rsa4096,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KeyAlgorithm::Ed25519 => "Ed25519",
            KeyAlgorithm::EcdsaP256 => "ECDSA P-256",
            KeyAlgorithm::EcdsaP384 => "ECDSA P-384",
            KeyAlgorithm::EcdsaP521 => "ECDSA P-521",
            KeyAlgorithm::Rsa3072 => "RSA 3072",
            KeyAlgorithm::Rsa4096 => "RSA 4096",
        }
    }

    /// `id_ed25519`, `id_ecdsa`, `id_rsa`: the start of a default file name.
    pub fn file_stem(self) -> &'static str {
        match self {
            KeyAlgorithm::Ed25519 => "id_ed25519",
            KeyAlgorithm::EcdsaP256 | KeyAlgorithm::EcdsaP384 | KeyAlgorithm::EcdsaP521 => {
                "id_ecdsa"
            }
            KeyAlgorithm::Rsa3072 | KeyAlgorithm::Rsa4096 => "id_rsa",
        }
    }

    fn generate(self) -> Result<KeypairData, String> {
        let mut rng = OsRng;
        let ecdsa = |curve| {
            EcdsaKeypair::random(&mut OsRng, curve)
                .map(KeypairData::from)
                .map_err(|e| e.to_string())
        };
        let rsa = |bits| {
            RsaKeypair::random(&mut OsRng, bits)
                .map(KeypairData::from)
                .map_err(|e| e.to_string())
        };
        match self {
            KeyAlgorithm::Ed25519 => Ok(Ed25519Keypair::random(&mut rng).into()),
            KeyAlgorithm::EcdsaP256 => ecdsa(EcdsaCurve::NistP256),
            KeyAlgorithm::EcdsaP384 => ecdsa(EcdsaCurve::NistP384),
            KeyAlgorithm::EcdsaP521 => ecdsa(EcdsaCurve::NistP521),
            KeyAlgorithm::Rsa3072 => rsa(3072),
            KeyAlgorithm::Rsa4096 => rsa(4096),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRequest {
    pub algorithm: KeyAlgorithm,
    /// The private key file; the public key goes next to it as `.pub`.
    pub path: PathBuf,
    pub comment: String,
    /// Empty: the key is stored unencrypted.
    pub passphrase: Secret,
    pub kdf_rounds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedKey {
    pub path: PathBuf,
    pub public_path: PathBuf,
    /// `ssh-ed25519 AAAA… comment`
    pub public_line: String,
    /// `SHA256:…`
    pub fingerprint: String,
    pub encrypted: bool,
}

/// `<path>.pub`
pub fn public_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

/// A comment ends up inside single quotes in the install command
/// (SPEC.md §11), so quotes and line breaks are refused.
pub fn check_comment(comment: &str) -> Result<(), String> {
    if comment.contains(['\'', '"', '\n', '\r', '\\']) {
        Err("the comment cannot contain quotes, backslashes or line breaks".into())
    } else {
        Ok(())
    }
}

/// Why `path` cannot receive a new key: it or its `.pub` exists.
pub fn check_target(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("no file name".into());
    }
    for file in [path.to_path_buf(), public_path(path)] {
        if fs::symlink_metadata(&file).is_ok() {
            return Err(format!("{} already exists", file.display()));
        }
    }
    Ok(())
}

/// The remote command that adds `public_line` to `~/.ssh/authorized_keys`
/// once (SPEC.md §11). The key travels inside it in single quotes, so no
/// pipe is needed (terminals are launched without a shell); a key line never
/// contains quotes (base64 plus a checked comment). Assumes a POSIX shell on
/// the server; has no `;` (Windows Terminal splits its command line there).
/// It waits for Enter so the result stays readable.
pub fn install_script(public_line: &str) -> Result<String, String> {
    let key = public_line.trim();
    if key.is_empty() || key.contains(['\'', '"', '\\', '\n', '\r', ';']) {
        return Err("the public key line has characters that cannot be sent safely".into());
    }
    Ok(format!(
        "umask 077 && mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && \
         (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys) && \
         echo 'sshelf: the key is installed. Press Enter to close.' && read -r _"
    ))
}

/// `ssh` arguments for installing `public_line` on `alias`: `-t`, so the
/// password prompt and the final `read` have a terminal.
pub fn install_args(alias: &str, public_line: &str) -> Result<Vec<String>, String> {
    Ok(vec![
        "-t".into(),
        "--".into(),
        alias.to_string(),
        install_script(public_line)?,
    ])
}

/// Generates the key and writes both files. Nothing is left behind on
/// failure.
pub fn generate(request: &KeyRequest) -> Result<GeneratedKey, String> {
    check_comment(&request.comment)?;
    if !(1..=MAX_KDF_ROUNDS).contains(&request.kdf_rounds) {
        return Err(format!("KDF rounds must be 1 to {MAX_KDF_ROUNDS}"));
    }
    check_target(&request.path)?;

    let key = PrivateKey::new(request.algorithm.generate()?, request.comment.clone())
        .map_err(|e| e.to_string())?;
    let encrypted = !request.passphrase.is_empty();
    let stored = if encrypted {
        let mut salt = vec![0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let kdf = Kdf::Bcrypt {
            salt,
            rounds: request.kdf_rounds,
        };
        key.encrypt_with(
            Cipher::Aes256Ctr,
            kdf,
            OsRng.next_u32(),
            request.passphrase.expose(),
        )
        .map_err(|e| format!("encrypting the key: {e}"))?
    } else {
        key.clone()
    };
    let private = stored
        .to_openssh(LineEnding::LF)
        .map_err(|e| e.to_string())?;
    let public_line = key.public_key().to_openssh().map_err(|e| e.to_string())?;
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();

    if let Some(dir) = request.path.parent()
        && !dir.as_os_str().is_empty()
        && !dir.exists()
    {
        create_private_dir(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let public_path = public_path(&request.path);
    write_new(&request.path, private.as_bytes(), true)
        .map_err(|e| format!("writing {}: {e}", request.path.display()))?;
    if let Err(e) = write_new(&public_path, format!("{public_line}\n").as_bytes(), false) {
        let _ = fs::remove_file(&request.path);
        return Err(format!("writing {}: {e}", public_path.display()));
    }
    Ok(GeneratedKey {
        path: request.path.clone(),
        public_path,
        public_line,
        fingerprint,
        encrypted,
    })
}

/// Creates `path` (refusing an existing one), owner-only first when
/// `private`, then writes `bytes`. A failed write removes the file again.
fn write_new(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    let mut file = create_new(path, private)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    written
}

#[cfg(unix)]
fn create_new(path: &Path, private: bool) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if private { 0o600 } else { 0o644 })
        .open(path)
}

#[cfg(windows)]
fn create_new(path: &Path, private: bool) -> std::io::Result<File> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if private && let Err(e) = crate::acl::restrict_to_current_user(path) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(file)
}

/// `~/.ssh` when it does not exist yet: 700 on Unix (ssh insists).
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(dir: &Path, algorithm: KeyAlgorithm, passphrase: &str) -> KeyRequest {
        KeyRequest {
            algorithm,
            path: dir.join("id_test"),
            comment: "web (sshelf)".into(),
            passphrase: Secret::new(passphrase.into()),
            kdf_rounds: 4,
        }
    }

    #[test]
    fn secrets_never_print() {
        assert_eq!(
            format!("{:?}", Secret::new("hunter2".into())),
            "Secret(***)"
        );
        assert_eq!(format!("{:?}", Secret::default()), "Secret(empty)");
        let request = request(Path::new("x"), KeyAlgorithm::Ed25519, "hunter2");
        assert!(!format!("{request:?}").contains("hunter2"));
    }

    #[test]
    fn every_algorithm_generates_a_readable_key() {
        // RSA 4096 takes the same path as 3072, just many seconds longer.
        for algorithm in KeyAlgorithm::ALL
            .into_iter()
            .filter(|a| *a != KeyAlgorithm::Rsa4096)
        {
            let dir = tempfile::tempdir().unwrap();
            let key = generate(&request(dir.path(), algorithm, "")).unwrap();
            assert!(!key.encrypted);
            assert!(key.fingerprint.starts_with("SHA256:"), "{algorithm:?}");
            assert!(key.public_line.ends_with(" web (sshelf)"), "{algorithm:?}");
            let read = PrivateKey::read_openssh_file(&key.path).unwrap();
            assert_eq!(
                read.public_key().to_openssh().unwrap(),
                key.public_line,
                "{algorithm:?}"
            );
            let public = fs::read_to_string(&key.public_path).unwrap();
            assert_eq!(public, format!("{}\n", key.public_line));
        }
    }

    #[test]
    fn a_passphrase_encrypts_with_the_chosen_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate(&request(dir.path(), KeyAlgorithm::Ed25519, "correct horse")).unwrap();
        assert!(key.encrypted);
        let read = PrivateKey::read_openssh_file(&key.path).unwrap();
        assert!(read.is_encrypted());
        assert!(matches!(read.kdf(), Kdf::Bcrypt { rounds: 4, .. }));
        assert!(read.decrypt("wrong").is_err());
        let open = read.decrypt("correct horse").unwrap();
        assert_eq!(open.public_key().to_openssh().unwrap(), key.public_line);
    }

    #[test]
    fn existing_files_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let req = request(dir.path(), KeyAlgorithm::Ed25519, "");
        fs::write(public_path(&req.path), "keep me").unwrap();
        assert!(generate(&req).unwrap_err().contains("already exists"));
        assert!(!req.path.exists(), "no private key left behind");
        assert_eq!(
            fs::read_to_string(public_path(&req.path)).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn bad_comments_and_rounds_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for comment in ["it's", "a\"b", "two\nlines", r"back\slash"] {
            let req = KeyRequest {
                comment: comment.into(),
                ..request(dir.path(), KeyAlgorithm::Ed25519, "")
            };
            assert!(generate(&req).is_err(), "{comment:?}");
        }
        for rounds in [0, MAX_KDF_ROUNDS + 1] {
            let req = KeyRequest {
                kdf_rounds: rounds,
                ..request(dir.path(), KeyAlgorithm::Ed25519, "x")
            };
            assert!(generate(&req).is_err(), "{rounds}");
        }
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn the_private_key_is_owner_only_and_a_missing_dir_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let req = KeyRequest {
            path: dir.path().join(".ssh").join("id_new"),
            ..request(dir.path(), KeyAlgorithm::Ed25519, "")
        };
        let key = generate(&req).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&key.path), 0o600);
            assert_eq!(mode(&dir.path().join(".ssh")), 0o700);
        }
        #[cfg(windows)]
        {
            assert!(crate::acl::is_owner_only(&key.path).unwrap());
            assert!(!crate::acl::is_owner_only(&key.public_path).unwrap());
        }
    }

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/867Gn6gWVC9L/2TosddbUBIfUwN1FA755XCQA20kG web (sshelf)";

    #[test]
    fn the_install_command_carries_the_key_safely() {
        let args = install_args("web", KEY).unwrap();
        assert_eq!(&args[..3], ["-t", "--", "web"]);
        let script = &args[3];
        assert!(script.contains(&format!("grep -qxF '{KEY}'")), "{script}");
        assert!(!script.contains(';'), "Windows Terminal splits on ;");
        for bad in ["a'b", "a;b", "a\nb", ""] {
            assert!(install_script(bad).is_err(), "{bad:?}");
        }
    }

    /// The script itself, run by the local POSIX shell on a temp home: adds
    /// the key once, with private permissions, and keeps existing lines.
    #[cfg(unix)]
    #[test]
    fn the_install_script_adds_the_key_once() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".ssh")).unwrap();
        let file = home.path().join(".ssh").join("authorized_keys");
        fs::write(&file, "ssh-rsa AAAA old\n").unwrap();
        let script = install_script(KEY).unwrap();
        for _ in 0..2 {
            Command::new("sh")
                .args(["-c", &script])
                .env("HOME", home.path())
                .stdin(Stdio::null())
                .status()
                .unwrap();
        }
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            format!("ssh-rsa AAAA old\n{KEY}\n")
        );
        let fresh = tempfile::tempdir().unwrap();
        Command::new("sh")
            .args(["-c", &script])
            .env("HOME", fresh.path())
            .stdin(Stdio::null())
            .status()
            .unwrap();
        let created = fresh.path().join(".ssh").join("authorized_keys");
        assert_eq!(fs::read_to_string(&created).unwrap(), format!("{KEY}\n"));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&created), 0o600);
        assert_eq!(mode(&fresh.path().join(".ssh")), 0o700);
    }
}
