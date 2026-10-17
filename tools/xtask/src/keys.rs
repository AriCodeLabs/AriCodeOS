// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Key management (Phase 31): generate Ed25519 key pairs, sign and verify
//! files. The private key (32-byte seed, hex) never leaves the host; the
//! system only carries public keys.

use crate::Result;
use aricode_crypto::{ed25519, hex, unhex};
use std::fs;
use std::path::Path;

pub const SEED_FILE: &str = "ed25519.seed";
pub const PUB_FILE: &str = "ed25519.pub";
/// Development key pair checked into the repo: for tests and QEMU only.
pub const DEV_KEY_DIR: &str = "keys/dev";

/// Environment variable through which a build embeds a non-development
/// public key (`aricode_crypto::trust`).
pub const SIGNING_PUB_ENV: &str = "ARICODE_SIGNING_PUB";

/// The key pair selected with `--signing-key DIR` for this run, with its
/// public key verified against the seed at selection time and kept here:
/// it is never read again, so a later read error can never fall back to the
/// development key (re-audit, R1). Unset means the development pair.
struct Selected {
    dir: std::path::PathBuf,
    public: [u8; 32],
    public_hex: String,
}

static SELECTED: std::sync::OnceLock<Selected> = std::sync::OnceLock::new();

/// `--signing-key DIR` (ADR 0029, "The signing key"): images, packages and
/// indexes are signed with `DIR/ed25519.seed` and the build embeds
/// `DIR/ed25519.pub`. The pair must match, and a production seed must live
/// outside the repository. Without it, a loud warning: the development
/// seed is public.
pub fn select_signing_key(dir: Option<&Path>) -> Result {
    let root = crate::root();
    let dev = root.join(DEV_KEY_DIR);
    let Some(dir) = dir else {
        eprintln!(
            "[xtask] WARNING: signing with the DEVELOPMENT key {DEV_KEY_DIR}, whose seed is public: \
             anyone on the network can sign images and packages that this build accepts. \
             Use --signing-key DIR (cargo xtask keygen DIR, kept outside the repository) \
             before upgrading machines over an untrusted network."
        );
        return Ok(());
    };
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("--signing-key {}: {e}", dir.display()))?;
    if dir == dev.canonicalize().unwrap_or(dev) {
        return select_signing_key(None);
    }
    if dir.starts_with(&root) {
        return Err(format!(
            "--signing-key {}: a signing seed must be kept outside the repository",
            dir.display()
        ));
    }
    let seed: [u8; 32] = read_hex(&dir.join(SEED_FILE))?;
    let public: [u8; 32] = read_hex(&dir.join(PUB_FILE))?;
    if ed25519::public_key(&seed) != public {
        return Err(format!(
            "--signing-key {}: {SEED_FILE} and {PUB_FILE} are not a pair",
            dir.display()
        ));
    }
    let mut buf = [0u8; 64];
    let public_hex = hex(&public, &mut buf).to_string();
    println!(
        "[xtask] signing key {} (public {public_hex})",
        dir.display()
    );
    SELECTED
        .set(Selected {
            dir,
            public,
            public_hex,
        })
        .map_err(|_| String::from("--signing-key given twice"))
}

/// Seed of the selected signing key (the development one by default).
pub fn signing_seed() -> Result<[u8; 32]> {
    let dir = SELECTED
        .get()
        .map(|s| s.dir.clone())
        .unwrap_or_else(|| crate::root().join(DEV_KEY_DIR));
    read_hex(&dir.join(SEED_FILE))
}

/// Public key (hex) a build must embed instead of the development one, as
/// verified at selection; `None` only when no key was selected.
pub fn signing_public_hex() -> Option<&'static str> {
    SELECTED.get().map(|s| s.public_hex.as_str())
}

/// The public key the build must embed and every signature must verify
/// with: the selected one, or the development one (whose file must read).
pub fn expected_public_key() -> Result<[u8; 32]> {
    match SELECTED.get() {
        Some(s) => Ok(s.public),
        None => read_hex(&crate::root().join(DEV_KEY_DIR).join(PUB_FILE)),
    }
}

/// Post-build check (re-audit, R1): `binary` must carry the trust anchor
/// (`aricode_crypto::trust`) with the expected public key, so a build that
/// embedded another key fails here instead of on the machines.
pub fn check_embedded_key(binary: &Path) -> Result {
    let data = fs::read(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let want = expected_public_key()?;
    let tag = aricode_crypto::trust::ANCHOR_TAG;
    let found: Vec<&[u8]> = data
        .windows(tag.len() + 32)
        .filter(|w| w[..tag.len()] == tag)
        .map(|w| &w[tag.len()..])
        .collect();
    match found.as_slice() {
        [key] if *key == want => Ok(()),
        [] => Err(format!("{}: no trust anchor found", binary.display())),
        _ => Err(format!(
            "{}: embeds a different public key than the one selected for signing",
            binary.display()
        )),
    }
}

fn os_random(out: &mut [u8]) -> Result {
    // `/dev/urandom` is endless: read exactly what is needed, never `fs::read`.
    use std::io::Read;
    let mut f = fs::File::open("/dev/urandom").map_err(|e| format!("/dev/urandom: {e}"))?;
    f.read_exact(out)
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(())
}

pub fn read_hex<const N: usize>(path: &Path) -> Result<[u8; N]> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = [0u8; N];
    unhex(text.trim(), &mut out)
        .ok_or_else(|| format!("{}: expected {} bytes of hex", path.display(), N))?;
    Ok(out)
}

