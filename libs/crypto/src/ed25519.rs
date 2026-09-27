//! Ed25519 (RFC 8032) desde cero: aritmética en GF(2^255-19) con cinco limbs
//! de 51 bits, puntos en coordenadas extendidas, escalares módulo L por
//! división larga. Pensado para verificar firmas de manifiestos en el
//! arranque; firmar es tarea del host. No es de tiempo constante.

use crate::sha512::Sha512;

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SEED_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

// ---------------------------------------------------------------- campo

/// Elemento de GF(p), p = 2^255 - 19, en cinco limbs de 51 bits.
#[derive(Clone, Copy, Debug)]
struct Fe([u64; 5]);

const MASK51: u64 = (1 << 51) - 1;
/// p en limbs de 51 bits.
const P: [u64; 5] = [
    0x7FFFFFFFFFFED,
    0x7FFFFFFFFFFFF,
    0x7FFFFFFFFFFFF,
    0x7FFFFFFFFFFFF,
    0x7FFFFFFFFFFFF,
];

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn from_u64(v: u64) -> Fe {
        Fe([v & MASK51, v >> 51, 0, 0, 0])
    }

    /// Carga 255 bits little-endian (el bit 255 se ignora).
    fn from_bytes(b: &[u8; 32]) -> Fe {
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

    /// Propaga acarreos: deja cada limb por debajo de 2^51 (+ pequeño exceso).
    fn carry(mut self) -> Fe {
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

    /// Representación canónica en [0, p).
    fn to_bytes(self) -> [u8; 32] {
        let mut h = self.carry().carry();
        // Restar p si h >= p.
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
        // bits 128.. : resto del limb 2 (25 bits), limbs 3 y 4.
        let upper: u128 = (bits(2) >> 26) | bits(3) << 25 | bits(4) << 76;
        out[16..].copy_from_slice(&upper.to_le_bytes());
        out
    }

    fn add(self, o: Fe) -> Fe {
        Fe([
            self.0[0] + o.0[0],
            self.0[1] + o.0[1],
            self.0[2] + o.0[2],
            self.0[3] + o.0[3],
            self.0[4] + o.0[4],
        ])
        .carry()
    }

    fn sub(self, o: Fe) -> Fe {
        // self + 2p - o evita negativos (los limbs de self están reducidos).
        Fe([
            self.0[0] + 2 * P[0] - o.0[0],
            self.0[1] + 2 * P[1] - o.0[1],
            self.0[2] + 2 * P[2] - o.0[2],
            self.0[3] + 2 * P[3] - o.0[3],
            self.0[4] + 2 * P[4] - o.0[4],
        ])
        .carry()
    }

    fn neg(self) -> Fe {
        Fe::ZERO.sub(self)
    }

    fn mul(self, o: Fe) -> Fe {
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

    fn square(self) -> Fe {
        self.mul(self)
    }

    /// self^exp, exponente little-endian.
    fn pow(self, exp: &[u8; 32]) -> Fe {
        let mut result = Fe::ONE;
        for i in (0..256).rev() {
            result = result.square();
            if (exp[i / 8] >> (i % 8)) & 1 == 1 {
                result = result.mul(self);
            }
        }
        result
    }

    fn invert(self) -> Fe {
        // p - 2 = 2^255 - 21
        let mut e = [0xFFu8; 32];
        e[0] = 0xEB;
        e[31] = 0x7F;
        self.pow(&e)
    }

    /// self^((p-5)/8), (p-5)/8 = 2^252 - 3.
    fn pow22523(self) -> Fe {
        let mut e = [0xFFu8; 32];
        e[0] = 0xFD;
        e[31] = 0x0F;
        self.pow(&e)
    }

    fn is_zero(self) -> bool {
        self.to_bytes() == [0u8; 32]
    }

    fn is_negative(self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }

    fn eq(self, o: Fe) -> bool {
        self.to_bytes() == o.to_bytes()
    }
}

/// sqrt(-1) = 2^((p-1)/4).
fn sqrt_m1() -> Fe {
    let mut e = [0xFFu8; 32];
    e[0] = 0xFB;
    e[31] = 0x1F;
    Fe::from_u64(2).pow(&e)
}

/// d = -121665 / 121666.
fn curve_d() -> Fe {
    Fe::from_u64(121665)
        .neg()
        .mul(Fe::from_u64(121666).invert())
}

// ---------------------------------------------------------------- puntos

/// Punto en coordenadas extendidas (X, Y, Z, T), x = X/Z, y = Y/Z, T = XY/Z.
#[derive(Clone, Copy, Debug)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

struct Curve {
    d: Fe,
    d2: Fe,
    sqrt_m1: Fe,
}

impl Curve {
    fn new() -> Self {
        let d = curve_d();
        Self {
            d,
            d2: d.add(d),
            sqrt_m1: sqrt_m1(),
        }
    }

    fn identity(&self) -> Point {
        Point {
            x: Fe::ZERO,
            y: Fe::ONE,
            z: Fe::ONE,
            t: Fe::ZERO,
        }
    }

    fn add(&self, p: &Point, q: &Point) -> Point {
        let a = p.y.sub(p.x).mul(q.y.sub(q.x));
        let b = p.y.add(p.x).mul(q.y.add(q.x));
        let c = p.t.mul(self.d2).mul(q.t);
        let d = p.z.mul(q.z);
        let d = d.add(d);
        let e = b.sub(a);
        let f = d.sub(c);
        let g = d.add(c);
        let h = b.add(a);
        Point {
            x: e.mul(f),
            y: g.mul(h),
            z: f.mul(g),
            t: e.mul(h),
        }
    }

    fn double(&self, p: &Point) -> Point {
        let a = p.x.square();
        let b = p.y.square();
        let c = p.z.square();
        let c = c.add(c);
        let h = a.add(b);
        let e = h.sub(p.x.add(p.y).square());
        let g = a.sub(b);
        let f = c.add(g);
        Point {
            x: e.mul(f),
            y: g.mul(h),
            z: f.mul(g),
            t: e.mul(h),
        }
    }

    /// Multiplicación escalar (doblado y suma, de tiempo variable).
    fn mul(&self, scalar: &[u8; 32], p: &Point) -> Point {
        let mut r = self.identity();
        for i in (0..256).rev() {
            r = self.double(&r);
            if (scalar[i / 8] >> (i % 8)) & 1 == 1 {
                r = self.add(&r, p);
            }
        }
        r
    }

    fn compress(&self, p: &Point) -> [u8; 32] {
        let zi = p.z.invert();
        let x = p.x.mul(zi);
        let y = p.y.mul(zi);
        let mut out = y.to_bytes();
        out[31] |= (x.is_negative() as u8) << 7;
        out
    }

    fn decompress(&self, b: &[u8; 32]) -> Option<Point> {
        let sign = b[31] >> 7;
        let mut yb = *b;
        yb[31] &= 0x7F;
        let y = Fe::from_bytes(&yb);
        // x^2 = (y^2 - 1) / (d y^2 + 1)
        let y2 = y.square();
        let u = y2.sub(Fe::ONE);
        let v = self.d.mul(y2).add(Fe::ONE);
        // x = u v^3 (u v^7)^((p-5)/8)
        let v3 = v.square().mul(v);
        let v7 = v3.square().mul(v);
        let mut x = u.mul(v3).mul(u.mul(v7).pow22523());
        let vx2 = v.mul(x.square());
        if !vx2.eq(u) {
            if vx2.eq(u.neg()) {
                x = x.mul(self.sqrt_m1);
            } else {
                return None;
            }
        }
        if x.is_zero() && sign == 1 {
            return None;
        }
        if x.is_negative() as u8 != sign {
            x = x.neg();
        }
        Some(Point {
            x,
            y,
            z: Fe::ONE,
            t: x.mul(y),
        })
    }

    fn base(&self) -> Point {
        let mut b = [0x66u8; 32];
        b[0] = 0x58;
        self.decompress(&b).expect("punto base")
    }
}

// ---------------------------------------------------------------- escalares mod L

/// L = 2^252 + 27742317777372353535851937790883648493, en limbs u64 LE.
const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0,
    0x1000000000000000,
];

fn ge(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn sub_in_place(a: &mut [u64; 4], b: &[u64; 4]) {
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d, b1) = a[i].overflowing_sub(b[i]);
        let (d, b2) = d.overflowing_sub(borrow);
        a[i] = d;
        borrow = (b1 || b2) as u64;
    }
}

