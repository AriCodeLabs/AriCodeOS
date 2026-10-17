// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! RSA signature verification (RFC 8017): RSASSA-PKCS1-v1_5 (certificates)
//! and RSASSA-PSS with MGF1 and a salt as long as the hash (the TLS 1.3
//! `rsa_pss_rsae_*` schemes). Keys and signatures are big-endian byte
//! strings; moduli up to [`bigint::MAX_BITS`] bits and any public exponent.

use crate::bigint::{self, Modulus};
use crate::sha256::Sha256;
use crate::sha512::{Sha384, Sha512};
use crate::{ct_eq, Hash};

const MAX_BYTES: usize = bigint::MAX_BITS / 8;

/// `s^e mod n` as a big-endian string of the modulus length into `em`;
/// returns that length. `None` for an invalid key or a signature that is
/// not exactly as long as the modulus or not smaller than it.
fn rsavp1(n: &[u8], e: &[u8], sig: &[u8], em: &mut [u8; MAX_BYTES]) -> Option<(Modulus, usize)> {
    let m = Modulus::new(n)?;
    let k = m.byte_len();
    if sig.len() != k || e.iter().all(|&b| b == 0) {
        return None;
    }
    m.pow(sig, e, &mut em[..k])?;
    Some((m, k))
}

/// DER `DigestInfo` prefix (AlgorithmIdentifier with NULL parameters and
/// the OCTET STRING header) for each hash.
fn digest_info_prefix(hash: Hash) -> &'static [u8] {
    match hash {
        Hash::Sha256 => &[
            0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x01, 0x05, 0x00, 0x04, 0x20,
        ],
        Hash::Sha384 => &[
            0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x02, 0x05, 0x00, 0x04, 0x30,
        ],
        Hash::Sha512 => &[
            0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x03, 0x05, 0x00, 0x04, 0x40,
        ],
    }
}

/// RSASSA-PKCS1-v1_5 verification: `digest` is the hash of the message.
/// The encoded message must be exactly
/// `00 01 FF..FF 00 DigestInfo(hash, digest)`.
pub fn verify_pkcs1v15(n: &[u8], e: &[u8], hash: Hash, digest: &[u8], sig: &[u8]) -> bool {
    if digest.len() != hash.len() {
        return false;
    }
    let mut em = [0u8; MAX_BYTES];
    let Some((_, k)) = rsavp1(n, e, sig, &mut em) else {
        return false;
    };
    let prefix = digest_info_prefix(hash);
    let t_len = prefix.len() + digest.len();
    // At least 8 bytes of 0xFF padding.
    if k < t_len + 11 {
        return false;
    }
    let mut expected = [0u8; MAX_BYTES];
    expected[1] = 1;
    expected[2..k - t_len - 1].fill(0xff);
    expected[k - t_len..k - digest.len()].copy_from_slice(prefix);
    expected[k - digest.len()..k].copy_from_slice(digest);
    ct_eq(&em[..k], &expected[..k])
}

/// Hash of the concatenation of `parts` into the front of `out`.
fn hash_parts(hash: Hash, parts: &[&[u8]], out: &mut [u8; 64]) {
    match hash {
        Hash::Sha256 => {
            let mut h = Sha256::new();
            parts.iter().for_each(|p| h.update(p));
            out[..32].copy_from_slice(&h.finish());
        }
        Hash::Sha384 => {
            let mut h = Sha384::new();
            parts.iter().for_each(|p| h.update(p));
            out[..48].copy_from_slice(&h.finish());
        }
        Hash::Sha512 => {
            let mut h = Sha512::new();
            parts.iter().for_each(|p| h.update(p));
            out[..64].copy_from_slice(&h.finish());
        }
    }
}

/// XORs `MGF1(seed)` into `data`.
fn mgf1_xor(hash: Hash, seed: &[u8], data: &mut [u8]) {
    let mut t = [0u8; 64];
    for (i, chunk) in data.chunks_mut(hash.len()).enumerate() {
        hash_parts(hash, &[seed, &(i as u32).to_be_bytes()], &mut t);
        for (d, m) in chunk.iter_mut().zip(t) {
            *d ^= m;
        }
    }
}

