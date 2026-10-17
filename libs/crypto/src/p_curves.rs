// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Arithmetic for the NIST prime curves P-256 and P-384 (FIPS 186-4,
//! SEC 2): integers modulo an odd prime in Montgomery form with `N`
//! 64-bit limbs, and points in Jacobian coordinates on
//! y^2 = x^3 - 3x + b. Used by [`crate::ecdsa`] to verify signatures, so
//! nothing here is constant time (all inputs are public).

/// Little-endian 64-bit limbs.
pub type Limbs<const N: usize> = [u64; N];

/// Parses a big-endian hexadecimal constant (exactly `16 * N` digits).
const fn limbs<const N: usize>(hex: &str) -> Limbs<N> {
    let h = hex.as_bytes();
    assert!(h.len() == 16 * N);
    let mut out = [0u64; N];
    let mut i = 0;
    while i < h.len() {
        let c = h[i];
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("bad hex digit"),
        } as u64;
        let limb = (h.len() - 1 - i) / 16;
        out[limb] = (out[limb] << 4) | d;
        i += 1;
    }
    out
}

/// Big-endian bytes (any length) to limbs; `None` if the value does not
/// fit in `N` limbs.
pub fn from_be_bytes<const N: usize>(bytes: &[u8]) -> Option<Limbs<N>> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let bytes = &bytes[start..];
    if bytes.len() > 8 * N {
        return None;
    }
    let mut out = [0u64; N];
    for (i, &b) in bytes.iter().rev().enumerate() {
        out[i / 8] |= (b as u64) << (8 * (i % 8));
    }
    Some(out)
}

fn is_zero<const N: usize>(a: &Limbs<N>) -> bool {
    a.iter().all(|&l| l == 0)
}

/// `a >= b`.
pub fn geq<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> bool {
    for i in (0..N).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// `a - b` in place, returns the borrow.
fn sub_in_place<const N: usize>(a: &mut Limbs<N>, b: &Limbs<N>) -> bool {
    let mut borrow = false;
    for i in 0..N {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow as u64);
        a[i] = d2;
        borrow = b1 | b2;
    }
    borrow
}

/// `a + b` in place, returns the carry.
fn add_in_place<const N: usize>(a: &mut Limbs<N>, b: &Limbs<N>) -> bool {
    let mut carry = false;
    for i in 0..N {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry as u64);
        a[i] = s2;
        carry = c1 | c2;
    }
    carry
}

/// Arithmetic modulo an odd `m > 2` with Montgomery multiplication
/// (R = 2^(64 N)). Values passed to the `*_mod` methods are below `m`.
pub struct Modulus<const N: usize> {
    m: Limbs<N>,
    /// -m^-1 mod 2^64.
    m0inv: u64,
    /// R^2 mod m.
    r2: Limbs<N>,
}

impl<const N: usize> Modulus<N> {
    pub fn new(m: Limbs<N>) -> Self {
        // Newton iteration for m^-1 mod 2^64 (each step doubles the bits).
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m[0].wrapping_mul(inv)));
        }
        let mut md = Modulus {
            m,
            m0inv: inv.wrapping_neg(),
            r2: [0; N],
        };
        let mut x = [0u64; N];
        x[0] = 1;
        for _ in 0..128 * N {
            x = md.add(&x, &x);
        }
        md.r2 = x;
        md
    }

    pub fn modulus(&self) -> &Limbs<N> {
        &self.m
    }

    /// `a mod m` for `a < 2m`.
    pub fn reduce(&self, a: &Limbs<N>) -> Limbs<N> {
        let mut r = *a;
        if geq(&r, &self.m) {
            sub_in_place(&mut r, &self.m);
        }
        r
    }

    pub fn add(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let mut r = *a;
        let carry = add_in_place(&mut r, b);
        if carry || geq(&r, &self.m) {
            sub_in_place(&mut r, &self.m);
        }
        r
    }

    pub fn sub(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let mut r = *a;
        if sub_in_place(&mut r, b) {
            add_in_place(&mut r, &self.m);
        }
        r
    }

    /// a * b / R mod m (CIOS).
    pub fn mul(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let mut t = [0u64; N];
        let mut top = 0u64;
        for &bi in b.iter() {
            let mut carry = 0u64;
            for j in 0..N {
                let v = t[j] as u128 + a[j] as u128 * bi as u128 + carry as u128;
                t[j] = v as u64;
                carry = (v >> 64) as u64;
            }
            let v = top as u128 + carry as u128;
            top = v as u64;
            let top2 = (v >> 64) as u64;

            let q = t[0].wrapping_mul(self.m0inv);
            let v = t[0] as u128 + q as u128 * self.m[0] as u128;
            let mut carry = (v >> 64) as u64;
            for j in 1..N {
                let v = t[j] as u128 + q as u128 * self.m[j] as u128 + carry as u128;
                t[j - 1] = v as u64;
                carry = (v >> 64) as u64;
            }
            let v = top as u128 + carry as u128;
            t[N - 1] = v as u64;
            top = top2 + (v >> 64) as u64;
        }
        if top != 0 || geq(&t, &self.m) {
            sub_in_place(&mut t, &self.m);
        }
        t
    }

    pub fn to_mont(&self, a: &Limbs<N>) -> Limbs<N> {
        self.mul(a, &self.r2)
    }

    pub fn to_normal(&self, a: &Limbs<N>) -> Limbs<N> {
        let mut one = [0u64; N];
        one[0] = 1;
        self.mul(a, &one)
    }

    /// Inverse of a Montgomery-form value (Fermat: a^(m-2), m prime).
    pub fn inv(&self, a: &Limbs<N>) -> Limbs<N> {
        let mut e = self.m;
        let mut two = [0u64; N];
        two[0] = 2;
        sub_in_place(&mut e, &two);
        let mut one = [0u64; N];
        one[0] = 1;
        let mut r = self.to_mont(&one);
        for i in (0..64 * N).rev() {
            r = self.mul(&r, &r);
            if (e[i / 64] >> (i % 64)) & 1 == 1 {
                r = self.mul(&r, a);
            }
        }
        r
    }
}

