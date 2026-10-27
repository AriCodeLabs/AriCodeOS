// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AES block cipher (FIPS 197), encryption only: AES-GCM runs it in counter
//! mode and never needs the inverse cipher. 128- and 256-bit keys.
//!
//! Constant time (audit 2026-10-25, incident 0044): the S-box is a Boolean
//! circuit evaluated on the 16 bytes of the state at once (bitsliced), so
//! no memory address depends on the key or the data, where a table lookup
//! per byte leaks both through the cache. MixColumns uses [`xtime`], a
//! multiplication by a 0 or 1 bit. The expanded key is wiped when dropped.

/// Multiplication by x in GF(2^8), without a branch on the top bit.
const fn xtime(a: u8) -> u8 {
    (a << 1) ^ (((a >> 7) & 1) * 0x1b)
}

/// The S-box on 8 bit planes (plane i holds bit i of up to 16 bytes): the
/// depth-16 circuit of Boyar and Peralta ("A depth-16 circuit for the AES
/// S-box", 2011, 113 gates), in the order BearSSL's `aes_ct` writes it.
/// The inversion in GF(2^8) and the affine map are both in the gates.
fn sbox_planes(q: &mut [u16; 8]) {
    let (x0, x1, x2, x3) = (q[7], q[6], q[5], q[4]);
    let (x4, x5, x6, x7) = (q[3], q[2], q[1], q[0]);

    // Top linear transformation.
    let y14 = x3 ^ x5;
    let y13 = x0 ^ x6;
    let y9 = x0 ^ x3;
    let y8 = x0 ^ x5;
    let t0 = x1 ^ x2;
    let y1 = t0 ^ x7;
    let y4 = y1 ^ x3;
    let y12 = y13 ^ y14;
    let y2 = y1 ^ x0;
    let y5 = y1 ^ x6;
    let y3 = y5 ^ y8;
    let t1 = x4 ^ y12;
    let y15 = t1 ^ x5;
    let y20 = t1 ^ x1;
    let y6 = y15 ^ x7;
    let y10 = y15 ^ t0;
    let y11 = y20 ^ y9;
    let y7 = x7 ^ y11;
    let y17 = y10 ^ y11;
    let y19 = y10 ^ y8;
    let y16 = t0 ^ y11;
    let y21 = y13 ^ y16;
    let y18 = x0 ^ y16;

    // Non-linear section.
    let t2 = y12 & y15;
    let t3 = y3 & y6;
    let t4 = t3 ^ t2;
    let t5 = y4 & x7;
    let t6 = t5 ^ t2;
    let t7 = y13 & y16;
    let t8 = y5 & y1;
    let t9 = t8 ^ t7;
    let t10 = y2 & y7;
    let t11 = t10 ^ t7;
    let t12 = y9 & y11;
    let t13 = y14 & y17;
    let t14 = t13 ^ t12;
    let t15 = y8 & y10;
    let t16 = t15 ^ t12;
    let t17 = t4 ^ t14;
    let t18 = t6 ^ t16;
    let t19 = t9 ^ t14;
    let t20 = t11 ^ t16;
    let t21 = t17 ^ y20;
    let t22 = t18 ^ y19;
    let t23 = t19 ^ y21;
    let t24 = t20 ^ y18;

    let t25 = t21 ^ t22;
    let t26 = t21 & t23;
    let t27 = t24 ^ t26;
    let t28 = t25 & t27;
    let t29 = t28 ^ t22;
    let t30 = t23 ^ t24;
    let t31 = t22 ^ t26;
    let t32 = t31 & t30;
    let t33 = t32 ^ t24;
    let t34 = t23 ^ t33;
    let t35 = t27 ^ t33;
    let t36 = t24 & t35;
    let t37 = t36 ^ t34;
    let t38 = t27 ^ t36;
    let t39 = t29 & t38;
    let t40 = t25 ^ t39;

    let t41 = t40 ^ t37;
    let t42 = t29 ^ t33;
    let t43 = t29 ^ t40;
    let t44 = t33 ^ t37;
    let t45 = t42 ^ t41;
    let z0 = t44 & y15;
    let z1 = t37 & y6;
    let z2 = t33 & x7;
    let z3 = t43 & y16;
    let z4 = t40 & y1;
    let z5 = t29 & y7;
    let z6 = t42 & y11;
    let z7 = t45 & y17;
    let z8 = t41 & y10;
    let z9 = t44 & y12;
    let z10 = t37 & y3;
    let z11 = t33 & y4;
    let z12 = t43 & y13;
    let z13 = t40 & y5;
    let z14 = t29 & y2;
    let z15 = t42 & y9;
    let z16 = t45 & y14;
    let z17 = t41 & y8;

    // Bottom linear transformation (the NOTs add the constant 0x63).
    let t46 = z15 ^ z16;
    let t47 = z10 ^ z11;
    let t48 = z5 ^ z13;
    let t49 = z9 ^ z10;
    let t50 = z2 ^ z12;
    let t51 = z2 ^ z5;
    let t52 = z7 ^ z8;
    let t53 = z0 ^ z3;
    let t54 = z6 ^ z7;
    let t55 = z16 ^ z17;
    let t56 = z12 ^ t48;
    let t57 = t50 ^ t53;
    let t58 = z4 ^ t46;
    let t59 = z3 ^ t54;
    let t60 = t46 ^ t57;
    let t61 = z14 ^ t57;
    let t62 = t52 ^ t58;
    let t63 = t49 ^ t58;
    let t64 = z4 ^ t59;
    let t65 = t61 ^ t62;
    let t66 = z1 ^ t63;
    let s0 = t59 ^ t63;
    let s6 = t56 ^ !t62;
    let s7 = t48 ^ !t60;
    let t67 = t64 ^ t65;
    let s3 = t53 ^ t66;
    let s4 = t51 ^ t66;
    let s5 = t47 ^ t65;
    let s1 = t64 ^ !s3;
    let s2 = t55 ^ !t67;

    *q = [s7, s6, s5, s4, s3, s2, s1, s0];
}