/// x mod L para x de hasta 512 bits (limbs LE), por división larga binaria.
fn reduce_limbs(x: &[u64; 8]) -> [u64; 4] {
    let mut rem = [0u64; 4];
    for i in (0..512).rev() {
        let bit = (x[i / 64] >> (i % 64)) & 1;
        // rem = rem * 2 + bit (cabe: rem < L < 2^253)
        let mut carry = bit;
        for limb in rem.iter_mut() {
            let top = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = top;
        }
        if ge(&rem, &L) {
            sub_in_place(&mut rem, &L);
        }
    }
    rem
}

fn limbs_from_bytes(b: &[u8]) -> [u64; 8] {
    let mut out = [0u64; 8];
    for (i, chunk) in b.chunks(8).enumerate().take(8) {
        let mut v = [0u8; 8];
        v[..chunk.len()].copy_from_slice(chunk);
        out[i] = u64::from_le_bytes(v);
    }
    out
}

fn bytes_from_limbs(l: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, limb) in l.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&limb.to_le_bytes());
    }
    out
}

/// Reduce un hash de 64 bytes módulo L.
fn reduce(h: &[u8; 64]) -> [u8; 32] {
    bytes_from_limbs(&reduce_limbs(&limbs_from_bytes(h)))
}

/// (a * b + c) mod L.
fn mul_add(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let (a, b, c) = (
        limbs_from_bytes(a),
        limbs_from_bytes(b),
        limbs_from_bytes(c),
    );
    let mut prod = [0u128; 9];
    for i in 0..4 {
        for j in 0..4 {
            prod[i + j] += a[i] as u128 * b[j] as u128;
        }
    }
    for i in 0..4 {
        prod[i] += c[i] as u128;
    }
    let mut limbs = [0u64; 8];
    let mut carry = 0u128;
    for i in 0..8 {
        let v = prod[i] + carry;
        limbs[i] = v as u64;
        carry = v >> 64;
    }
    bytes_from_limbs(&reduce_limbs(&limbs))
}