fn hex_line(bytes: &[u8]) -> String {
    let mut buf = vec![0u8; bytes.len() * 2];
    format!("{}\n", hex(bytes, &mut buf))
}

fn write_hex(path: &Path, bytes: &[u8]) -> Result {
    fs::write(path, hex_line(bytes)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes a secret: exclusive creation with mode 0600 from the very first
/// instant (without relying on the umask or a later chmod).
fn write_secret(path: &Path, bytes: &[u8]) -> Result {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(hex_line(bytes).as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `xtask keygen DIR`: creates `DIR/ed25519.seed` and `DIR/ed25519.pub`.
pub fn keygen(dir: &Path) -> Result {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let seed_path = dir.join(SEED_FILE);
    if seed_path.exists() {
        return Err(format!(
            "{} already exists; keys are never overwritten",
            seed_path.display()
        ));
    }
    let mut seed = [0u8; ed25519::SEED_LEN];
    os_random(&mut seed)?;
    let pk = ed25519::public_key(&seed);
    write_secret(&seed_path, &seed)?;
    write_hex(&dir.join(PUB_FILE), &pk)?;
    let mut buf = [0u8; 64];
    println!("[xtask] public key {}", hex(&pk, &mut buf));
    Ok(())
}

/// `xtask sign SEED FILE`: writes `FILE.sig` (hex).
pub fn sign(seed_path: &Path, file: &Path) -> Result {
    let seed: [u8; 32] = read_hex(seed_path)?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sig = ed25519::sign(&seed, &data);
    let out = sig_path(file);
    write_hex(&out, &sig)?;
    println!("[xtask] signature at {}", out.display());
    Ok(())
}

/// `xtask verify PUB FILE [SIG]`.
pub fn verify(pub_path: &Path, file: &Path, sig: Option<&Path>) -> Result {
    let pk: [u8; 32] = read_hex(pub_path)?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sig_file = sig.map(Path::to_path_buf).unwrap_or_else(|| sig_path(file));
    let signature: [u8; 64] = read_hex(&sig_file)?;
    if ed25519::verify(&pk, &data, &signature) {
        println!("[xtask] valid signature: {}", file.display());
        Ok(())
    } else {
        Err(format!("INVALID signature: {}", file.display()))
    }
}

pub fn sig_path(file: &Path) -> std::path::PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".sig");
    file.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keygen_sign_verify_round_trip() {
        let dir = std::env::temp_dir().join(format!("aricode-keys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        keygen(&dir).unwrap();
        assert!(keygen(&dir).is_err(), "does not overwrite");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(SEED_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the seed must not be readable by others: {mode:o}"
            );
        }
        let file = dir.join("message.txt");
        fs::write(&file, b"test manifest").unwrap();
        sign(&dir.join(SEED_FILE), &file).unwrap();
        verify(&dir.join(PUB_FILE), &file, None).unwrap();
        fs::write(&file, b"tampered manifest").unwrap();
        assert!(verify(&dir.join(PUB_FILE), &file, None).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signing_key_must_be_a_pair_outside_the_repository() {
        // Audit 1 (production key): a seed inside the repository, or a seed
        // and public key that do not match, are refused before anything
        // is signed.
        let err = select_signing_key(Some(&crate::root().join("keys"))).unwrap_err();
        assert!(err.contains("outside the repository"), "{err}");
        let tmp = crate::test_util::TempDir::new("signing-pair");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        keygen(&a).unwrap();
        keygen(&b).unwrap();
        fs::copy(b.join(PUB_FILE), a.join(PUB_FILE)).unwrap();
        let err = select_signing_key(Some(&a)).unwrap_err();
        assert!(err.contains("not a pair"), "{err}");
        assert!(signing_public_hex().is_none(), "nothing was selected");
    }

    #[test]
    fn embedded_key_check_finds_the_anchor() {
        let tmp = crate::test_util::TempDir::new("anchor");
        let dev = expected_public_key().unwrap();
        let mut bin = b"junk".to_vec();
        bin.extend_from_slice(&aricode_crypto::trust::ANCHOR_TAG);
        bin.extend_from_slice(&dev);
        bin.extend_from_slice(b"more");
        let good = tmp.path().join("good");
        fs::write(&good, &bin).unwrap();
        check_embedded_key(&good).unwrap();
        let n = bin.len();
        bin[n - 5] ^= 1; // last key byte
        let bad = tmp.path().join("bad");
        fs::write(&bad, &bin).unwrap();
        assert!(check_embedded_key(&bad)
            .unwrap_err()
            .contains("different public key"));
        let none = tmp.path().join("none");
        fs::write(&none, b"no anchor here").unwrap();
        assert!(check_embedded_key(&none)
            .unwrap_err()
            .contains("no trust anchor"));
    }
}
