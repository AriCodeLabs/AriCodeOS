// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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
//! - TLS 1.3 client (HTTPS): [`sha512::Sha384`], [`hmac`], [`hkdf`]
//!   (RFC 5869), [`aead`] (ChaCha20-Poly1305 of RFC 8439 and AES-GCM over
//!   [`aes`]) and [`rsa`] signature verification (PKCS#1 v1.5 and PSS) over
//!   [`bigint`].
//!
//! What handles secrets runs in constant time: Ed25519 signing, the X25519
//! ladder and AES (a bitsliced S-box, no table). Ed25519 verification
//! works on public values only. Key types (`Aes`, `AesGcm`, `ChaChaPoly`,
//! `Poly1305`, the hash states, `Rng`) wipe themselves when dropped, and
//! [`SecretBytes`] does it for a secret array on the stack (incident 0044).

#![no_std]

pub mod aead;
pub mod aes;
pub mod bigint;
pub mod chacha20;
pub mod ecdsa;
pub mod ed25519;
mod field25519;
pub mod hkdf;
pub mod hmac;
mod p_curves;
pub mod poly1305;
pub mod rsa;
pub mod sha256;
pub mod sha512;
pub mod ssh_chachapoly;
pub mod trust;
pub mod x25519;

/// Hash functions selectable at run time (signature schemes, certificates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    /// Digest length in bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> usize {
        match self {
            Hash::Sha256 => sha256::DIGEST_LEN,
            Hash::Sha384 => sha512::SHA384_DIGEST_LEN,
            Hash::Sha512 => sha512::DIGEST_LEN,
        }
    }

    /// Hashes `data` into the front of `out`; returns the digest length.
    pub fn digest(self, data: &[u8], out: &mut [u8; 64]) -> usize {
        let n = self.len();
        match self {
            Hash::Sha256 => out[..n].copy_from_slice(&sha256::sha256(data)),
            Hash::Sha384 => out[..n].copy_from_slice(&sha512::sha384(data)),
            Hash::Sha512 => out[..n].copy_from_slice(&sha512::sha512(data)),
        }
        n
    }
}

/// Equality comparison without short-circuit (for tags and signatures).
/// The accumulator goes through [`core::hint::black_box`] at every byte,
/// so the optimizer cannot see that a nonzero value is final and turn the
/// loop into an early exit (audit 2026-10-25, incident 0044).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff = core::hint::black_box(diff | (x ^ y));
    }
    diff == 0
}

/// Zeroes memory that held secret material (a seed, a key, a key
/// schedule), with writes the compiler cannot drop as dead stores.
pub fn wipe<T: Copy + Default>(buf: &mut [T]) {
    for b in buf.iter_mut() {
        // SAFETY: a valid, exclusive `&mut T`.
        unsafe { core::ptr::write_volatile(b, T::default()) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// A secret byte array that is [`wipe`]d when it goes out of scope, on
/// every way out of the function holding it (an early `?` included).
/// Copies the compiler makes when the value moves are not covered.
pub struct SecretBytes<const N: usize>(pub [u8; N]);

impl<const N: usize> Drop for SecretBytes<N> {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
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
    fn wipe_zeroes_everything() {
        let mut buf = [0xFFu8; 40];
        wipe(&mut buf);
        assert!(buf.iter().all(|&b| b == 0));
        let mut words = [u64::MAX; 5];
        wipe(&mut words);
        assert_eq!(words, [0; 5]);
    }

    #[test]
    fn secret_bytes_are_wiped_when_dropped() {
        let mut s = core::mem::ManuallyDrop::new(SecretBytes([0xA5u8; 32]));
        // SAFETY: dropped once; afterwards only plain bytes are read.
        unsafe { core::mem::ManuallyDrop::drop(&mut s) };
        assert_eq!(s.0, [0; 32]);
    }

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

    #[test]
    fn hash_dispatch() {
        let mut out = [0u8; 64];
        for (h, len) in [(Hash::Sha256, 32), (Hash::Sha384, 48), (Hash::Sha512, 64)] {
            assert_eq!(h.len(), len);
            assert_eq!(h.digest(b"abc", &mut out), len);
        }
        // The last call (SHA-512) wrote all 64 bytes.
        assert_eq!(out, sha512::sha512(b"abc"));
        Hash::Sha256.digest(b"abc", &mut out);
        assert_eq!(out[..32], sha256::sha256(b"abc"));
    }
}