/// RSASSA-PSS verification with MGF1 over `hash` and a salt of
/// `hash.len()` bytes (EMSA-PSS-VERIFY, RFC 8017 section 9.1.2).
pub fn verify_pss(n: &[u8], e: &[u8], hash: Hash, digest: &[u8], sig: &[u8]) -> bool {
    let h_len = hash.len();
    if digest.len() != h_len {
        return false;
    }
    let mut buf = [0u8; MAX_BYTES];
    let Some((m, k)) = rsavp1(n, e, sig, &mut buf) else {
        return false;
    };
    // EM has emBits = modBits - 1 bits; when modBits = 8k + 1 it is one
    // byte shorter than the modulus and the leading byte must be zero.
    let em_bits = m.bits() - 1;
    let em_len = em_bits.div_ceil(8);
    if em_len < k && buf[0] != 0 {
        return false;
    }
    let em = &mut buf[k - em_len..k];
    let s_len = h_len;
    if em_len < h_len + s_len + 2 || em[em_len - 1] != 0xbc {
        return false;
    }
    let db_len = em_len - h_len - 1;
    let (db, rest) = em.split_at_mut(db_len);
    let h = &rest[..h_len];
    let top_mask = 0xffu8 >> (8 * em_len - em_bits);
    if db[0] & !top_mask != 0 {
        return false;
    }
    mgf1_xor(hash, h, db);
    db[0] &= top_mask;
    let ps_len = db_len - s_len - 1;
    if db[..ps_len].iter().any(|&b| b != 0) || db[ps_len] != 1 {
        return false;
    }
    let salt = &db[ps_len + 1..];
    let mut h2 = [0u8; 64];
    hash_parts(hash, &[&[0u8; 8], digest, salt], &mut h2);
    ct_eq(&h2[..h_len], h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    extern crate std;

    /// Signatures made with OpenSSL 3.5 (`openssl dgst -<hash> -sign key.pem`,
    /// plus `-sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:digest`)
    /// with 1537-, 2048-, 3072-bit (e = 3) and 4096-bit keys (generator in
    /// `testdata/gen_rsa_vectors.sh`). One line per
    /// signature: `bits scheme hash n e message signature` (hex).
    const VECTORS: &str = include_str!("../testdata/rsa_vectors.txt");

    struct Vector {
        bits: usize,
        pss: bool,
        hash: Hash,
        n: Vec<u8>,
        e: Vec<u8>,
        digest: Vec<u8>,
        sig: Vec<u8>,
    }

    fn unhex(s: &str) -> Vec<u8> {
        let mut b = std::vec![0u8; s.len() / 2];
        crate::unhex(s, &mut b).unwrap();
        b
    }

    fn vectors() -> Vec<Vector> {
        VECTORS
            .lines()
            .map(|l| {
                let f: Vec<&str> = l.split(' ').collect();
                let hash = match f[2] {
                    "sha256" => Hash::Sha256,
                    "sha384" => Hash::Sha384,
                    _ => Hash::Sha512,
                };
                let mut d = [0u8; 64];
                let len = hash.digest(&unhex(f[5]), &mut d);
                Vector {
                    bits: f[0].parse().unwrap(),
                    pss: f[1] == "pss",
                    hash,
                    n: unhex(f[3]),
                    e: unhex(f[4]),
                    digest: d[..len].to_vec(),
                    sig: unhex(f[6]),
                }
            })
            .collect()
    }

    fn verify(v: &Vector, n: &[u8], e: &[u8], hash: Hash, digest: &[u8], sig: &[u8]) -> bool {
        if v.pss {
            verify_pss(n, e, hash, digest, sig)
        } else {
            verify_pkcs1v15(n, e, hash, digest, sig)
        }
    }

    #[test]
    fn openssl_signatures_verify() {
        let vs = vectors();
        assert_eq!(vs.len(), 24);
        for v in &vs {
            assert_eq!(Modulus::new(&v.n).unwrap().bits(), v.bits);
            assert!(
                verify(v, &v.n, &v.e, v.hash, &v.digest, &v.sig),
                "{} {:?}",
                v.bits,
                v.hash
            );
            // A leading zero on the modulus (as in DER INTEGERs) is accepted.
            let mut n0 = std::vec![0u8];
            n0.extend_from_slice(&v.n);
            assert!(verify(v, &n0, &v.e, v.hash, &v.digest, &v.sig));
        }
    }

    #[test]
    fn tampering_is_rejected() {
        let vs = vectors();
        for v in &vs {
            let ok = |n: &[u8], e: &[u8], hash, digest: &[u8], sig: &[u8]| {
                verify(v, n, e, hash, digest, sig)
            };
            let mut sig = v.sig.clone();
            sig[10] ^= 1;
            assert!(!ok(&v.n, &v.e, v.hash, &v.digest, &sig));
            let mut digest = v.digest.clone();
            digest[0] ^= 0x80;
            assert!(!ok(&v.n, &v.e, v.hash, &digest, &v.sig));
            let mut n = v.n.clone();
            n[5] ^= 0x10;
            assert!(!ok(&n, &v.e, v.hash, &v.digest, &v.sig));
            assert!(!ok(&v.n, &[1, 0, 0, 1], v.hash, &v.digest, &v.sig));
            assert!(!ok(&v.n, &[], v.hash, &v.digest, &v.sig));
            // Wrong hash: different digest length or the same length.
            let other = if v.hash == Hash::Sha256 {
                Hash::Sha384
            } else {
                Hash::Sha256
            };
            let mut d = [0u8; 64];
            let len = other.digest(b"The quick brown fox jumps over the lazy dog", &mut d);
            assert!(!ok(&v.n, &v.e, other, &d[..len], &v.sig));
            assert!(!ok(&v.n, &v.e, v.hash, &v.digest[1..], &v.sig));
            // Signature with the wrong length or not smaller than n.
            assert!(!ok(&v.n, &v.e, v.hash, &v.digest, &v.sig[1..]));
            let mut long = std::vec![0u8];
            long.extend_from_slice(&v.sig);
            assert!(!ok(&v.n, &v.e, v.hash, &v.digest, &long));
            assert!(!ok(&v.n, &v.e, v.hash, &v.digest, &v.n));
        }
    }

    #[test]
    fn schemes_are_not_interchangeable() {
        for v in &vectors() {
            let other = if v.pss {
                verify_pkcs1v15(&v.n, &v.e, v.hash, &v.digest, &v.sig)
            } else {
                verify_pss(&v.n, &v.e, v.hash, &v.digest, &v.sig)
            };
            assert!(!other);
        }
    }

    #[test]
    fn oversized_modulus_is_rejected() {
        let n = [0xffu8; MAX_BYTES + 1];
        let sig = [1u8; MAX_BYTES + 1];
        let d = [0u8; 32];
        assert!(!verify_pkcs1v15(&n, &[3], Hash::Sha256, &d, &sig));
        assert!(!verify_pss(&n, &[3], Hash::Sha256, &d, &sig));
    }
}
