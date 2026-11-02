// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Poly1305 one-time authenticator (RFC 8439 section 2.5) over RustCrypto's
//! `poly1305` (ADR 0066), whose state wipes itself on every backend. The
//! key must never be reused: SSH derives a fresh one per packet from
//! ChaCha20.

use poly1305::universal_hash::KeyInit;

pub const KEY_LEN: usize = 32;
pub const TAG_LEN: usize = 16;

/// Tag of `msg` under the one-time `key`.
pub fn tag(key: &[u8; KEY_LEN], msg: &[u8]) -> [u8; TAG_LEN] {
    poly1305::Poly1305::new(key.into())
        .compute_unpadded(msg)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> std::vec::Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn rfc8439_vector() {
        // Section 2.5.2.
        let key: [u8; 32] = hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
            .try_into()
            .unwrap();
        assert_eq!(
            tag(&key, b"Cryptographic Forum Research Group").to_vec(),
            hex("a8061dc1305136c6c22b8baf0c0127a9")
        );
    }

    #[test]
    fn edge_cases_from_rfc8439_appendix() {
        // A.3 test vector #1: all-zero key and message give a zero tag.
        assert_eq!(tag(&[0u8; 32], &[0u8; 64]), [0u8; 16]);
        // A.3 #11 style: h close to p exercises the final reduction.
        let mut key = [0u8; 32];
        key[0] = 2;
        let msg = [0xffu8; 16];
        assert_eq!(
            tag(&key, &msg).to_vec(),
            hex("03000000000000000000000000000000")
        );
    }

    extern crate std;
}
