// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AEAD ciphers of TLS 1.3: ChaCha20-Poly1305 (RFC 8439 section 2.8) and
//! AES-GCM (NIST SP 800-38D) with 128- or 256-bit keys, both with 96-bit
//! nonces and 16-byte tags, over RustCrypto's `chacha20poly1305` and
//! `aes-gcm` (ADR 0066). Data is encrypted or decrypted in place; `open`
//! leaves the data untouched when the tag does not match.

use aes_gcm::aead::{AeadInPlace, KeyInit};

pub const TAG_LEN: usize = 16;

/// Encrypts `data` in place and returns the tag.
///
/// # Panics
///
/// Past 256 GiB of data, the limit of RFC 8439.
pub fn chacha20poly1305_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
) -> [u8; TAG_LEN] {
    chacha20poly1305::ChaCha20Poly1305::new(key.into())
        .encrypt_in_place_detached(nonce.into(), aad, data)
        .expect("ChaCha20-Poly1305 message within 256 GiB")
        .into()
}

/// Checks `tag` and, if it is valid, decrypts `data` in place.
pub fn chacha20poly1305_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    tag: &[u8; TAG_LEN],
) -> bool {
    chacha20poly1305::ChaCha20Poly1305::new(key.into())
        .decrypt_in_place_detached(nonce.into(), aad, data, tag.into())
        .is_ok()
}

/// AES-GCM with an expanded 128- or 256-bit key; the key schedule and the
/// GHASH key are wiped when it is dropped, whatever backend the CPU
/// selected (`polyval`'s run-time detected one does not wipe itself).
#[derive(Clone)]
pub struct AesGcm(Key);

// No `alloc` in this crate to box the larger AES-256 key schedule; an
// `AesGcm` lives once per TLS connection direction.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum Key {
    Aes128(aes_gcm::Aes128Gcm),
    Aes256(aes_gcm::Aes256Gcm),
}

impl AesGcm {
    /// `key` must be 16 or 32 bytes long.
    pub fn new(key: &[u8]) -> Option<Self> {
        // The key expansion's intermediate words are scrubbed.
        crate::scrubbed::<{ crate::SCRUB_HASH }, _>(|| {
            match key.len() {
                16 => aes_gcm::Aes128Gcm::new_from_slice(key)
                    .ok()
                    .map(Key::Aes128),
                32 => aes_gcm::Aes256Gcm::new_from_slice(key)
                    .ok()
                    .map(Key::Aes256),
                _ => None,
            }
            .map(Self)
        })
    }

    /// Encrypts `data` in place and returns the tag.
    ///
    /// # Panics
    ///
    /// Past 64 GiB of data, the limit of NIST SP 800-38D.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; TAG_LEN] {
        let n = nonce.into();
        match &self.0 {
            Key::Aes128(c) => c.encrypt_in_place_detached(n, aad, data),
            Key::Aes256(c) => c.encrypt_in_place_detached(n, aad, data),
        }
        .expect("AES-GCM message within 64 GiB")
        .into()
    }

    /// Checks `tag` and, if it is valid, decrypts `data` in place.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8; TAG_LEN]) -> bool {
        let (n, t) = (nonce.into(), tag.into());
        match &self.0 {
            Key::Aes128(c) => c.decrypt_in_place_detached(n, aad, data, t),
            Key::Aes256(c) => c.decrypt_in_place_detached(n, aad, data, t),
        }
        .is_ok()
    }
}

