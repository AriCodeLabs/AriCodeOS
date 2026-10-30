// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! ChaCha20 (RFC 8439) over RustCrypto's `chacha20` (ADR 0066), portable
//! in the kernel and AriBoot; basis of the kernel RNG ([`Rng`], ours).

use chacha20::cipher::consts::U10;
use chacha20::cipher::{
    KeyIvInit, StreamCipher, StreamCipherCore, StreamCipherSeek, StreamCipherSeekCore,
};

/// The cipher for (`key`, `nonce`), positioned at block `counter`. It
/// wipes its key when dropped.
fn at(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> chacha20::ChaCha20 {
    let mut c = chacha20::ChaCha20::new(key.into(), nonce.into());
    c.seek(u64::from(counter) * 64);
    c
}

/// One keystream block for `key`, `nonce` (96 bits) and `counter`, any
/// counter included: the stream wrapper refuses block 2^32 - 1, which
/// [`Rng::reseed`] uses, so this goes to the cipher core.
pub fn block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> [u8; 64] {
    let mut core = chacha20::ChaChaCore::<U10>::new(key.into(), nonce.into());
    core.set_block_pos(counter);
    let mut out = [0u8; 64];
    core.write_keystream_block((&mut out).into());
    out
}

/// XORs `data` with the keystream for (`key`, `nonce`) starting at block
/// `counter` (encryption and decryption are the same operation).
///
/// # Panics
///
/// If `data` runs past block 2^32 - 1, the end of the keystream: the
/// callers start at block 0 or 1 with packets of kilobytes.
pub fn xor_keystream(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    at(key, nonce, counter).apply_keystream(data);
}

pub struct Rng {
    key: [u8; 32],
    nonce: [u8; 12],
    counter: u32,
    buf: [u8; 64],
    used: usize,
}

impl Rng {
    pub const fn new(seed: [u8; 32]) -> Self {
        Self {
            key: seed,
            nonce: [0; 12],
            counter: 0,
            buf: [0; 64],
            used: 64,
        }
    }

    /// New key = ChaCha(current key) XOR entropy: never worse than before.
    pub fn reseed(&mut self, entropy: &[u8; 32]) {
        let mut mix = block(&self.key, &self.nonce, u32::MAX);
        for (k, (m, e)) in self.key.iter_mut().zip(mix.iter().zip(entropy)) {
            *k = *k ^ m ^ e;
        }
        crate::wipe(&mut mix);
        self.counter = 0;
        self.used = 64;
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            if self.used == 64 {
                self.buf = block(&self.key, &self.nonce, self.counter);
                self.counter = self.counter.wrapping_add(1);
                if self.counter == 0 {
                    // When the counter is exhausted, advance the nonce.
                    let n = u32::from_le_bytes([
                        self.nonce[0],
                        self.nonce[1],
                        self.nonce[2],
                        self.nonce[3],
                    ]);
                    self.nonce[..4].copy_from_slice(&n.wrapping_add(1).to_le_bytes());
                }
                self.used = 0;
            }
            *byte = self.buf[self.used];
            self.used += 1;
        }
    }
}

/// The key and the unread output are secret (TLS and SSH draw their
/// ephemeral keys from an `Rng`).
impl Drop for Rng {
    fn drop(&mut self) {
        crate::wipe(&mut self.key);
        crate::wipe(&mut self.buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7539_block_vector() {
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce = [0, 0, 0, 9, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let out = block(&key, &nonce, 1);
        assert_eq!(
            &out[..16],
            &[
                0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20,
                0x71, 0xc4
            ]
        );
        assert_eq!(
            &out[48..],
            &[
                0xb5, 0x12, 0x9c, 0xd1, 0xde, 0x16, 0x4e, 0xb9, 0xcb, 0xd0, 0x83, 0xe8, 0xa2, 0x50,
                0x3c, 0x4e
            ]
        );
    }

    #[test]
    fn rng_is_deterministic_and_reseed_changes_stream() {
        let mut a = Rng::new([7; 32]);
        let mut b = Rng::new([7; 32]);
        let (mut x, mut y) = ([0u8; 100], [0u8; 100]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
        assert!(x.iter().any(|&v| v != 0));
        b.reseed(&[1; 32]);
        b.fill(&mut y);
        a.fill(&mut x);
        assert_ne!(x, y);
    }

    /// The last block of a nonce: the stream wrapper of `chacha20` refuses
    /// it and `Rng::reseed` uses it (ADR 0066). Reference value from an
    /// independent implementation of RFC 8439's block function.
    #[test]
    fn last_block_of_a_nonce() {
        let key: [u8; 32] = core::array::from_fn(|i| i as u8 + 1);
        let nonce: [u8; 12] = core::array::from_fn(|i| i as u8);
        let mut expected = [0u8; 64];
        crate::unhex(
            "461d29d38817022be371e89ba0337f939c1c079b820287720fab9ccd2e9789f1\
             a660ef15c18594da1cf1351e2f868b2d028f784ee84102a8512223ca966a9f35",
            &mut expected,
        )
        .unwrap();
        assert_eq!(block(&key, &nonce, u32::MAX), expected);
        let mut r = Rng::new(key);
        r.reseed(&[0; 32]);
    }

    #[test]
    fn rng_is_wiped_when_dropped() {
        let mut r = core::mem::ManuallyDrop::new(Rng::new([7; 32]));
        r.fill(&mut [0u8; 3]);
        // SAFETY: dropped once; afterwards only plain bytes are read.
        unsafe { core::mem::ManuallyDrop::drop(&mut r) };
        assert_eq!((r.key, r.buf), ([0; 32], [0; 64]));
    }
}
