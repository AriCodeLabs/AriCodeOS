// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Arithmetic in GF(2^255 - 19) with five 51-bit limbs, shared by Ed25519
//! and X25519. Only multiplication by small constants and the swap used by
//! the X25519 ladder are written to be constant time; inversion and
//! comparison are not (they only see public values or the final result).

/// Element of GF(p), p = 2^255 - 19, in five 51-bit limbs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fe(pub(crate) [u64; 5]);

const MASK51: u64 = (1 << 51) - 1;
/// p in 51-bit limbs.
const P: [u64; 5] = [
    0x7FFFFFFFFFFED,
    0x7FFFFFFFFFFFF,
    0x7FFFFFFFFFFFF,
    0x7FFFFFFFFFFFF,
    0x7FFFFFFFFFFFF,
];

impl Fe {
    pub(crate) const ZERO: Fe = Fe([0; 5]);
    pub(crate) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    pub(crate) fn from_u64(v: u64) -> Fe {
        Fe([v & MASK51, v >> 51, 0, 0, 0])
    }

    /// Loads 255 little-endian bits (bit 255 is ignored).
    pub(crate) fn from_bytes(b: &[u8; 32]) -> Fe {
        let load = |i: usize| -> u64 {
            let mut v = 0u64;
            for k in 0..8 {
                v |= (b[i + k] as u64) << (8 * k);
            }
            v
        };
        let l0 = load(0) & MASK51;
        let l1 = (load(6) >> 3) & MASK51;
        let l2 = (load(12) >> 6) & MASK51;
        let l3 = (load(19) >> 1) & MASK51;
        let l4 = (load(24) >> 12) & MASK51;
        Fe([l0, l1, l2, l3, l4])
    }

    /// Propagates carries: leaves each limb below 2^51 (+ a small excess).
    pub(crate) fn carry(mut self) -> Fe {
        for i in 0..4 {
            let c = self.0[i] >> 51;
            self.0[i] &= MASK51;
            self.0[i + 1] += c;
        }
        let c = self.0[4] >> 51;
        self.0[4] &= MASK51;
        self.0[0] += c * 19;
        let c = self.0[0] >> 51;
        self.0[0] &= MASK51;
        self.0[1] += c;
        self
    }

    /// Canonical representation in [0, p).
    pub(crate) fn to_bytes(self) -> [u8; 32] {
        let mut h = self.carry().carry();
        // Subtract p if h >= p.
        let mut q = (h.0[0] + 19) >> 51;
        for limb in &h.0[1..] {
            q = (limb + q) >> 51;
        }
        h.0[0] += 19 * q;
        for i in 0..4 {
            let c = h.0[i] >> 51;
            h.0[i] &= MASK51;
            h.0[i + 1] += c;
        }
        h.0[4] &= MASK51;
        let mut out = [0u8; 32];
        let bits = |i: usize| h.0[i] as u128;
        let packed: u128 = bits(0) | bits(1) << 51 | bits(2) << 102;
        out[..16].copy_from_slice(&packed.to_le_bytes());
        // bits 128.. : rest of limb 2 (25 bits), limbs 3 and 4.
        let upper: u128 = (bits(2) >> 26) | bits(3) << 25 | bits(4) << 76;
        out[16..].copy_from_slice(&upper.to_le_bytes());
        out
    }

    pub(crate) fn add(self, o: Fe) -> Fe {
        Fe([
            self.0[0] + o.0[0],
            self.0[1] + o.0[1],
            self.0[2] + o.0[2],
            self.0[3] + o.0[3],
            self.0[4] + o.0[4],
        ])
        .carry()
    }

    pub(crate) fn sub(self, o: Fe) -> Fe {
        // self + 2p - o avoids negatives (the limbs of self are reduced).
        Fe([
            self.0[0] + 2 * P[0] - o.0[0],
            self.0[1] + 2 * P[1] - o.0[1],
            self.0[2] + 2 * P[2] - o.0[2],
            self.0[3] + 2 * P[3] - o.0[3],
            self.0[4] + 2 * P[4] - o.0[4],
        ])
        .carry()
    }

    pub(crate) fn neg(self) -> Fe {
        Fe::ZERO.sub(self)
    }

    pub(crate) fn mul(self, o: Fe) -> Fe {
        let a = self.0.map(|v| v as u128);
        let b = o.0.map(|v| v as u128);
        let r0 = a[0] * b[0] + 19 * (a[1] * b[4] + a[2] * b[3] + a[3] * b[2] + a[4] * b[1]);
        let r1 = a[0] * b[1] + a[1] * b[0] + 19 * (a[2] * b[4] + a[3] * b[3] + a[4] * b[2]);
        let r2 = a[0] * b[2] + a[1] * b[1] + a[2] * b[0] + 19 * (a[3] * b[4] + a[4] * b[3]);
        let r3 = a[0] * b[3] + a[1] * b[2] + a[2] * b[1] + a[3] * b[0] + 19 * (a[4] * b[4]);
        let r4 = a[0] * b[4] + a[1] * b[3] + a[2] * b[2] + a[3] * b[1] + a[4] * b[0];
        let mut r = [r0, r1, r2, r3, r4];
        for i in 0..4 {
            let c = r[i] >> 51;
            r[i] &= MASK51 as u128;
            r[i + 1] += c;
        }
        let c = r[4] >> 51;
        r[4] &= MASK51 as u128;
        r[0] += c * 19;
        Fe(r.map(|v| v as u64)).carry()
    }

    pub(crate) fn square(self) -> Fe {
        self.mul(self)
    }

    /// self^exp, little-endian exponent.
    pub(crate) fn pow(self, exp: &[u8; 32]) -> Fe {
        let mut result = Fe::ONE;
        for i in (0..256).rev() {
            result = result.square();
            if (exp[i / 8] >> (i % 8)) & 1 == 1 {
                result = result.mul(self);
            }
        }
        result
    }

    pub(crate) fn invert(self) -> Fe {
        // p - 2 = 2^255 - 21
        let mut e = [0xFFu8; 32];
        e[0] = 0xEB;
        e[31] = 0x7F;
        self.pow(&e)
    }

    /// self^((p-5)/8), (p-5)/8 = 2^252 - 3.
    pub(crate) fn pow22523(self) -> Fe {
        let mut e = [0xFFu8; 32];
        e[0] = 0xFD;
        e[31] = 0x0F;
        self.pow(&e)
    }

    pub(crate) fn is_zero(self) -> bool {
        self.to_bytes() == [0u8; 32]
    }

    pub(crate) fn is_negative(self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }

    pub(crate) fn eq(self, o: Fe) -> bool {
        self.to_bytes() == o.to_bytes()
    }

    /// Swaps `a` and `b` when `swap` is 1, without branching on it (the
    /// X25519 ladder must not leak the secret scalar's bits).
    pub(crate) fn cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
        let mask = 0u64.wrapping_sub(swap);
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}
