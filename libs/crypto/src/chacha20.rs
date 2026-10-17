// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! ChaCha20 (RFC 7539): stream of 64-byte blocks; basis of the kernel RNG.

const CONSTANTS: [u32; 4] = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574];

#[inline(always)]
fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One keystream block for `key`, `nonce` (96 bits) and `counter`.
pub fn block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&CONSTANTS);
    for (i, k) in key.as_chunks::<4>().0.iter().enumerate() {
        state[4 + i] = u32::from_le_bytes(*k);
    }
    state[12] = counter;
    for (i, n) in nonce.as_chunks::<4>().0.iter().enumerate() {
        state[13 + i] = u32::from_le_bytes(*n);
    }
    let mut w = state;
    for _ in 0..10 {
        quarter(&mut w, 0, 4, 8, 12);
        quarter(&mut w, 1, 5, 9, 13);
        quarter(&mut w, 2, 6, 10, 14);
        quarter(&mut w, 3, 7, 11, 15);
        quarter(&mut w, 0, 5, 10, 15);
        quarter(&mut w, 1, 6, 11, 12);
        quarter(&mut w, 2, 7, 8, 13);
        quarter(&mut w, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for (i, o) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *o = w[i].wrapping_add(state[i]).to_le_bytes();
    }
    out
}

/// Deterministic generator: consecutive ChaCha20 blocks over a seed key;
/// `reseed` mixes fresh entropy into the key.
/// XORs `data` with the keystream for (`key`, `nonce`) starting at block
/// `counter` (encryption and decryption are the same operation).
pub fn xor_keystream(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = block(key, nonce, counter.wrapping_add(i as u32));
        for (b, k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= k;
        }
    }
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
        let mix = block(&self.key, &self.nonce, u32::MAX);
        for (k, (m, e)) in self.key.iter_mut().zip(mix.iter().zip(entropy)) {
            *k = *k ^ m ^ e;
        }
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
}
