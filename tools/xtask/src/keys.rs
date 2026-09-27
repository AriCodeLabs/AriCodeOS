//! Gestión de claves (Fase 31): generar pares Ed25519, firmar y verificar
//! ficheros. La clave privada (semilla de 32 bytes, hex) nunca sale del
//! host; el sistema solo lleva claves públicas.

use crate::Result;
use aricode_crypto::{ed25519, hex, unhex};
use std::fs;
use std::path::Path;

pub const SEED_FILE: &str = "ed25519.seed";
pub const PUB_FILE: &str = "ed25519.pub";
/// Par de desarrollo versionado en el repo: solo para tests y QEMU.
pub const DEV_KEY_DIR: &str = "keys/dev";

fn os_random(out: &mut [u8]) -> Result {
    // `/dev/urandom` es infinito: leer justo lo necesario, nunca `fs::read`.
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
        .ok_or_else(|| format!("{}: hex de {} bytes esperado", path.display(), N))?;
    Ok(out)
}

fn write_hex(path: &Path, bytes: &[u8]) -> Result {
    let mut buf = vec![0u8; bytes.len() * 2];
    let text = format!("{}\n", hex(bytes, &mut buf));
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `xtask keygen DIR`: crea `DIR/ed25519.seed` y `DIR/ed25519.pub`.
pub fn keygen(dir: &Path) -> Result {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let seed_path = dir.join(SEED_FILE);
    if seed_path.exists() {
        return Err(format!(
            "{} ya existe; no se sobrescriben claves",
            seed_path.display()
        ));
    }
    let mut seed = [0u8; ed25519::SEED_LEN];
    os_random(&mut seed)?;
    let pk = ed25519::public_key(&seed);
    write_hex(&seed_path, &seed)?;
    write_hex(&dir.join(PUB_FILE), &pk)?;
    let mut buf = [0u8; 64];
    println!("[xtask] clave publica {}", hex(&pk, &mut buf));
    Ok(())
}

/// `xtask sign SEED FICHERO`: escribe `FICHERO.sig` (hex).
pub fn sign(seed_path: &Path, file: &Path) -> Result {
    let seed: [u8; 32] = read_hex(seed_path)?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sig = ed25519::sign(&seed, &data);
    let out = sig_path(file);
    write_hex(&out, &sig)?;
    println!("[xtask] firma en {}", out.display());
    Ok(())
}

/// `xtask verify PUB FICHERO [FIRMA]`.
pub fn verify(pub_path: &Path, file: &Path, sig: Option<&Path>) -> Result {
    let pk: [u8; 32] = read_hex(pub_path)?;
    let data = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sig_file = sig.map(Path::to_path_buf).unwrap_or_else(|| sig_path(file));
    let signature: [u8; 64] = read_hex(&sig_file)?;
    if ed25519::verify(&pk, &data, &signature) {
        println!("[xtask] firma valida: {}", file.display());
        Ok(())
    } else {
        Err(format!("firma NO valida: {}", file.display()))
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
        assert!(keygen(&dir).is_err(), "no sobrescribe");
        let file = dir.join("mensaje.txt");
        fs::write(&file, b"manifiesto de prueba").unwrap();
        sign(&dir.join(SEED_FILE), &file).unwrap();
        verify(&dir.join(PUB_FILE), &file, None).unwrap();
        fs::write(&file, b"manifiesto alterado").unwrap();
        assert!(verify(&dir.join(PUB_FILE), &file, None).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
