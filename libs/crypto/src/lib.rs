//! Cryptography (Phase 31): the four primitives the chain of trust needs,
//! written from scratch and checked against the official vectors
//! (FIPS 180-4, RFC 8032, RFC 7539). No `alloc`.
//!
//! - [`sha256`] and [`sha512`]: image and message hashes.
//! - [`ed25519`]: manifest signatures (the private key only lives on the host).
//! - [`chacha20`]: deterministic generator for the kernel RNG and the SSH
//!   stream cipher.
//! - [`x25519`] (RFC 7748), [`poly1305`] (RFC 8439) and [`ssh_chachapoly`]
//!   (`chacha20-poly1305@openssh.com`): the SSH transport (ADR 0025).
//!
//! Ed25519 operations are not constant time; they verify signatures and,
//! for the SSH host key, sign with a key that lives on the device. X25519
//! uses a constant-time ladder because its scalar is the ephemeral secret.

#![no_std]

pub mod chacha20;
pub mod ed25519;
mod field25519;
pub mod poly1305;
pub mod sha256;
pub mod sha512;
pub mod ssh_chachapoly;
pub mod x25519;

/// Equality comparison without short-circuit (for tags and signatures).
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

/// Writes `bytes` as lowercase hexadecimal into `out` (2 characters per byte).
pub fn hex<'a>(bytes: &[u8], out: &'a mut [u8]) -> &'a str {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let n = bytes.len().min(out.len() / 2);
    for (i, b) in bytes[..n].iter().enumerate() {
        out[2 * i] = DIGITS[(b >> 4) as usize];
        out[2 * i + 1] = DIGITS[(b & 15) as usize];
    }
    core::str::from_utf8(&out[..2 * n]).unwrap_or("")
}

/// Parses hexadecimal (upper or lower case) into `out`; `None` if the
/// length does not match or there are invalid characters.
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
