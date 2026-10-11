//! Fixed-capacity unsigned big integers for RSA verification: modular
//! exponentiation with Montgomery multiplication, no `alloc`. Moduli must be
//! odd and at most [`MAX_BITS`] bits long. Not constant time: it only ever
//! processes public values (signatures and public keys).

pub const MAX_BITS: usize = 4096;
const MAX_LIMBS: usize = MAX_BITS / 64;

type Limbs = [u64; MAX_LIMBS];

/// Little-endian limbs of a big-endian byte string; `None` if the value
/// (ignoring leading zeros) does not fit in `len` limbs.
fn limbs_from_be(bytes: &[u8], len: usize) -> Option<Limbs> {
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let bytes = &bytes[first..];
    if bytes.len() > len * 8 {
        return None;
    }
    let mut out = [0u64; MAX_LIMBS];
    for (i, &b) in bytes.iter().rev().enumerate() {
        out[i / 8] |= (b as u64) << (8 * (i % 8));
    }
    Some(out)
}

/// `a >= b` over the first `len` limbs.
fn ge(a: &[u64], b: &[u64]) -> bool {
    for (x, y) in a.iter().zip(b).rev() {
        if x != y {
            return x > y;
        }
    }
    true
}

/// `a -= b`, returning the borrow.
fn sub_assign(a: &mut [u64], b: &[u64]) -> bool {
    let mut borrow = false;
    for (x, &y) in a.iter_mut().zip(b) {
        let (d, b1) = x.overflowing_sub(y);
        let (d, b2) = d.overflowing_sub(borrow as u64);
        *x = d;
        borrow = b1 || b2;
    }
    borrow
}

/// An odd modulus with its Montgomery constants (R = 2^(64 * len)).
pub struct Modulus {
    n: Limbs,
    len: usize,
    /// Length of the modulus in bytes, without leading zeros.
    byte_len: usize,
    /// -n^-1 mod 2^64.
    n0inv: u64,
    /// R^2 mod n.
    rr: Limbs,
}