/// Domain parameters of a short Weierstrass curve with a = -3.
pub struct CurveParams<const N: usize> {
    pub p: Limbs<N>,
    pub n: Limbs<N>,
    pub b: Limbs<N>,
    pub gx: Limbs<N>,
    pub gy: Limbs<N>,
}

pub const P256: CurveParams<4> = CurveParams {
    p: limbs("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff"),
    n: limbs("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"),
    b: limbs("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b"),
    gx: limbs("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"),
    gy: limbs("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"),
};

pub const P384: CurveParams<6> = CurveParams {
    p: limbs(
        "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe\
         ffffffff0000000000000000ffffffff",
    ),
    n: limbs(
        "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf\
         581a0db248b0a77aecec196accc52973",
    ),
    b: limbs(
        "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875a\
         c656398d8a2ed19d2a85c8edd3ec2aef",
    ),
    gx: limbs(
        "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a38\
         5502f25dbf55296c3a545e3872760ab7",
    ),
    gy: limbs(
        "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c0\
         0a60b1ce1d7e819d7a431d7c90ea0e5f",
    ),
};

/// Point in Jacobian coordinates (x = X/Z^2, y = Y/Z^3), coordinates in
/// Montgomery form; Z = 0 is the point at infinity.
#[derive(Clone, Copy)]
pub struct Point<const N: usize> {
    pub x: Limbs<N>,
    pub y: Limbs<N>,
    pub z: Limbs<N>,
}

/// The field of a curve plus its `b` coefficient in Montgomery form.
pub struct Curve<const N: usize> {
    pub fp: Modulus<N>,
    b: Limbs<N>,
    one: Limbs<N>,
}

impl<const N: usize> Curve<N> {
    pub fn new(params: &CurveParams<N>) -> Self {
        let fp = Modulus::new(params.p);
        let b = fp.to_mont(&params.b);
        let mut one = [0u64; N];
        one[0] = 1;
        let one = fp.to_mont(&one);
        Curve { fp, b, one }
    }

    /// Affine point from normal-form coordinates; `None` unless both are
    /// below p and satisfy the curve equation.
    pub fn affine(&self, x: &Limbs<N>, y: &Limbs<N>) -> Option<Point<N>> {
        let p = self.fp.modulus();
        if geq(x, p) || geq(y, p) {
            return None;
        }
        let f = &self.fp;
        let (x, y) = (f.to_mont(x), f.to_mont(y));
        let lhs = f.mul(&y, &y);
        let x3 = f.mul(&f.mul(&x, &x), &x);
        let three_x = f.add(&f.add(&x, &x), &x);
        let rhs = f.add(&f.sub(&x3, &three_x), &self.b);
        (lhs == rhs).then_some(Point { x, y, z: self.one })
    }

