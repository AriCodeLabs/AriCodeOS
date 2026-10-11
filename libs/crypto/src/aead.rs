//! AEAD ciphers of TLS 1.3: ChaCha20-Poly1305 (RFC 8439 section 2.8) and
//! AES-GCM (NIST SP 800-38D) with 128- or 256-bit keys, both with 96-bit
//! nonces and 16-byte tags. Data is encrypted or decrypted in place; `open`
//! leaves the data untouched when the tag does not match.

use crate::aes::Aes;
use crate::chacha20::{block, xor_keystream};
use crate::ct_eq;
use crate::poly1305::Poly1305;

pub const TAG_LEN: usize = 16;

fn chacha_tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; TAG_LEN] {
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&block(key, nonce, 0)[..32]);
    let mut p = Poly1305::new(&otk);
    p.update_padded(aad);
    p.update_padded(ct);
    let mut lens = [0u8; 16];
    lens[..8].copy_from_slice(&(aad.len() as u64).to_le_bytes());
    lens[8..].copy_from_slice(&(ct.len() as u64).to_le_bytes());
    p.update_padded(&lens);
    p.finish()
}

/// Encrypts `data` in place and returns the tag.
pub fn chacha20poly1305_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
) -> [u8; TAG_LEN] {
    xor_keystream(key, nonce, 1, data);
    chacha_tag(key, nonce, aad, data)
}

/// Checks `tag` and, if it is valid, decrypts `data` in place.
pub fn chacha20poly1305_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    tag: &[u8; TAG_LEN],
) -> bool {
    if !ct_eq(&chacha_tag(key, nonce, aad, data), tag) {
        return false;
    }
    xor_keystream(key, nonce, 1, data);
    true
}

/// AES-GCM with an expanded 128- or 256-bit key.
#[derive(Clone)]
pub struct AesGcm {
    aes: Aes,
    /// The GHASH key `H = E(K, 0^128)`.
    h: u128,
}

/// Multiplication in GF(2^128) with the GCM bit order, without
/// data-dependent branches.
fn gf_mul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xe1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in (0..128).rev() {
        z ^= v & 0u128.wrapping_sub((x >> i) & 1);
        v = (v >> 1) ^ (R & 0u128.wrapping_sub(v & 1));
    }
    z
}

impl AesGcm {
    /// `key` must be 16 or 32 bytes long.
    pub fn new(key: &[u8]) -> Option<Self> {
        let aes = Aes::new(key)?;
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        Some(Self {
            aes,
            h: u128::from_be_bytes(h),
        })
    }

    fn counter_block(nonce: &[u8; 12], counter: u32) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..12].copy_from_slice(nonce);
        b[12..].copy_from_slice(&counter.to_be_bytes());
        b
    }

    /// CTR mode starting at counter 2 (counter 1 masks the tag).
    fn ctr(&self, nonce: &[u8; 12], data: &mut [u8]) {
        for (i, chunk) in data.chunks_mut(16).enumerate() {
            let mut ks = Self::counter_block(nonce, (i as u32).wrapping_add(2));
            self.aes.encrypt_block(&mut ks);
            for (d, k) in chunk.iter_mut().zip(ks) {
                *d ^= k;
            }
        }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; TAG_LEN] {
        let mut y = 0u128;
        for part in [aad, ct] {
            for chunk in part.chunks(16) {
                let mut b = [0u8; 16];
                b[..chunk.len()].copy_from_slice(chunk);
                y = gf_mul(y ^ u128::from_be_bytes(b), self.h);
            }
        }
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        y = gf_mul(y ^ lens, self.h);
        let mut j0 = Self::counter_block(nonce, 1);
        self.aes.encrypt_block(&mut j0);
        (y ^ u128::from_be_bytes(j0)).to_be_bytes()
    }

    /// Encrypts `data` in place and returns the tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; TAG_LEN] {
        self.ctr(nonce, data);
        self.tag(nonce, aad, data)
    }

    /// Checks `tag` and, if it is valid, decrypts `data` in place.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8; TAG_LEN]) -> bool {
        if !ct_eq(&self.tag(nonce, aad, data), tag) {
            return false;
        }
        self.ctr(nonce, data);
        true
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
}
