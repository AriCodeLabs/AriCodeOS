//! TLS 1.3 record layer (RFC 8446 section 5): framing and AEAD record
//! protection with the per-record nonce (IV XOR sequence number) and the
//! inner content type plus optional zero padding.

use crate::keys::{traffic_key, Secret, Suite};
use crate::Error;
use alloc::boxed::Box;
use alloc::vec::Vec;
use aricode_crypto::aead::{self, AesGcm, TAG_LEN};

pub const CHANGE_CIPHER_SPEC: u8 = 20;
pub const ALERT: u8 = 21;
pub const HANDSHAKE: u8 = 22;
pub const APPLICATION_DATA: u8 = 23;

pub const HEADER_LEN: usize = 5;
/// Largest plaintext fragment (2^14).
pub const MAX_PLAINTEXT: usize = 1 << 14;
/// Largest protected record body we accept (2^14 + 256).
pub const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + 256;

/// Appends an unprotected record (`legacy_record_version` given).
pub fn write_plain(content_type: u8, version: u16, data: &[u8], out: &mut Vec<u8>) {
    out.push(content_type);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

enum Cipher {
    ChaCha([u8; 32]),
    Gcm(Box<AesGcm>),
}

/// One direction's record protection.
pub struct Protection {
    cipher: Cipher,
    iv: [u8; 12],
    seq: u64,
}

impl Protection {
    pub fn new(suite: Suite, secret: &Secret) -> Self {
        let (key, iv) = traffic_key(suite, secret);
        let cipher = match suite {
            Suite::Chacha20Poly1305Sha256 => Cipher::ChaCha(key),
            Suite::Aes128GcmSha256 | Suite::Aes256GcmSha384 => Cipher::Gcm(Box::new(
                AesGcm::new(&key[..suite.key_len()]).expect("AES key length is 16 or 32"),
            )),
        };
        Self { cipher, iv, seq: 0 }
    }

    fn nonce(&mut self) -> [u8; 12] {
        let mut nonce = self.iv;
        for (n, s) in nonce[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *n ^= s;
        }
        self.seq += 1;
        nonce
    }

    /// Appends a protected record carrying `data` (at most
    /// [`MAX_PLAINTEXT`] bytes) of type `content_type`, without padding.
    pub fn seal(&mut self, content_type: u8, data: &[u8], out: &mut Vec<u8>) {
        let len = data.len() + 1 + TAG_LEN;
        let header = [APPLICATION_DATA, 3, 3, (len >> 8) as u8, len as u8];
        let nonce = self.nonce();
        out.extend_from_slice(&header);
        let start = out.len();
        out.extend_from_slice(data);
        out.push(content_type);
        let body = &mut out[start..];
        let tag = match &self.cipher {
            Cipher::ChaCha(key) => aead::chacha20poly1305_seal(key, &nonce, &header, body),
            Cipher::Gcm(gcm) => gcm.seal(&nonce, &header, body),
        };
        out.extend_from_slice(&tag);
    }

    /// Decrypts a record body in place; returns the inner content type and
    /// the plaintext length (padding removed).
    pub fn open(
        &mut self,
        header: &[u8; HEADER_LEN],
        body: &mut [u8],
    ) -> Result<(u8, usize), Error> {
        if body.len() < TAG_LEN + 1 {
            return Err(Error::Decrypt);
        }
        let (data, tag) = body.split_at_mut(body.len() - TAG_LEN);
        let tag: [u8; TAG_LEN] = (&*tag).try_into().unwrap_or([0; TAG_LEN]);
        let nonce = self.nonce();
        let ok = match &self.cipher {
            Cipher::ChaCha(key) => aead::chacha20poly1305_open(key, &nonce, header, data, &tag),
            Cipher::Gcm(gcm) => gcm.open(&nonce, header, data, &tag),
        };
        if !ok {
            return Err(Error::Decrypt);
        }
        let Some(end) = data.iter().rposition(|&b| b != 0) else {
            return Err(Error::Protocol("record without content type"));
        };
        if end > MAX_PLAINTEXT {
            return Err(Error::Protocol("record overflow"));
        }
        Ok((data[end], end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Schedule;
    use crate::rfc8448::*;

    /// The server's encrypted flight and the client's Finished record of
    /// RFC 8448 section 3 (TLS_AES_128_GCM_SHA256).
    #[test]
    fn rfc8448_records() {
        let suite = Suite::Aes128GcmSha256;
        let mut transcript = h(CLIENT_HELLO);
        transcript.extend_from_slice(&h(SERVER_HELLO));
        let shared = h(SHARED);
        let (_, hs) = Schedule::new(suite, &shared, &transcript);

        let record = h(SERVER_RECORD);
        let header: [u8; 5] = record[..5].try_into().unwrap();
        let mut body = record[5..].to_vec();
        let mut recv = Protection::new(suite, &hs.server);
        let (typ, len) = recv.open(&header, &mut body).unwrap();
        assert_eq!(typ, HANDSHAKE);
        assert_eq!(&body[..len], h(SERVER_FLIGHT).as_slice());

        let mut send = Protection::new(suite, &hs.client);
        let mut out = Vec::new();
        send.seal(
            HANDSHAKE,
            &h(
                "14 00 00 20 a8 ec 43 6d 67 76 34 ae 52 5a c1 fc eb e1 1a 03 9e
                c1 76 94 fa c6 e9 85 27 b6 42 f2 ed d5 ce 61",
            ),
            &mut out,
        );
        assert_eq!(out, h(CLIENT_FINISHED_RECORD));

        // Tampering, and replaying the same record (wrong sequence number).
        let mut tampered = record[5..].to_vec();
        tampered[10] ^= 1;
        let mut fresh = Protection::new(suite, &hs.server);
        assert_eq!(fresh.open(&header, &mut tampered), Err(Error::Decrypt));
        let mut again = record[5..].to_vec();
        assert_eq!(recv.open(&header, &mut again), Err(Error::Decrypt));
    }

    #[test]
    fn padding_is_stripped_and_all_padding_rejected() {
        for suite in Suite::ALL {
            let (_, hs) = Schedule::new(suite, &[1; 32], b"t");
            // Padded inner plaintext "ok" 23 0 0 0: sealing with type 0
            // appends the last zero.
            let mut send = Protection::new(suite, &hs.server);
            let mut padded = Vec::new();
            send.seal(0, b"ok\x17\0\0", &mut padded);
            let header: [u8; 5] = padded[..5].try_into().unwrap();
            let mut recv = Protection::new(suite, &hs.server);
            let (typ, len) = recv.open(&header, &mut padded[5..]).unwrap();
            assert_eq!((typ, &padded[5..5 + len]), (APPLICATION_DATA, &b"ok"[..]));

            let mut send = Protection::new(suite, &hs.server);
            let mut zeros = Vec::new();
            send.seal(0, &[0, 0], &mut zeros);
            let header: [u8; 5] = zeros[..5].try_into().unwrap();
            let mut recv = Protection::new(suite, &hs.server);
            assert!(matches!(
                recv.open(&header, &mut zeros[5..]),
                Err(Error::Protocol(_))
            ));
        }
    }
}
