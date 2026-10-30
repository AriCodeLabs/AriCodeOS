// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Ed25519 (RFC 8032) over `ed25519-dalek` (ADR 0066), with precomputed
//! tables; signing and the public key from a seed run in constant time.
//! The signing key wipes itself when dropped, and the stack those leave
//! behind (the expanded key, the nonce, SHA-512 states) is scrubbed
//! before returning ([`crate::scrub_stack`]).
//!
//! Verification is `verify_strict` plus one check of ours, so it refuses
//! (incident 0044):
//! - a point encoding whose y is not below p (RFC 8032 section 5.1.3), for
//!   the public key A as well: `ed25519-dalek` accepts such an A when the
//!   point has large order;
//! - a public key A or a commitment R of small order (\[8\]P is the
//!   identity): with such an A, \[S\]B = R + \[k\]A holds for R =
//!   identity and S = 0 whenever the order of A divides k, for one message
//!   in eight or better, without any private key;
//! - S not below the group order L (a malleable signature).
//!
//! The equation is the cofactorless one; a key with a small-order component
//! added to a point of order L is not refused (it only verifies what its
//! holder signs).

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SEED_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

/// Public key of `seed`.
pub fn public_key(seed: &[u8; SEED_LEN]) -> [u8; PUBLIC_KEY_LEN] {
    crate::scrubbed::<{ crate::SCRUB_CURVE }, _>(|| {
        SigningKey::from_bytes(seed).verifying_key().to_bytes()
    })
}

/// Signature of `msg` with `seed` (deterministic, RFC 8032 section 5.1.6).
pub fn sign(seed: &[u8; SEED_LEN], msg: &[u8]) -> [u8; SIGNATURE_LEN] {
    // `ed25519-dalek` leaves the nonce r and the secret scalar on the
    // stack; with r and the signature, anyone gets the private scalar.
    crate::scrubbed::<{ crate::SCRUB_CURVE }, _>(|| {
        SigningKey::from_bytes(seed).sign(msg).to_bytes()
    })
}

/// Verifies `sig` over `msg` with the public key `pk` (RFC 8032 §5.1.7).
/// Non-canonical encodings, a small-order A or R and a non-canonical S are
/// refused (see the module documentation).
pub fn verify(pk: &[u8; PUBLIC_KEY_LEN], msg: &[u8], sig: &[u8; SIGNATURE_LEN]) -> bool {
    if !canonical_y(pk) {
        return false;
    }
    let Ok(key) = VerifyingKey::from_bytes(pk) else {
        return false;
    };
    key.verify_strict(msg, &Signature::from_bytes(sig)).is_ok()
}

