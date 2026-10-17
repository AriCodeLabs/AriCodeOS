// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! HMAC (RFC 2104) over SHA-256 and SHA-384, as used by HKDF and the
//! TLS 1.3 Finished messages. The message is given in parts so callers can
//! authenticate concatenations without a buffer (this crate has no `alloc`).

use crate::sha256::Sha256;
use crate::sha512::Sha384;

/// The hash interface HMAC needs: `BLOCK`-byte input blocks, `N`-byte output.
trait Digest<const N: usize> {
    const BLOCK: usize;
    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finish(self) -> [u8; N];
}

impl Digest<32> for Sha256 {
    const BLOCK: usize = 64;
    fn new() -> Self {
        Sha256::new()
    }
    fn update(&mut self, data: &[u8]) {
        Sha256::update(self, data)
    }
    fn finish(self) -> [u8; 32] {
        Sha256::finish(self)
    }
}

impl Digest<48> for Sha384 {
    const BLOCK: usize = 128;
    fn new() -> Self {
        Sha384::new()
    }
    fn update(&mut self, data: &[u8]) {
        Sha384::update(self, data)
    }
    fn finish(self) -> [u8; 48] {
        Sha384::finish(self)
    }
}

fn hmac<H: Digest<N>, const N: usize>(key: &[u8], parts: &[&[u8]]) -> [u8; N] {
    // Keys longer than a block are hashed first; shorter ones are zero-padded.
    let mut k = [0u8; 128];
    if key.len() > H::BLOCK {
        let mut h = H::new();
        h.update(key);
        k[..N].copy_from_slice(&h.finish());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let k = &mut k[..H::BLOCK];

    k.iter_mut().for_each(|b| *b ^= 0x36);
    let mut inner = H::new();
    inner.update(k);
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finish();

    // 0x36 ^ 0x5c turns the inner pad into the outer pad.
    k.iter_mut().for_each(|b| *b ^= 0x36 ^ 0x5c);
    let mut outer = H::new();
    outer.update(k);
    outer.update(&inner);
    outer.finish()
}

/// HMAC-SHA-256 of the concatenation of `parts`.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    hmac::<Sha256, 32>(key, parts)
}

/// HMAC-SHA-384 of the concatenation of `parts`.
pub fn hmac_sha384(key: &[u8], parts: &[&[u8]]) -> [u8; 48] {
    hmac::<Sha384, 48>(key, parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    fn h256(key: &[u8], parts: &[&[u8]]) -> std::string::String {
        let mut b = [0u8; 64];
        hex(&hmac_sha256(key, parts), &mut b).into()
    }

    fn h384(key: &[u8], parts: &[&[u8]]) -> std::string::String {
        let mut b = [0u8; 96];
        hex(&hmac_sha384(key, parts), &mut b).into()
    }

    #[test]
    fn rfc4231_vectors() {
        // Test case 1.
        let key = [0x0bu8; 20];
        assert_eq!(
            h256(&key, &[b"Hi There"]),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            h384(&key, &[b"Hi There"]),
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6"
        );
        // Test case 2, message split in parts.
        let parts: [&[u8]; 3] = [b"what do ya ", b"", b"want for nothing?"];
        assert_eq!(
            h256(b"Jefe", &parts),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            h384(b"Jefe", &parts),
            "af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649"
        );
        // Test case 6: key longer than the block size.
        let key = [0xaau8; 131];
        let msg: &[u8] = b"Test Using Larger Than Block-Size Key - Hash Key First";
        assert_eq!(
            h256(&key, &[msg]),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(
            h384(&key, &[msg]),
            "4ece084485813e9088d2c63a041bc5b44f9ef1012a2b588f3cd11f05033ac4c60c2ef6ab4030fe8296248df163f44952"
        );
    }

    extern crate std;
}