impl Drop for AesGcm {
    fn drop(&mut self) {
        // SAFETY: AES key schedules and GHASH states are words and SIMD
        // registers, valid as zeros; the variant is left as it is.
        unsafe {
            match &mut self.0 {
                Key::Aes128(c) => crate::wipe_state(c),
                Key::Aes256(c) => crate::wipe_state(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unhex;
    use std::vec::Vec;

    extern crate std;

    fn v(s: &str) -> Vec<u8> {
        let mut b = std::vec![0u8; s.len() / 2];
        unhex(s, &mut b).unwrap();
        b
    }

    fn a<const N: usize>(s: &str) -> [u8; N] {
        v(s).try_into().unwrap()
    }

    #[test]
    fn rfc8439_2_8_2() {
        let key = a::<32>("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        let nonce = a::<12>("070000004041424344454647");
        let aad = v("50515253c0c1c2c3c4c5c6c7");
        let pt: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let mut data = pt.to_vec();
        let tag = chacha20poly1305_seal(&key, &nonce, &aad, &mut data);
        assert_eq!(
            data,
            v(concat!(
                "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6",
                "3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36",
                "92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc",
                "3ff4def08e4b7a9de576d26586cec64b6116"
            ))
        );
        assert_eq!(tag, a::<16>("1ae10b594f09e26a7e902ecbd0600691"));

        // Negative cases: tampered tag, ciphertext or AAD leave data as is.
        let mut bad = tag;
        bad[0] ^= 1;
        let sealed = data.clone();
        assert!(!chacha20poly1305_open(&key, &nonce, &aad, &mut data, &bad));
        assert_eq!(data, sealed);
        data[5] ^= 1;
        assert!(!chacha20poly1305_open(&key, &nonce, &aad, &mut data, &tag));
        data[5] ^= 1;
        assert!(!chacha20poly1305_open(
            &key,
            &nonce,
            &aad[1..],
            &mut data,
            &tag
        ));
        assert!(chacha20poly1305_open(&key, &nonce, &aad, &mut data, &tag));
        assert_eq!(data, pt);
    }

    /// Runs one GCM test case from the McGrew-Viega GCM specification
    /// (the vectors NIST published with SP 800-38D).
    fn gcm_case(key: &str, iv: &str, pt: &str, aad: &str, ct: &str, tag: &str) {
        let gcm = AesGcm::new(&v(key)).unwrap();
        let iv = a::<12>(iv);
        let (aad, tag) = (v(aad), a::<16>(tag));
        let mut data = v(pt);
        assert_eq!(gcm.seal(&iv, &aad, &mut data), tag);
        assert_eq!(data, v(ct));
        if !data.is_empty() {
            data[0] ^= 0x80;
            assert!(!gcm.open(&iv, &aad, &mut data, &tag));
            data[0] ^= 0x80;
        }
        let mut bad = tag;
        bad[15] ^= 1;
        assert!(!gcm.open(&iv, &aad, &mut data, &bad));
        assert!(gcm.open(&iv, &aad, &mut data, &tag));
        assert_eq!(data, v(pt));
    }

    const K4: &str = "feffe9928665731c6d6a8f9467308308";
    const IV4: &str = "cafebabefacedbaddecaf888";
    const P4: &str = concat!(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72",
        "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39"
    );
    const A4: &str = "feedfacedeadbeeffeedfacedeadbeefabaddad2";

    #[test]
    fn gcm_aes128() {
        let z16 = "00000000000000000000000000000000";
        let z12 = "000000000000000000000000";
        gcm_case(z16, z12, "", "", "", "58e2fccefa7e3061367f1d57a4e7455a");
        gcm_case(
            z16,
            z12,
            z16,
            "",
            "0388dace60b6a392f328c2b971b2fe78",
            "ab6e47d42cec13bdf53a67b21257bddf",
        );
        gcm_case(
            K4,
            IV4,
            P4,
            A4,
            concat!(
                "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e",
                "21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091"
            ),
            "5bc94fbc3221a5db94fae95ae7121a47",
        );
    }

    #[test]
    fn gcm_aes256() {
        let z32 = "0000000000000000000000000000000000000000000000000000000000000000";
        let z16 = "00000000000000000000000000000000";
        let z12 = "000000000000000000000000";
        gcm_case(z32, z12, "", "", "", "530f8afbc74536b9a963b4f1c4cb738b");
        gcm_case(
            z32,
            z12,
            z16,
            "",
            "cea7403d4d606b6e074ec5d3baf39d18",
            "d0d1c8a799996bf0265b98b5d48ab919",
        );
        let k16 = std::format!("{K4}{K4}");
        gcm_case(
            &k16,
            IV4,
            P4,
            A4,
            concat!(
                "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa",
                "8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662"
            ),
            "76fc6ece0f4e1768cddf8853bb2d551b",
        );
    }

    #[test]
    fn gcm_rejects_bad_key_length() {
        assert!(AesGcm::new(&[0u8; 24]).is_none());
    }

    #[test]
    fn keys_are_wiped_when_dropped() {
        for len in [16, 32] {
            let mut gcm = core::mem::ManuallyDrop::new(AesGcm::new(&[0xA5; 32][..len]).unwrap());
            let state = |g: &AesGcm| match &g.0 {
                Key::Aes128(c) => crate::tests::bytes_of(c).to_vec(),
                Key::Aes256(c) => crate::tests::bytes_of(c).to_vec(),
            };
            assert!(state(&gcm).iter().any(|&b| b != 0));
            // SAFETY: dropped once; afterwards only its bytes are read.
            unsafe { core::mem::ManuallyDrop::drop(&mut gcm) };
            assert!(state(&gcm).iter().all(|&b| b == 0), "AES-{}", len * 8);
        }
    }
}
