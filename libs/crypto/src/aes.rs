// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! AES block cipher (FIPS 197), encryption only: AES-GCM runs it in counter
//! mode and never needs the inverse cipher. 128- and 256-bit keys.
//!
//! This is the straightforward byte-oriented formulation with an S-box
//! table, so it is not hardened against cache-timing side channels.

/// The S-box, computed at compile time: multiplicative inverse in GF(2^8)
/// followed by the affine transformation.
const SBOX: [u8; 256] = {
    let mut sbox = [0u8; 256];
    let mut x = 0;
    while x < 256 {
        // x^254 = x^-1 (and 0 maps to 0).
        let mut inv = 1u8;
        let mut e = 0;
        while e < 254 {
            inv = gf_mul(inv, x as u8);
            e += 1;
        }
        if x == 0 {
            inv = 0;
        }
        sbox[x] = inv
            ^ inv.rotate_left(1)
            ^ inv.rotate_left(2)
            ^ inv.rotate_left(3)
            ^ inv.rotate_left(4)
            ^ 0x63;
        x += 1;
    }
    sbox
};

const fn xtime(a: u8) -> u8 {
    (a << 1) ^ (((a >> 7) & 1) * 0x1b)
}

const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

pub const BLOCK_LEN: usize = 16;
const MAX_ROUNDS: usize = 14;

/// An expanded AES key.
#[derive(Clone)]
pub struct Aes {
    round_keys: [[u8; 16]; MAX_ROUNDS + 1],
    rounds: usize,
}

impl Aes {
    /// Expands a 16- or 32-byte key; `None` for any other length.
    pub fn new(key: &[u8]) -> Option<Self> {
        let (nk, rounds) = match key.len() {
            16 => (4, 10),
            32 => (8, 14),
            _ => return None,
        };
        let mut w = [[0u8; 4]; 4 * (MAX_ROUNDS + 1)];
        for (i, word) in key.chunks(4).enumerate() {
            w[i].copy_from_slice(word);
        }
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [
                    SBOX[t[1] as usize],
                    SBOX[t[2] as usize],
                    SBOX[t[3] as usize],
                    SBOX[t[0] as usize],
                ];
                t[0] ^= rcon;
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| SBOX[b as usize]);
            }
            for j in 0..4 {
                w[i][j] = w[i - nk][j] ^ t[j];
            }
        }
        let mut round_keys = [[0u8; 16]; MAX_ROUNDS + 1];
        for (r, rk) in round_keys.iter_mut().enumerate().take(rounds + 1) {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Some(Self { round_keys, rounds })
    }

    /// Encrypts one block in place.
    pub fn encrypt_block(&self, block: &mut [u8; BLOCK_LEN]) {
        let xor_key = |b: &mut [u8; 16], rk: &[u8; 16]| {
            for (x, k) in b.iter_mut().zip(rk) {
                *x ^= k;
            }
        };
        // State is column-major: byte (row r, column c) is block[4c + r].
        let sub_shift = |b: &mut [u8; 16]| {
            let old = *b;
            for c in 0..4 {
                for r in 0..4 {
                    b[4 * c + r] = SBOX[old[4 * ((c + r) % 4) + r] as usize];
                }
            }
        };
        xor_key(block, &self.round_keys[0]);
        for rk in &self.round_keys[1..self.rounds] {
            sub_shift(block);
            for col in block.as_chunks_mut::<4>().0 {
                let [a0, a1, a2, a3] = *col;
                let t = a0 ^ a1 ^ a2 ^ a3;
                col[0] ^= t ^ xtime(a0 ^ a1);
                col[1] ^= t ^ xtime(a1 ^ a2);
                col[2] ^= t ^ xtime(a2 ^ a3);
                col[3] ^= t ^ xtime(a3 ^ a0);
            }
            xor_key(block, rk);
        }
        sub_shift(block);
        xor_key(block, &self.round_keys[self.rounds]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unhex;

    fn bytes<const N: usize>(s: &str) -> [u8; N] {
        let mut b = [0u8; N];
        unhex(s, &mut b).unwrap();
        b
    }

    #[test]
    fn sbox_spot_checks() {
        assert_eq!(
            (SBOX[0x00], SBOX[0x01], SBOX[0x53], SBOX[0xff]),
            (0x63, 0x7c, 0xed, 0x16)
        );
    }

    #[test]
    fn fips197_appendix_c() {
        let pt: [u8; 16] = bytes("00112233445566778899aabbccddeeff");
        let mut b = pt;
        Aes::new(&bytes::<16>("000102030405060708090a0b0c0d0e0f"))
            .unwrap()
            .encrypt_block(&mut b);
        assert_eq!(b, bytes::<16>("69c4e0d86a7b0430d8cdb78070b4c55a"));
        let mut b = pt;
        Aes::new(&bytes::<32>(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        ))
        .unwrap()
        .encrypt_block(&mut b);
        assert_eq!(b, bytes::<16>("8ea2b7ca516745bfeafc49904b496089"));
    }

    #[test]
    fn rejects_other_key_sizes() {
        assert!(Aes::new(&[0u8; 24]).is_none());
        assert!(Aes::new(&[]).is_none());
    }
}