impl Modulus {
    /// Builds the context for the big-endian modulus `n` (leading zeros are
    /// ignored). `None` if it is even, smaller than 3 or too large.
    pub fn new(n: &[u8]) -> Option<Self> {
        let first = n.iter().position(|&b| b != 0)?;
        let byte_len = n.len() - first;
        let len = byte_len.div_ceil(8);
        let n = limbs_from_be(n, MAX_LIMBS)?;
        if n[0] & 1 == 0 || (len == 1 && n[0] < 3) {
            return None;
        }
        // Newton iteration doubles the correct low bits each step: 1 -> 64.
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(n[0].wrapping_mul(inv)));
        }
        let mut m = Self {
            n,
            len,
            byte_len,
            n0inv: inv.wrapping_neg(),
            rr: [0; MAX_LIMBS],
        };
        // R^2 mod n by doubling 1 a total of 2 * 64 * len times.
        let mut r = [0u64; MAX_LIMBS + 1];
        r[0] = 1;
        for _ in 0..128 * len {
            let mut carry = 0;
            for limb in &mut r[..=len] {
                let next = *limb >> 63;
                *limb = (*limb << 1) | carry;
                carry = next;
            }
            if r[len] != 0 || ge(&r[..len], &m.n[..len]) {
                let borrow = sub_assign(&mut r[..len], &m.n[..len]);
                r[len] -= borrow as u64;
            }
        }
        m.rr[..len].copy_from_slice(&r[..len]);
        Some(m)
    }

    /// Length of the modulus in bytes.
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Length of the modulus in bits.
    pub fn bits(&self) -> usize {
        64 * self.len - self.n[self.len - 1].leading_zeros() as usize
    }

    /// Montgomery product `a * b / R mod n` (CIOS method).
    fn mul(&self, a: &Limbs, b: &Limbs) -> Limbs {
        let len = self.len;
        let n = &self.n;
        let mut t = [0u64; MAX_LIMBS + 2];
        for &bi in &b[..len] {
            let mut c = 0u128;
            for j in 0..len {
                c += t[j] as u128 + a[j] as u128 * bi as u128;
                t[j] = c as u64;
                c >>= 64;
            }
            c += t[len] as u128;
            t[len] = c as u64;
            t[len + 1] = (c >> 64) as u64;

            let m = t[0].wrapping_mul(self.n0inv);
            let mut c = (t[0] as u128 + m as u128 * n[0] as u128) >> 64;
            for j in 1..len {
                c += t[j] as u128 + m as u128 * n[j] as u128;
                t[j - 1] = c as u64;
                c >>= 64;
            }
            c += t[len] as u128;
            t[len - 1] = c as u64;
            t[len] = t[len + 1] + (c >> 64) as u64;
        }
        if t[len] != 0 || ge(&t[..len], &n[..len]) {
            sub_assign(&mut t[..len], &n[..len]);
        }
        let mut out = [0u64; MAX_LIMBS];
        out[..len].copy_from_slice(&t[..len]);
        out
    }

    /// Writes `base^exp mod n` big-endian into `out`, which must be
    /// [`Self::byte_len`] bytes long. `None` if `base >= n` or `out` has the
    /// wrong length.
    pub fn pow(&self, base: &[u8], exp: &[u8], out: &mut [u8]) -> Option<()> {
        if out.len() != self.byte_len {
            return None;
        }
        let b = limbs_from_be(base, self.len)?;
        if ge(&b[..self.len], &self.n[..self.len]) {
            return None;
        }
        let mut one = [0u64; MAX_LIMBS];
        one[0] = 1;
        let b = self.mul(&b, &self.rr);
        let mut acc = self.mul(&one, &self.rr); // R mod n: Montgomery 1
        for byte in exp {
            for i in (0..8).rev() {
                acc = self.mul(&acc, &acc);
                if byte >> i & 1 == 1 {
                    acc = self.mul(&acc, &b);
                }
            }
        }
        let acc = self.mul(&acc, &one);
        for (i, o) in out.iter_mut().rev().enumerate() {
            *o = (acc[i / 8] >> (8 * (i % 8))) as u8;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pow_u64(n: u64, b: u64, e: u64) -> u64 {
        let m = Modulus::new(&n.to_be_bytes()).unwrap();
        let mut out = std::vec![0u8; m.byte_len()];
        m.pow(&b.to_be_bytes(), &e.to_be_bytes(), &mut out).unwrap();
        let mut full = [0u8; 8];
        full[8 - out.len()..].copy_from_slice(&out);
        u64::from_be_bytes(full)
    }

    fn reference(n: u64, b: u64, mut e: u64) -> u64 {
        let (n, mut b, mut r) = (n as u128, b as u128 % n as u128, 1u128);
        while e > 0 {
            if e & 1 == 1 {
                r = r * b % n;
            }
            b = b * b % n;
            e >>= 1;
        }
        r as u64
    }

    #[test]
    fn small_moduli_match_reference() {
        for &(n, b, e) in &[
            (3u64, 2u64, 5u64),
            (0xffff_ffff_ffff_ffc5, 0x1234_5678_9abc_def0, 65537),
            (1_000_000_007, 123_456_789, 1_000_000_005),
            (0x8000_0000_0000_0001, 0x7fff_ffff_ffff_ffff, u64::MAX),
            (255, 254, 0),
        ] {
            assert_eq!(pow_u64(n, b, e), reference(n, b, e), "n={n} b={b} e={e}");
        }
    }

    #[test]
    fn multi_limb_fermat() {
        // n = 2^127 - 1 (prime), so b^(n-1) = 1 and b^n = b (Fermat).
        let n = ((1u128 << 127) - 1).to_be_bytes();
        let m = Modulus::new(&n).unwrap();
        assert_eq!((m.byte_len(), m.bits()), (16, 127));
        let b = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128.to_be_bytes();
        let mut out = [0u8; 16];
        m.pow(&b, &n, &mut out).unwrap();
        assert_eq!(out, b);
        let nm1 = ((1u128 << 127) - 2).to_be_bytes();
        m.pow(&b, &nm1, &mut out).unwrap();
        assert_eq!(u128::from_be_bytes(out), 1);
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!(Modulus::new(&[0, 0]).is_none());
        assert!(Modulus::new(&[0x10]).is_none()); // even
        assert!(Modulus::new(&[1]).is_none());
        assert!(Modulus::new(&[0xff; 513]).is_none()); // 4104 bits
        assert!(Modulus::new(&[0xff; 512]).is_some());
        let m = Modulus::new(&[0, 0x0b]).unwrap(); // leading zero ignored
        let mut out = [0u8; 1];
        assert!(m.pow(&[0x0b], &[3], &mut out).is_none()); // base == n
        assert!(m.pow(&[0x0a], &[3], &mut [0u8; 2]).is_none()); // wrong out length
        m.pow(&[0, 0x0a], &[3], &mut out).unwrap();
        assert_eq!(out, [1000u32.rem_euclid(11) as u8]);
    }

    extern crate std;
}
