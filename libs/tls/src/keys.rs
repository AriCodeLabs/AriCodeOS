//! TLS 1.3 key schedule (RFC 8446 section 7): HKDF-Expand-Label,
//! Derive-Secret, the handshake and application traffic secrets, Finished
//! keys and KeyUpdate. The transcript is passed as the concatenation of the
//! handshake messages and hashed here.

use aricode_crypto::{hkdf, hmac, Hash};

/// The three cipher suites we offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    Aes128GcmSha256,
    Aes256GcmSha384,
    Chacha20Poly1305Sha256,
}

impl Suite {
    /// In our order of preference (ChaCha20 is the fastest in software).
    pub const ALL: [Suite; 3] = [
        Suite::Chacha20Poly1305Sha256,
        Suite::Aes128GcmSha256,
        Suite::Aes256GcmSha384,
    ];

    pub fn id(self) -> u16 {
        match self {
            Suite::Aes128GcmSha256 => 0x1301,
            Suite::Aes256GcmSha384 => 0x1302,
            Suite::Chacha20Poly1305Sha256 => 0x1303,
        }
    }

    pub fn from_id(id: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn hash(self) -> Hash {
        match self {
            Suite::Aes256GcmSha384 => Hash::Sha384,
            _ => Hash::Sha256,
        }
    }

    pub fn key_len(self) -> usize {
        match self {
            Suite::Aes128GcmSha256 => 16,
            _ => 32,
        }
    }
}

/// A secret or digest as long as the suite's hash (32 or 48 bytes).
#[derive(Clone)]
pub struct Secret {
    bytes: [u8; 48],
    len: usize,
}

impl Secret {
    fn new(len: usize) -> Self {
        Self {
            bytes: [0; 48],
            len,
        }
    }

