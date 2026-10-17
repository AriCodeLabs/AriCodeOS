// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! X25519 (RFC 7748): Diffie-Hellman on Curve25519 with the Montgomery
//! ladder. The ladder swaps with [`Fe::cswap`] and never branches on the
//! scalar, so the ephemeral secret does not leak through timing.

use crate::field25519::Fe;

pub const KEY_LEN: usize = 32;

/// The base point u = 9.
pub const BASEPOINT: [u8; KEY_LEN] = {
    let mut b = [0u8; KEY_LEN];
    b[0] = 9;
    b
};

fn clamp(k: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let mut k = *k;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    k
}

/// `k * u`: the scalar is clamped and the top bit of `u` is ignored.
pub fn scalar_mult(k: &[u8; KEY_LEN], u: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let k = clamp(k);
    let x1 = Fe::from_bytes(u);
    let a24 = Fe::from_u64(121_665);
    let (mut x2, mut z2) = (Fe::ONE, Fe::ZERO);
    let (mut x3, mut z3) = (x1, Fe::ONE);
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let bit = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= bit;
        Fe::cswap(&mut x2, &mut x3, swap);
        Fe::cswap(&mut z2, &mut z3, swap);
        swap = bit;
        let a = x2.add(z2);
        let aa = a.square();
        let b = x2.sub(z2);
        let bb = b.square();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).square();
        z3 = x1.mul(da.sub(cb).square());
        x2 = aa.mul(bb);
        z2 = e.mul(aa.add(a24.mul(e)));
    }
    Fe::cswap(&mut x2, &mut x3, swap);
    Fe::cswap(&mut z2, &mut z3, swap);
    x2.mul(z2.invert()).to_bytes()
}

/// Public key for the secret `k`.
pub fn public_key(k: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    scalar_mult(k, &BASEPOINT)
}

/// Shared secret with the peer's public key; `None` if it is all zeros
/// (a low-order point, which RFC 7748 section 6.1 says to reject).
pub fn shared(k: &[u8; KEY_LEN], peer: &[u8; KEY_LEN]) -> Option<[u8; KEY_LEN]> {
    let s = scalar_mult(k, peer);
    (s != [0u8; KEY_LEN]).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn rfc7748_vectors() {
        // Section 5.2.
        assert_eq!(
            scalar_mult(
                &h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
                &h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c")
            ),
            h("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")
        );
        assert_eq!(
            scalar_mult(
                &h("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
                &h("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493")
            ),
            h("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957")
        );
    }

    #[test]
    fn rfc7748_iterations() {
        // Section 5.2: k = u = 9, then k <- k*u, u <- old k.
        let (mut k, mut u) = (BASEPOINT, BASEPOINT);
        let step = |k: &mut [u8; 32], u: &mut [u8; 32]| {
            let r = scalar_mult(k, u);
            *u = *k;
            *k = r;
        };
        step(&mut k, &mut u);
        assert_eq!(
            k,
            h("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")
        );
        for _ in 1..1000 {
            step(&mut k, &mut u);
        }
        assert_eq!(
            k,
            h("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51")
        );
    }

    #[test]
    fn rfc7748_diffie_hellman() {
        // Section 6.1.
        let a = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b = h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let pa = public_key(&a);
        let pb = public_key(&b);
        assert_eq!(
            pa,
            h("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        assert_eq!(
            pb,
            h("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        );
        let k = h("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(shared(&a, &pb), Some(k));
        assert_eq!(shared(&b, &pa), Some(k));
        assert_eq!(shared(&a, &[0u8; 32]), None, "low-order point rejected");
    }
}
