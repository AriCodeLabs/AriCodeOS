//! Criptografía (Fase 31): las cuatro primitivas que necesita la cadena de
//! confianza, escritas desde cero y comprobadas con los vectores oficiales
//! (FIPS 180-4, RFC 8032, RFC 7539). Sin `alloc`.
//!
//! - [`sha256`] y [`sha512`]: hashes de imágenes y de mensajes.
//! - [`ed25519`]: firma de manifiestos (la clave privada solo vive en el host).
//! - [`chacha20`]: generador determinista para el RNG del kernel.
//!
//! Las operaciones de Ed25519 no son de tiempo constante: el sistema solo
//! verifica firmas con claves públicas; firmar es cosa del host (xtask).

#![no_std]

pub mod chacha20;
pub mod ed25519;
pub mod sha256;
pub mod sha512;

/// Comparación de igualdad sin cortocircuito (para etiquetas y firmas).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Escribe `bytes` en hexadecimal minúsculo en `out` (2 caracteres por byte).
pub fn hex<'a>(bytes: &[u8], out: &'a mut [u8]) -> &'a str {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let n = bytes.len().min(out.len() / 2);
    for (i, b) in bytes[..n].iter().enumerate() {
        out[2 * i] = DIGITS[(b >> 4) as usize];
        out[2 * i + 1] = DIGITS[(b & 15) as usize];
    }
    core::str::from_utf8(&out[..2 * n]).unwrap_or("")
}

/// Analiza hexadecimal (mayúsculas o minúsculas) en `out`; `None` si la
/// longitud no cuadra o hay caracteres inválidos.
pub fn unhex(text: &str, out: &mut [u8]) -> Option<()> {
    let t = text.as_bytes();
    if t.len() != out.len() * 2 {
        return None;
    }
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (t[2 * i] as char).to_digit(16)?;
        let lo = (t[2 * i + 1] as char).to_digit(16)?;
        *slot = (hi * 16 + lo) as u8;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip_and_ct_eq() {
        let mut out = [0u8; 8];
        assert_eq!(hex(&[0xde, 0xad, 0xbe, 0xef], &mut out), "deadbeef");
        let mut back = [0u8; 4];
        unhex("DEADbeef", &mut back).unwrap();
        assert_eq!(back, [0xde, 0xad, 0xbe, 0xef]);
        assert!(unhex("dead", &mut back).is_none());
        assert!(unhex("zzzzzzzz", &mut back).is_none());
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
    }
}
