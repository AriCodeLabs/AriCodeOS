// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
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
}