/// `s` es un escalar canónico (< L).
fn is_canonical(s: &[u8; 32]) -> bool {
    let l = limbs_from_bytes(s);
    !ge(&[l[0], l[1], l[2], l[3]], &L)
}

fn clamp(a: &mut [u8; 32]) {
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
}

// ---------------------------------------------------------------- API

/// Clave pública derivada de la semilla de 32 bytes.
pub fn public_key(seed: &[u8; SEED_LEN]) -> [u8; PUBLIC_KEY_LEN] {
    let h = crate::sha512::sha512(seed);
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    clamp(&mut a);
    let c = Curve::new();
    c.compress(&c.mul(&a, &c.base()))
}

/// Firma `msg` con la semilla (RFC 8032 §5.1.6).
pub fn sign(seed: &[u8; SEED_LEN], msg: &[u8]) -> [u8; SIGNATURE_LEN] {
    let h = crate::sha512::sha512(seed);
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    clamp(&mut a);
    let prefix = &h[32..];
    let c = Curve::new();
    let base = c.base();
    let pk = c.compress(&c.mul(&a, &base));

    let mut hasher = Sha512::new();
    hasher.update(prefix);
    hasher.update(msg);
    let r = reduce(&hasher.finish());
    let rb = c.compress(&c.mul(&r, &base));

    let mut hasher = Sha512::new();
    hasher.update(&rb);
    hasher.update(&pk);
    hasher.update(msg);
    let k = reduce(&hasher.finish());
    let s = mul_add(&k, &a, &r);

    let mut sig = [0u8; SIGNATURE_LEN];
    sig[..32].copy_from_slice(&rb);
    sig[32..].copy_from_slice(&s);
    sig
}