/// Transposes the 8x8 bit matrix whose row r is byte r (little endian):
/// bit c of byte r goes to bit r of byte c (Hacker's Delight, 7-3).
fn transpose8(mut x: u64) -> u64 {
    let t = (x ^ (x >> 7)) & 0x00AA_00AA_00AA_00AA;
    x ^= t ^ (t << 7);
    let t = (x ^ (x >> 14)) & 0x0000_CCCC_0000_CCCC;
    x ^= t ^ (t << 14);
    let t = (x ^ (x >> 28)) & 0x0000_0000_F0F0_F0F0;
    x ^ t ^ (t << 28)
}

/// SubBytes on 16 bytes: into bit planes (plane i, bit j = bit i of byte
/// j), through the circuit and back.
fn sub_bytes(b: &mut [u8; 16]) {
    let (lo, hi) = b.split_at(8);
    let lo = transpose8(u64::from_le_bytes(lo.try_into().unwrap_or([0; 8])));
    let hi = transpose8(u64::from_le_bytes(hi.try_into().unwrap_or([0; 8])));
    let mut q: [u16; 8] = core::array::from_fn(|i| {
        ((lo >> (8 * i)) & 0xFF) as u16 | (((hi >> (8 * i)) & 0xFF) as u16) << 8
    });
    sbox_planes(&mut q);
    let lo = u64::from_le_bytes(q.map(|p| p as u8));
    let hi = u64::from_le_bytes(q.map(|p| (p >> 8) as u8));
    b[..8].copy_from_slice(&transpose8(lo).to_le_bytes());
    b[8..].copy_from_slice(&transpose8(hi).to_le_bytes());
}

/// SubWord of the key expansion.
fn sub_word(w: [u8; 4]) -> [u8; 4] {
    let mut b = [0u8; 16];
    b[..4].copy_from_slice(&w);
    sub_bytes(&mut b);
    [b[0], b[1], b[2], b[3]]
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
                t = sub_word([t[1], t[2], t[3], t[0]]);
                t[0] ^= rcon;
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = sub_word(t);
            }
            for j in 0..4 {
                w[i][j] = w[i - nk][j] ^ t[j];
            }
        }
        let mut aes = Self {
            round_keys: [[0u8; 16]; MAX_ROUNDS + 1],
            rounds,
        };
        for (r, rk) in aes.round_keys.iter_mut().enumerate().take(rounds + 1) {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        crate::wipe(w.as_flattened_mut());
        Some(aes)
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
            sub_bytes(b);
            let old = *b;
            for c in 0..4 {
                for r in 0..4 {
                    b[4 * c + r] = old[4 * ((c + r) % 4) + r];
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

impl Drop for Aes {
    fn drop(&mut self) {
        crate::wipe(self.round_keys.as_flattened_mut());
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

    /// The table S-box AES used before incident 0044: multiplicative
    /// inverse in GF(2^8) (x^254, 0 maps to 0) followed by the affine
    /// transformation.
    const TABLE: [u8; 256] = {
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
        let mut sbox = [0u8; 256];
        let mut x = 0;
        while x < 256 {
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

    #[test]
    fn bitsliced_sbox_equals_the_table() {
        assert_eq!(
            (TABLE[0x00], TABLE[0x01], TABLE[0x53], TABLE[0xff]),
            (0x63, 0x7c, 0xed, 0x16)
        );
        // Every input in every one of the 16 lanes.
        for x in 0..256usize {
            let mut b: [u8; 16] = core::array::from_fn(|j| (x + 37 * j) as u8);
            let expected = b.map(|v| TABLE[v as usize]);
            sub_bytes(&mut b);
            assert_eq!(b, expected, "input {x}");
        }
        assert_eq!(sub_word([0x00, 0x01, 0x53, 0xff]), [0x63, 0x7c, 0xed, 0x16]);
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

    #[test]
    fn expanded_key_is_wiped_when_dropped() {
        let mut aes = core::mem::ManuallyDrop::new(Aes::new(&[0xA5; 32]).unwrap());
        assert!(aes.round_keys.as_flattened().iter().any(|&b| b != 0));
        // SAFETY: dropped once; afterwards only its plain bytes are read.
        unsafe { core::mem::ManuallyDrop::drop(&mut aes) };
        assert!(aes.round_keys.as_flattened().iter().all(|&b| b == 0));
    }
}
