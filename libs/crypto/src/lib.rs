// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Cryptography: one interface over the implementations that measured
//! best (ADR 0063, ADR 0066), with this crate's policy on top (strict
//! verification, wiping key material).
//!
//! - Adopted: [`sha256`] and [`sha512`] (`sha2`), [`aead`] (`aes-gcm`,
//!   `chacha20poly1305`), [`chacha20`] (`chacha20`), [`poly1305`]
//!   (`poly1305`), [`ed25519`] (`ed25519-dalek`) and [`x25519`]
//!   (`x25519-dalek`). User programs get their SIMD backends, detected at
//!   run time; the kernel and AriBoot build them portable (feature `soft`
//!   and the `--cfg` flags of `.cargo/config.toml`), since they must never
//!   execute SIMD instructions (ADR 0065).
//! - Ours, faster than the adopted alternatives: [`ecdsa`] (P-256, P-384)
//!   and [`rsa`] (PKCS#1 v1.5 and PSS) verification over [`bigint`].
//! - Ours, thin: [`hmac`], [`hkdf`] (RFC 5869), the kernel generator
//!   ([`chacha20::Rng`]), [`ssh_chachapoly`]
//!   (`chacha20-poly1305@openssh.com`) and [`trust`].
//!
//! Every implementation passes the Wycheproof vectors it supports. What
//! handles secrets runs in constant time; [`unhex`] does not (it branches
//! on each character). Key types and hash states wipe themselves when
//! dropped, whatever backend was selected, and [`SecretBytes`] does it for
//! a secret array on the stack (incident 0044).

#![no_std]

pub mod aead;
pub mod bigint;
pub mod chacha20;
pub mod ecdsa;
pub mod ed25519;
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