/// Verifica `sig` sobre `msg` con la clave pública `pk` (RFC 8032 §5.1.7).
pub fn verify(pk: &[u8; PUBLIC_KEY_LEN], msg: &[u8], sig: &[u8; SIGNATURE_LEN]) -> bool {
    let mut rb = [0u8; 32];
    let mut s = [0u8; 32];
    rb.copy_from_slice(&sig[..32]);
    s.copy_from_slice(&sig[32..]);
    if !is_canonical(&s) {
        return false;
    }
    let c = Curve::new();
    let (Some(a), Some(r)) = (c.decompress(pk), c.decompress(&rb)) else {
        return false;
    };
    let mut hasher = Sha512::new();
    hasher.update(&rb);
    hasher.update(pk);
    hasher.update(msg);
    let k = reduce(&hasher.finish());
    let sb = c.mul(&s, &c.base());
    let rka = c.add(&r, &c.mul(&k, &a));
    crate::ct_eq(&c.compress(&sb), &c.compress(&rka))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unhex;

    fn bytes<const N: usize>(h: &str) -> [u8; N] {
        let mut out = [0u8; N];
        unhex(h, &mut out).expect("hex");
        out
    }

    #[test]
    fn field_round_trip_and_inverse() {
        let one = Fe::ONE;
        assert_eq!(one.to_bytes()[0], 1);
        let v = Fe::from_bytes(&bytes::<32>(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd0f",
        ));
        assert!(v.mul(v.invert()).eq(one));
        assert!(v.sub(v).is_zero());
        assert!(v.add(v.neg()).is_zero());
        let bytes_back = Fe::from_bytes(&v.to_bytes());
        assert!(bytes_back.eq(v));
        // p mismo debe reducirse a 0.
        let mut p = [0xFFu8; 32];
        p[0] = 0xED;
        p[31] = 0x7F;
        assert!(Fe::from_bytes(&p).is_zero());
        let i = sqrt_m1();
        assert!(i.square().eq(Fe::ONE.neg()));
    }

    #[test]
    fn scalar_reduction() {
        // L mod L = 0 ; (L + 5) mod L = 5
        let mut l = [0u8; 64];
        l[..32].copy_from_slice(&bytes_from_limbs(&L));
        assert_eq!(reduce(&l), [0u8; 32]);
        l[0] = l[0].wrapping_add(5);
        let mut five = [0u8; 32];
        five[0] = 5;
        assert_eq!(reduce(&l), five);
        // 2 * 3 + 4 = 10
        let (mut two, mut three, mut four, mut ten) = ([0u8; 32], [0u8; 32], [0u8; 32], [0u8; 32]);
        two[0] = 2;
        three[0] = 3;
        four[0] = 4;
        ten[0] = 10;
        assert_eq!(mul_add(&two, &three, &four), ten);
        assert!(is_canonical(&ten));
        assert!(!is_canonical(&bytes_from_limbs(&L)));
    }

    #[test]
    fn rfc8032_test_vectors() {
        let cases: [(&str, &str, &str, &str); 3] = [
            (
                "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
                "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
                "",
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            ),
            (
                "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
                "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
                "72",
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            ),
            (
                "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
                "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
                "af82",
                "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
            ),
        ];
        for (seed, pk, msg, sig) in cases {
            let seed = bytes::<32>(seed);
            let pk = bytes::<32>(pk);
            let sig = bytes::<64>(sig);
            let mut m = [0u8; 2];
            let msg_len = msg.len() / 2;
            unhex(msg, &mut m[..msg_len]).unwrap();
            let m = &m[..msg_len];
            assert_eq!(public_key(&seed), pk, "clave publica");
            assert_eq!(sign(&seed, m), sig, "firma");
            assert!(verify(&pk, m, &sig), "verificacion");
            // Mensaje alterado, firma alterada y clave ajena fallan.
            let mut bad_msg = [0u8; 3];
            bad_msg[..msg_len].copy_from_slice(m);
            bad_msg[msg_len] = 1;
            assert!(!verify(&pk, &bad_msg[..msg_len + 1], &sig));
            let mut bad_sig = sig;
            bad_sig[10] ^= 1;
            assert!(!verify(&pk, m, &bad_sig));
            let mut bad_sig = sig;
            bad_sig[40] ^= 1;
            assert!(!verify(&pk, m, &bad_sig));
            let other =
                bytes::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
            if other != pk {
                assert!(!verify(&other, m, &sig));
            }
        }
    }

    #[test]
    fn long_message_round_trip() {
        let seed = [42u8; 32];
        let pk = public_key(&seed);
        let msg: [u8; 1000] = core::array::from_fn(|i| (i % 253) as u8);
        let sig = sign(&seed, &msg);
        assert!(verify(&pk, &msg, &sig));
        // Firma no canonica (S >= L) se rechaza.
        let mut noncanon = sig;
        noncanon[32..].copy_from_slice(&bytes_from_limbs(&L));
        assert!(!verify(&pk, &msg, &noncanon));
        // Clave publica no decodificable.
        let mut bad_pk = pk;
        bad_pk[31] = 0xFF;
        let _ = verify(&bad_pk, &msg, &sig); // no debe entrar en panico
    }
}
