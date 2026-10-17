// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `chacha20-poly1305@openssh.com` (OpenSSH PROTOCOL.chacha20poly1305), the
//! SSH packet cipher (ADR 0025). The 64-byte key is split in two ChaCha20
//! keys: the second half encrypts the 4-byte packet length, the first half
//! the rest of the packet and, from its block 0, the one-time Poly1305 key.
//! The nonce is the packet sequence number (big endian) and the tag covers
//! the encrypted length and body.

use crate::chacha20::{block, xor_keystream};
use crate::poly1305;

pub const KEY_LEN: usize = 64;
pub const TAG_LEN: usize = poly1305::TAG_LEN;

#[derive(Clone)]
pub struct ChaChaPoly {
    main: [u8; 32],
    header: [u8; 32],
}

fn nonce(seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&(seq as u64).to_be_bytes());
    n
}

impl ChaChaPoly {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        let mut main = [0u8; 32];
        let mut header = [0u8; 32];
        main.copy_from_slice(&key[..32]);
        header.copy_from_slice(&key[32..]);
        Self { main, header }
    }

    /// Decrypts the 4-byte length field of packet `seq` (needed before the
    /// rest of the packet can be read).
    pub fn decrypt_length(&self, seq: u32, enc: &[u8; 4]) -> u32 {
        let mut l = *enc;
        xor_keystream(&self.header, &nonce(seq), 0, &mut l);
        u32::from_be_bytes(l)
    }

    /// Encrypts `packet` in place (4-byte length followed by the body) and
    /// returns the tag to append.
    pub fn seal(&self, seq: u32, packet: &mut [u8]) -> [u8; TAG_LEN] {
        let n = nonce(seq);
        let (len, body) = packet.split_at_mut(4);
        xor_keystream(&self.header, &n, 0, len);
        xor_keystream(&self.main, &n, 1, body);
        self.tag(&n, packet)
    }

    /// Checks the tag of the encrypted `packet` (length + body) and, if it is
    /// valid, decrypts it in place. `false` leaves the packet untouched.
    pub fn open(&self, seq: u32, packet: &mut [u8], tag: &[u8; TAG_LEN]) -> bool {
        let n = nonce(seq);
        if !crate::ct_eq(&self.tag(&n, packet), tag) {
            return false;
        }
        let (len, body) = packet.split_at_mut(4);
        xor_keystream(&self.header, &n, 0, len);
        xor_keystream(&self.main, &n, 1, body);
        true
    }

    fn tag(&self, n: &[u8; 12], packet: &[u8]) -> [u8; TAG_LEN] {
        let b0 = block(&self.main, n, 0);
        let mut key = [0u8; 32];
        key.copy_from_slice(&b0[..32]);
        poly1305::tag(&key, packet)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn matches_an_independent_openssl_computation() {
        // Vector computed with the OpenSSL command line (chacha20 keystream
        // and POLY1305) for key = 0..64, seq = 7.
        let key: [u8; 64] = core::array::from_fn(|i| i as u8);
        let c = ChaChaPoly::new(&key);
        let body = hex("04050000000c7373682d7573657261757468aabbccdd");
        let mut packet = Vec::new();
        packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
        packet.extend_from_slice(&body);
        let tag = c.seal(7, &mut packet);
        assert_eq!(
            packet,
            hex("a39afcbc2c4315434e8f592d0440ce83b2fdb25940da9a8db23e")
        );
        assert_eq!(tag.to_vec(), hex("0f02f7a115a0a3475d077bc53de85dcc"));
        // The receiver reads the length first, then authenticates and opens.
        let enc_len: [u8; 4] = packet[..4].try_into().unwrap();
        assert_eq!(c.decrypt_length(7, &enc_len), body.len() as u32);
        let mut copy = packet.clone();
        assert!(c.open(7, &mut copy, &tag));
        assert_eq!(&copy[4..], &body[..]);
        // A flipped bit or the wrong sequence number is rejected untouched.
        let mut bad = packet.clone();
        bad[10] ^= 1;
        assert!(!c.open(7, &mut bad, &tag));
        assert_eq!(bad[10], packet[10] ^ 1);
        let mut wrong_seq = packet.clone();
        assert!(!c.open(8, &mut wrong_seq, &tag));
    }
}