    fn from_slice(data: &[u8]) -> Self {
        let mut s = Self::new(data.len());
        s.bytes[..data.len()].copy_from_slice(data);
        s
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Hash of the transcript (the concatenated handshake messages).
pub fn transcript_hash(suite: Suite, transcript: &[u8]) -> Secret {
    let mut out = [0u8; 64];
    let n = suite.hash().digest(transcript, &mut out);
    Secret::from_slice(&out[..n])
}

/// HKDF-Extract with the suite's hash.
pub fn extract(suite: Suite, salt: &[u8], ikm: &[u8]) -> Secret {
    match suite.hash() {
        Hash::Sha384 => Secret::from_slice(&hkdf::extract_sha384(salt, ikm)),
        _ => Secret::from_slice(&hkdf::extract_sha256(salt, ikm)),
    }
}

/// HKDF-Expand-Label(secret, label, context, out.len()), with the
/// `"tls13 "` prefix added here.
pub fn expand_label(suite: Suite, secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    let mut info = [0u8; 2 + 1 + 255 + 1 + 255];
    let full_label = 6 + label.len();
    info[..2].copy_from_slice(&(out.len() as u16).to_be_bytes());
    info[2] = full_label as u8;
    info[3..9].copy_from_slice(b"tls13 ");
    info[9..9 + label.len()].copy_from_slice(label);
    let mut n = 9 + label.len();
    info[n] = context.len() as u8;
    info[n + 1..n + 1 + context.len()].copy_from_slice(context);
    n += 1 + context.len();
    match suite.hash() {
        Hash::Sha384 => hkdf::expand_sha384(secret, &info[..n], out),
        _ => hkdf::expand_sha256(secret, &info[..n], out),
    }
}

fn expand_secret(suite: Suite, secret: &Secret, label: &[u8], context: &[u8]) -> Secret {
    let mut out = Secret::new(secret.len);
    expand_label(
        suite,
        secret.as_bytes(),
        label,
        context,
        &mut out.bytes[..secret.len],
    );
    out
}

/// Derive-Secret(secret, label, messages).
pub fn derive_secret(suite: Suite, secret: &Secret, label: &[u8], transcript: &[u8]) -> Secret {
    let hash = transcript_hash(suite, transcript);
    expand_secret(suite, secret, label, hash.as_bytes())
}

/// Traffic secrets for one phase of the connection.
pub struct TrafficSecrets {
    pub client: Secret,
    pub server: Secret,
}

/// The key schedule after the (EC)DHE shared secret is known. Without a
/// pre-shared key the early secret is HKDF-Extract(0, 0).
pub struct Schedule {
    suite: Suite,
    handshake: Secret,
}

impl Schedule {
    /// `transcript`: ClientHello..ServerHello. Returns the schedule and the
    /// handshake traffic secrets.
    pub fn new(suite: Suite, shared: &[u8], transcript: &[u8]) -> (Self, TrafficSecrets) {
        let zeros = [0u8; 48];
        let len = suite.hash().len();
        let early = extract(suite, &[], &zeros[..len]);
        let derived = derive_secret(suite, &early, b"derived", &[]);
        let handshake = extract(suite, derived.as_bytes(), shared);
        let secrets = TrafficSecrets {
            client: derive_secret(suite, &handshake, b"c hs traffic", transcript),
            server: derive_secret(suite, &handshake, b"s hs traffic", transcript),
        };
        (Self { suite, handshake }, secrets)
    }

    /// `transcript`: ClientHello..server Finished. Application secrets.
    pub fn application(&self, transcript: &[u8]) -> TrafficSecrets {
        let suite = self.suite;
        let zeros = [0u8; 48];
        let derived = derive_secret(suite, &self.handshake, b"derived", &[]);
        let master = extract(suite, derived.as_bytes(), &zeros[..suite.hash().len()]);
        TrafficSecrets {
            client: derive_secret(suite, &master, b"c ap traffic", transcript),
            server: derive_secret(suite, &master, b"s ap traffic", transcript),
        }
    }
}

/// Finished verify_data: HMAC(finished_key(base_key), Hash(transcript)).
pub fn finished(suite: Suite, base_key: &Secret, transcript: &[u8]) -> Secret {
    let key = expand_secret(suite, base_key, b"finished", &[]);
    let hash = transcript_hash(suite, transcript);
    match suite.hash() {
        Hash::Sha384 => Secret::from_slice(&hmac::hmac_sha384(key.as_bytes(), &[hash.as_bytes()])),
        _ => Secret::from_slice(&hmac::hmac_sha256(key.as_bytes(), &[hash.as_bytes()])),
    }
}

/// application_traffic_secret_N+1 (KeyUpdate).
pub fn next_traffic_secret(suite: Suite, secret: &Secret) -> Secret {
    expand_secret(suite, secret, b"traffic upd", &[])
}

/// Record protection key and IV from a traffic secret.
pub fn traffic_key(suite: Suite, secret: &Secret) -> ([u8; 32], [u8; 12]) {
    let mut key = [0u8; 32];
    let mut iv = [0u8; 12];
    expand_label(
        suite,
        secret.as_bytes(),
        b"key",
        &[],
        &mut key[..suite.key_len()],
    );
    expand_label(suite, secret.as_bytes(), b"iv", &[], &mut iv);
    (key, iv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rfc8448::*;

    #[test]
    fn rfc8448_simple_1rtt_key_schedule() {
        let suite = Suite::Aes128GcmSha256;
        let shared = h(SHARED);
        // The x25519 shared secret of the trace.
        let private: [u8; 32] = h(CLIENT_PRIVATE).try_into().unwrap();
        let sh = h(SERVER_HELLO);
        let server_public: [u8; 32] = sh[sh.len() - 38..sh.len() - 6].try_into().unwrap();
        assert_eq!(
            aricode_crypto::x25519::shared(&private, &server_public)
                .unwrap()
                .as_slice(),
            shared.as_slice()
        );

        let early = extract(suite, &[], &[0; 32]);
        assert_eq!(
            early.as_bytes(),
            h("33 ad 0a 1c 60 7e c0 3b 09 e6 cd 98 93 68 0c e2 10 ad f3 00 aa 1f 26 60 e1 b2 2e 10 f1 70 f9 2a")
        );
        let derived = derive_secret(suite, &early, b"derived", &[]);
        assert_eq!(
            derived.as_bytes(),
            h("6f 26 15 a1 08 c7 02 c5 67 8f 54 fc 9d ba b6 97 16 c0 76 18 9c 48 25 0c eb ea c3 57 6c 36 11 ba")
        );

        let mut transcript = h(CLIENT_HELLO);
        transcript.extend_from_slice(&sh);
        let (schedule, hs) = Schedule::new(suite, &shared, &transcript);
        assert_eq!(
            schedule.handshake.as_bytes(),
            h("1d c8 26 e9 36 06 aa 6f dc 0a ad c1 2f 74 1b 01 04 6a a6 b9 9f 69 1e d2 21 a9 f0 ca 04 3f be ac")
        );
        assert_eq!(
            hs.client.as_bytes(),
            h("b3 ed db 12 6e 06 7f 35 a7 80 b3 ab f4 5e 2d 8f 3b 1a 95 07 38 f5 2e 96 00 74 6a 0e 27 a5 5a 21")
        );
        assert_eq!(
            hs.server.as_bytes(),
            h("b6 7b 7d 69 0c c1 6c 4e 75 e5 42 13 cb 2d 37 b4 e9 c9 12 bc de d9 10 5d 42 be fd 59 d3 91 ad 38")
        );
        let (key, iv) = traffic_key(suite, &hs.server);
        assert_eq!(
            key[..16],
            h("3f ce 51 60 09 c2 17 27 d0 f2 e4 e8 6e e4 03 bc")
        );
        assert_eq!(iv, h("5d 31 3e b2 67 12 76 ee 13 00 0b 30").as_slice());
        let (key, iv) = traffic_key(suite, &hs.client);
        assert_eq!(
            key[..16],
            h("db fa a6 93 d1 76 2c 5b 66 6a f5 d9 50 25 8d 01")
        );
        assert_eq!(iv, h("5b d3 c7 1b 83 6e 0b 76 bb 73 26 5f").as_slice());

        // Server Finished covers ClientHello..CertificateVerify.
        let flight = h(SERVER_FLIGHT);
        let (before_finished, server_finished) = flight.split_at(flight.len() - 36);
        transcript.extend_from_slice(before_finished);
        assert_eq!(
            finished(suite, &hs.server, &transcript).as_bytes(),
            &server_finished[4..]
        );
        transcript.extend_from_slice(server_finished);
        assert_eq!(
            finished(suite, &hs.client, &transcript).as_bytes(),
            h("a8 ec 43 6d 67 76 34 ae 52 5a c1 fc eb e1 1a 03 9e c1 76 94 fa c6 e9 85 27 b6 42 f2 ed d5 ce 61")
        );

        let ap = schedule.application(&transcript);
        assert_eq!(
            ap.client.as_bytes(),
            h("9e 40 64 6c e7 9a 7f 9d c0 5a f8 88 9b ce 65 52 87 5a fa 0b 06 df 00 87 f7 92 eb b7 c1 75 04 a5")
        );
        assert_eq!(
            ap.server.as_bytes(),
            h("a1 1a f9 f0 55 31 f8 56 ad 47 11 6b 45 a9 50 32 82 04 b4 f4 4b fb 6b 3a 4b 4f 1f 3f cb 63 16 43")
        );
        let (key, iv) = traffic_key(suite, &ap.client);
        assert_eq!(
            key[..16],
            h("17 42 2d da 59 6e d5 d9 ac d8 90 e3 c6 3f 50 51")
        );
        assert_eq!(iv, h("5b 78 92 3d ee 08 57 90 33 e5 23 d9").as_slice());
        let (key, iv) = traffic_key(suite, &ap.server);
        assert_eq!(
            key[..16],
            h("9f 02 28 3b 6c 9c 07 ef c2 6b b9 f2 ac 92 e3 56")
        );
        assert_eq!(iv, h("cf 78 2b 88 dd 83 54 9a ad f1 e9 84").as_slice());
    }

    /// SHA-384 schedule (TLS_AES_256_GCM_SHA384): the client handshake
    /// secret for ECDHE = 32 x 0x07 and transcript "transcript", computed
    /// step by step with `openssl kdf ... HKDF` (EXTRACT_ONLY/EXPAND_ONLY).
    #[test]
    fn sha384_schedule_matches_openssl() {
        let suite = Suite::Aes256GcmSha384;
        let (_, hs) = Schedule::new(suite, &[7; 32], b"transcript");
        assert_eq!(
            hs.client.as_bytes(),
            h(
                "2cabd7037487e49f11e70ec1ceb8ff226d95956a687b486be9f5af4f259177c3\
               689a835f258b1d18af7df67d5977b9b4"
            )
        );
        let next = next_traffic_secret(suite, &hs.client);
        assert_eq!(next.as_bytes().len(), 48);
        assert_ne!(next.as_bytes(), hs.client.as_bytes());
        assert_eq!(finished(suite, &hs.server, b"x").as_bytes().len(), 48);
        assert_eq!(Suite::from_id(0x1302), Some(suite));
        assert_eq!(Suite::from_id(0x1304), None);
    }
}
