// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! HKDF (RFC 5869) over SHA-256 and SHA-384: the TLS 1.3 key schedule.

use crate::hmac::{hmac_sha256, hmac_sha384};

/// HKDF-Extract with SHA-256: the pseudorandom key `HMAC(salt, ikm)`.
pub fn extract_sha256(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    hmac_sha256(salt, &[ikm])
}

/// HKDF-Extract with SHA-384.
pub fn extract_sha384(salt: &[u8], ikm: &[u8]) -> [u8; 48] {
    hmac_sha384(salt, &[ikm])
}

/// HKDF-Expand: fills `out` with `T(1) || T(2) || ...` where
/// `T(i) = HMAC(prk, T(i-1) || info || i)`.
fn expand<const N: usize>(
    mac: fn(&[u8], &[&[u8]]) -> [u8; N],
    prk: &[u8],
    info: &[u8],
    out: &mut [u8],
) {
    assert!(out.len() <= 255 * N, "HKDF output too long");
    let mut t = [0u8; N];
    for (i, chunk) in out.chunks_mut(N).enumerate() {
        let counter = [i as u8 + 1];
        let prev: &[u8] = if i == 0 { &[] } else { &t };
        t = mac(prk, &[prev, info, &counter]);
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

/// HKDF-Expand with SHA-256; `out.len()` must not exceed 255 * 32.
pub fn expand_sha256(prk: &[u8], info: &[u8], out: &mut [u8]) {
    expand(hmac_sha256, prk, info, out)
}

/// HKDF-Expand with SHA-384; `out.len()` must not exceed 255 * 48.
pub fn expand_sha384(prk: &[u8], info: &[u8], out: &mut [u8]) {
    expand(hmac_sha384, prk, info, out)
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
    fn rfc5869_a1_sha256() {
        let ikm = [0x0bu8; 22];
        let salt: [u8; 13] = bytes("000102030405060708090a0b0c");
        let info: [u8; 10] = bytes("f0f1f2f3f4f5f6f7f8f9");
        let prk = extract_sha256(&salt, &ikm);
        assert_eq!(
            prk,
            bytes::<32>("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5")
        );
        let mut okm = [0u8; 42];
        expand_sha256(&prk, &info, &mut okm);
        assert_eq!(
            okm,
            bytes::<42>(
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
            )
        );
    }

    #[test]
    fn rfc5869_a3_empty_salt_and_info() {
        let prk = extract_sha256(&[], &[0x0bu8; 22]);
        let mut okm = [0u8; 42];
        expand_sha256(&prk, &[], &mut okm);
        assert_eq!(
            okm,
            bytes::<42>(
                "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
            )
        );
    }

    #[test]
    fn sha384_against_openssl() {
        // openssl kdf -keylen 100 -kdfopt digest:SHA384 -kdfopt hexkey:<ikm>
        //   -kdfopt hexsalt:<salt> -kdfopt hexinfo:<info> HKDF
        let ikm = [0x0bu8; 22];
        let salt: [u8; 13] = bytes("000102030405060708090a0b0c");
        let info: [u8; 10] = bytes("f0f1f2f3f4f5f6f7f8f9");
        let prk = extract_sha384(&salt, &ikm);
        let mut okm = [0u8; 100];
        expand_sha384(&prk, &info, &mut okm);
        assert_eq!(okm, bytes::<100>(OPENSSL_SHA384_OKM));
    }

    const OPENSSL_SHA384_OKM: &str = concat!(
        "9b5097a86038b805309076a44b3a9f38063e25b516dcbf369f394cfab43685f748b6457763e4f0204fc5d95d1da3e62587b2",
        "2eb8943d0fab6bb631a2fe9df1a68c6ce5d56116a52005b3f122b88b39b7251fcd6c44d3ef25f20ed96802bf1b2c1d98bf74"
    );
}