    pub fn double(&self, p: &Point<N>) -> Point<N> {
        // dbl-2001-b (a = -3); Z = 0 or Y = 0 yields Z3 = 0.
        let f = &self.fp;
        let delta = f.mul(&p.z, &p.z);
        let gamma = f.mul(&p.y, &p.y);
        let beta = f.mul(&p.x, &gamma);
        let t = f.mul(&f.sub(&p.x, &delta), &f.add(&p.x, &delta));
        let alpha = f.add(&f.add(&t, &t), &t);
        let beta2 = f.add(&beta, &beta);
        let beta4 = f.add(&beta2, &beta2);
        let beta8 = f.add(&beta4, &beta4);
        let x3 = f.sub(&f.mul(&alpha, &alpha), &beta8);
        let yz = f.add(&p.y, &p.z);
        let z3 = f.sub(&f.sub(&f.mul(&yz, &yz), &gamma), &delta);
        let g2 = f.mul(&gamma, &gamma);
        let g2_2 = f.add(&g2, &g2);
        let g2_4 = f.add(&g2_2, &g2_2);
        let g2_8 = f.add(&g2_4, &g2_4);
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &g2_8);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    pub fn add(&self, p: &Point<N>, q: &Point<N>) -> Point<N> {
        if is_zero(&p.z) {
            return *q;
        }
        if is_zero(&q.z) {
            return *p;
        }
        // add-2007-bl.
        let f = &self.fp;
        let z1z1 = f.mul(&p.z, &p.z);
        let z2z2 = f.mul(&q.z, &q.z);
        let u1 = f.mul(&p.x, &z2z2);
        let u2 = f.mul(&q.x, &z1z1);
        let s1 = f.mul(&f.mul(&p.y, &q.z), &z2z2);
        let s2 = f.mul(&f.mul(&q.y, &p.z), &z1z1);
        let h = f.sub(&u2, &u1);
        let r_half = f.sub(&s2, &s1);
        if is_zero(&h) {
            if is_zero(&r_half) {
                return self.double(p);
            }
            return Point {
                x: self.one,
                y: self.one,
                z: [0; N],
            };
        }
        let h2 = f.add(&h, &h);
        let i = f.mul(&h2, &h2);
        let j = f.mul(&h, &i);
        let r = f.add(&r_half, &r_half);
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.mul(&r, &r), &j), &f.add(&v, &v));
        let s1j = f.mul(&s1, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
        let zz = f.add(&p.z, &q.z);
        let z3 = f.mul(&f.sub(&f.sub(&f.mul(&zz, &zz), &z1z1), &z2z2), &h);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// `a * p + b * q` (Shamir's trick) for normal-form scalars.
    pub fn double_mul(&self, a: &Limbs<N>, p: &Point<N>, b: &Limbs<N>, q: &Point<N>) -> Point<N> {
        let pq = self.add(p, q);
        let mut acc = Point {
            x: self.one,
            y: self.one,
            z: [0; N],
        };
        for i in (0..64 * N).rev() {
            acc = self.double(&acc);
            let bit_a = (a[i / 64] >> (i % 64)) & 1 == 1;
            let bit_b = (b[i / 64] >> (i % 64)) & 1 == 1;
            acc = match (bit_a, bit_b) {
                (true, true) => self.add(&acc, &pq),
                (true, false) => self.add(&acc, p),
                (false, true) => self.add(&acc, q),
                (false, false) => acc,
            };
        }
        acc
    }

    /// Normal-form affine x coordinate; `None` for the point at infinity.
    pub fn affine_x(&self, p: &Point<N>) -> Option<Limbs<N>> {
        if is_zero(&p.z) {
            return None;
        }
        let f = &self.fp;
        let zi = f.inv(&p.z);
        Some(f.to_normal(&f.mul(&p.x, &f.mul(&zi, &zi))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modular_arithmetic_round_trips() {
        let f = Modulus::new(P256.n);
        let a = limbs::<4>("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let am = f.to_mont(&a);
        assert_eq!(f.to_normal(&am), a);
        let one = f.to_mont(&[1, 0, 0, 0]);
        assert_eq!(f.mul(&am, &f.inv(&am)), one);
        assert_eq!(f.add(&f.sub(&a, &P256.gx), &P256.gx), a);
        assert_eq!(from_be_bytes::<4>(&[0, 0, 1, 2]), Some([0x102, 0, 0, 0]));
        assert_eq!(from_be_bytes::<1>(&[1; 9]), None);
    }

    #[test]
    fn generators_are_on_their_curves_and_have_order_n() {
        fn check<const N: usize>(params: &CurveParams<N>) {
            let c = Curve::new(params);
            let g = c.affine(&params.gx, &params.gy).expect("G on curve");
            let mut n_minus_1 = params.n;
            n_minus_1[0] -= 1;
            // (n - 1) G = -G has the same x; n G = infinity.
            let zero = [0u64; N];
            let neg = c.double_mul(&n_minus_1, &g, &zero, &g);
            assert_eq!(c.affine_x(&neg), Some(params.gx));
            assert!(c.affine_x(&c.add(&neg, &g)).is_none());
            let mut bad_y = params.gy;
            bad_y[0] ^= 1;
            assert!(c.affine(&params.gx, &bad_y).is_none());
        }
        check(&P256);
        check(&P384);
    }
}