/// Unsigned integers: every byte of the value is data (no padding, no
/// niche), so writing zero clears all of it.
pub trait Word: Copy + sealed::Sealed {
    const ZERO: Self;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! words {
    ($($t:ty),*) => {$(
        impl sealed::Sealed for $t {}
        impl Word for $t {
            const ZERO: Self = 0;
        }
    )*};
}
words!(u8, u32, u64, u128);

/// Zeroes memory that held secret material (a seed, a key, a key
/// schedule), with writes the compiler cannot drop as dead stores.
pub fn wipe<T: Word>(buf: &mut [T]) {
    for b in buf.iter_mut() {
        // SAFETY: a valid, exclusive `&mut T`.
        unsafe { core::ptr::write_volatile(b, T::ZERO) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Zeroes every byte of a state of an adopted crate (ADR 0066) that holds
/// key material but does not wipe itself on every backend: `sha2` 0.10 has
/// no `zeroize` support, and the run-time detected backend of `polyval`
/// (GHASH, inside `aes-gcm`) keeps its key in a `ManuallyDrop` that is
/// never dropped.
///
/// # Safety
///
/// All-zero bytes must be a valid `T`, since `T`'s own `Drop` (if any)
/// runs afterwards. Audited for the versions `Cargo.toml` pins with `=`
/// (audit of ADR 0066, L2):
/// - `sha2::Sha256`, `Sha384`, `Sha512`: `u32`/`u64` arrays, a `u64`
///   block count, a byte buffer with a `u8` position, `PhantomData`;
/// - `aes_gcm::Aes128Gcm`, `Aes256Gcm`: `aes` key schedules (a union of
///   `ManuallyDrop` AES-NI and portable schedules plus a zero-sized CPU
///   detection token) and `ghash`/`polyval` (the same shape, `u64` pairs).
///
/// None holds a reference, a `NonZero`, an enum or an `Option`. A version
/// bump of these crates (or of `digest`, `block-buffer`, `cipher`) needs
/// this list checked again.
unsafe fn wipe_state<T>(state: &mut T) {
    // SAFETY: an exclusive reference covers `size_of::<T>()` bytes.
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(state as *mut T as *mut u8, core::mem::size_of::<T>())
    };
    wipe(bytes);
}

/// Words (8 bytes) [`scrub_stack`] clears after a curve operation on a
/// secret (Ed25519 signing and public key, X25519), 4 KiB: they reach 2.5
/// KiB of stack, measured by painting it, in the kernel's and in user
/// programs' builds.
const SCRUB_CURVE: usize = 512;
/// The same after HMAC and AES key expansion, 2 KiB (at most 1.1 KiB).
const SCRUB_HASH: usize = 256;

/// Runs `f` in a frame of its own, then zeroes the `N` words of stack
/// below, where `f` and what it called kept their locals. The adopted
/// crates leave secrets there that our own code used to wipe: Ed25519's
/// nonce and secret scalar, X25519's clamped scalar and ladder, `sha2`'s
/// message schedule (audit of ADR 0066, M1). `f` must not be inlined into
/// the caller, or its locals would sit in the caller's frame, above the
/// scrubbed area.
fn scrubbed<const N: usize, T>(f: impl FnOnce() -> T) -> T {
    let value = isolated(f);
    scrub_stack::<N>();
    value
}

#[inline(never)]
fn isolated<T>(f: impl FnOnce() -> T) -> T {
    f()
}

/// Zeroes the `N` words of stack just below the caller. Never inlined, so
/// its frame starts where the frame of the function called before it did.
///
/// The area is not initialized first: `[0u64; N]` becomes a call to
/// `memset`, whose return address and saved registers would land below
/// the area. Volatile word stores stay stores.
#[inline(never)]
fn scrub_stack<const N: usize>() {
    let mut area = core::mem::MaybeUninit::<[u64; N]>::uninit();
    let p = area.as_mut_ptr().cast::<u64>();
    for i in 0..N {
        // SAFETY: within the `N` words of `area`.
        unsafe { p.add(i).write_volatile(0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    core::hint::black_box(&area);
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
    // Without a table or a branch on the nibble: callers print seeds and
    // keys. 39 moves 10..=15 from ':'..='?' to 'a'..='f'.
    fn digit(n: u8) -> u8 {
        let above_9 = 0u8.wrapping_sub((9u8.wrapping_sub(n) >> 7) & 1);
        b'0' + n + (above_9 & 39)
    }
    let n = bytes.len().min(out.len() / 2);
    for (i, b) in bytes[..n].iter().enumerate() {
        out[2 * i] = digit(b >> 4);
        out[2 * i + 1] = digit(b & 15);
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
    extern crate std;

    /// The bytes of `v`, to check that a dropped state was wiped.
    pub(crate) fn bytes_of<T>(v: &T) -> &[u8] {
        // SAFETY: reads `size_of::<T>()` bytes of a live value; the callers
        // only pass values whose every byte was written by `wipe`.
        unsafe {
            core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>())
        }
    }

    /// Runs `f` and returns a copy of the stack it used below the caller
    /// (x86_64 only; reads memory below `rsp` that this thread owns).
    #[cfg(target_arch = "x86_64")]
    #[inline(never)]
    fn stack_after(f: &dyn Fn()) -> std::vec::Vec<u8> {
        const DEPTH: usize = 16 * 1024;
        let top: usize;
        // SAFETY: reads rsp.
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) top, options(nomem, nostack)) };
        for a in top - DEPTH..top - 256 {
            // SAFETY: the thread's stack, below every live frame.
            unsafe { core::ptr::write_volatile(a as *mut u8, 0) };
        }
        f();
        (top - DEPTH..top - 256)
            // SAFETY: as above.
            .map(|a| unsafe { core::ptr::read_volatile(a as *const u8) })
            .collect()
    }

    /// Whether any 8 consecutive bytes of `secret` appear in `stack`.
    #[cfg(target_arch = "x86_64")]
    fn found(stack: &[u8], secret: &[u8]) -> bool {
        secret
            .windows(8)
            .any(|w| w.iter().any(|&b| b != 0) && stack.windows(8).any(|s| s == w))
    }

    /// Audit of ADR 0066, M1: the adopted crates leave Ed25519's expanded
    /// key and nonce, X25519's clamped scalar and HMAC's padded keys on the
    /// stack; the facade scrubs them. Each secret is searched for after the
    /// operation returns.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn secret_operations_leave_nothing_on_the_stack() {
        use std::vec::Vec;
        let seed: [u8; 32] = core::array::from_fn(|i| 0x40 + i as u8);
        let msg = b"message to sign";
        let expanded = sha512::sha512(&seed);
        let mut nonce_input = expanded[32..].to_vec();
        nonce_input.extend_from_slice(msg);
        let nonce_hash = sha512::sha512(&nonce_input);
        let mut clamped = seed;
        clamped[0] &= 248;
        clamped[31] = (clamped[31] & 127) | 64;
        // The secret scalar a (clamped, reduced mod L, as `ed25519-dalek`
        // keeps it) and the nonce r of this seed and message, computed
        // with Python integers: r = SHA-512(prefix || msg) mod L.
        let mut a = [0u8; 32];
        unhex(
            "bf040757e91311c04b94092aaa0577f2e8165e486417ad3ae5e48b49290cd606",
            &mut a,
        )
        .unwrap();
        let mut r = [0u8; 32];
        unhex(
            "3578efe9df8c32423e8a5175547011752ec8b550c556a80a3c5d258e277f9b0a",
            &mut r,
        )
        .unwrap();
        let peer = x25519::public_key(&[5u8; 32]);
        let hkey: Vec<u8> = (0..100u8).map(|i| i ^ 0x5C).collect();
        let hashed_key = sha256::sha256(&hkey);
        let ipad: Vec<u8> = hashed_key.iter().map(|b| b ^ 0x36).collect();
        let cases: [(&str, &dyn Fn(), Vec<&[u8]>); 4] = [
            (
                "ed25519 sign",
                &|| {
                    core::hint::black_box(ed25519::sign(&seed, msg));
                },
                std::vec![&seed[..], &expanded[..], &nonce_hash[..], &a[..], &r[..]],
            ),
            (
                "ed25519 public key",
                &|| {
                    core::hint::black_box(ed25519::public_key(&seed));
                },
                std::vec![&seed[..], &expanded[..], &a[..]],
            ),
            (
                "x25519",
                &|| {
                    core::hint::black_box(x25519::scalar_mult(&seed, &peer));
                },
                std::vec![&seed[..], &clamped[..]],
            ),
            (
                "hmac-sha256",
                &|| {
                    core::hint::black_box(hmac::hmac_sha256(&hkey, &[b"m"]));
                },
                std::vec![&hkey[..], &hashed_key[..], &ipad[..]],
            ),
        ];
        for (name, f, secrets) in cases {
            let stack = stack_after(f);
            for (i, secret) in secrets.iter().enumerate() {
                assert!(
                    !found(&stack, secret),
                    "{name}: secret {i} left on the stack"
                );
            }
        }
    }

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
        // Every byte, against the table the branchless digits replaced.
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        for b in 0..=255u8 {
            let mut two = [0u8; 2];
            hex(&[b], &mut two);
            assert_eq!(two, [DIGITS[(b >> 4) as usize], DIGITS[(b & 15) as usize]]);
        }
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