/// Whether the y coordinate of a point encoding (its low 255 bits, little
/// endian) is below p = 2^255 - 19. R needs no such check: `verify_strict`
/// compares the canonical encoding it computes with R's bytes.
fn canonical_y(enc: &[u8; 32]) -> bool {
    // p in little endian: ed ff .. ff 7f.
    let top = enc[31] & 0x7F;
    if top != 0x7F {
        return true;
    }
    if enc[1..31].iter().any(|&b| b != 0xFF) {
        return true;
    }
    enc[0] < 0xED
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

    /// The group order L, little endian.
    const L: &str = "edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010";

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
            assert_eq!(public_key(&seed), pk, "public key");
            assert_eq!(sign(&seed, m), sig, "signature");
            assert!(verify(&pk, m, &sig), "verification");
            // Altered message, altered signature and foreign key fail.
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
    fn varied_seeds_round_trip() {
        for seed in [[9u8; 32], [0xFFu8; 32], [0x80u8; 32], [0x5Au8; 32]] {
            let pk = public_key(&seed);
            let sig = sign(&seed, b"limbs grandes");
            assert!(verify(&pk, b"limbs grandes", &sig), "seed {:?}", seed[0]);
        }
    }

    #[test]
    fn long_message_round_trip() {
        let seed = [42u8; 32];
        let pk = public_key(&seed);
        let msg: [u8; 1000] = core::array::from_fn(|i| (i % 253) as u8);
        let sig = sign(&seed, &msg);
        assert!(verify(&pk, &msg, &sig));
        // Non-canonical signature (S >= L) is rejected.
        let mut noncanon = sig;
        noncanon[32..].copy_from_slice(&bytes::<32>(L));
        assert!(!verify(&pk, &msg, &noncanon));
        // Undecodable public key.
        let mut bad_pk = pk;
        bad_pk[31] = 0xFF;
        let _ = verify(&bad_pk, &msg, &sig); // must not panic
    }

    /// The eight points of order dividing 8, canonically encoded, in the
    /// order of "Taming the many EdDSAs" (Chalkias et al., 2020, table 6)
    /// and ed25519-zebra's test vectors: the identity, the point of order
    /// 2, the two of order 4 and the four of order 8.
    const SMALL_ORDER: [&str; 8] = [
        "0100000000000000000000000000000000000000000000000000000000000000",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000080",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    ];

    /// Encodings with y >= p that the decoder used to reduce: y = p (the
    /// order-4 points, both signs) and y = p + 1 (the identity).
    const NON_CANONICAL: [&str; 3] = [
        "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    ];

    #[test]
    fn small_order_points_are_weak_keys() {
        for enc in SMALL_ORDER {
            let key = VerifyingKey::from_bytes(&bytes::<32>(enc)).expect("on the curve");
            assert!(key.is_weak(), "{enc}");
        }
        let key = VerifyingKey::from_bytes(&public_key(&[42; 32])).unwrap();
        assert!(!key.is_weak());
    }

    /// Before incident 0044 a small-order A with R = identity (or another
    /// small-order R) and S = 0 verified 1160 of these 12 544 (A, R,
    /// message) forgeries, among them every message under A = identity.
    #[test]
    fn small_order_and_non_canonical_keys_and_commitments_are_refused() {
        let points = SMALL_ORDER.iter().chain(&NON_CANONICAL);
        for a in points.clone() {
            for r in points.clone() {
                let mut sig = [0u8; 64];
                sig[..32].copy_from_slice(&bytes::<32>(r));
                for m in 0..64u8 {
                    assert!(!verify(&bytes::<32>(a), &[m], &sig), "A {a} R {r}");
                }
            }
        }
        for enc in NON_CANONICAL {
            assert!(!canonical_y(&bytes::<32>(enc)), "{enc}");
        }
        // An honest key with a small-order R in place of its own.
        let seed = [42u8; 32];
        let pk = public_key(&seed);
        let sig = sign(&seed, b"m");
        assert!(verify(&pk, b"m", &sig));
        for r in SMALL_ORDER {
            let mut bad = sig;
            bad[..32].copy_from_slice(&bytes::<32>(r));
            assert!(!verify(&pk, b"m", &bad));
        }
    }

    /// A non-canonical encoding (y + p) of a point of large order:
    /// `ed25519-dalek` decodes it as a key, the check of ours refuses it.
    #[test]
    fn non_canonical_large_order_keys_are_refused() {
        let mut found = 0;
        for y in 2u8..19 {
            let mut canonical = [0u8; 32];
            canonical[0] = y;
            let Ok(key) = VerifyingKey::from_bytes(&canonical) else {
                continue;
            };
            if key.is_weak() {
                continue;
            }
            // y + p: low byte y + 0xed (no carry for y < 19), then all ones.
            let mut alias = [0xFFu8; 32];
            alias[0] = y + 0xED;
            alias[31] = 0x7F;
            assert!(
                VerifyingKey::from_bytes(&alias).is_ok(),
                "dalek decodes y = {y} + p"
            );
            assert!(!canonical_y(&alias));
            assert!(!verify(&alias, b"m", &[0u8; 64]));
            found += 1;
        }
        assert!(found > 0, "no large-order point with y < 19");
        assert!(canonical_y(&public_key(&[42; 32])));
    }
}
