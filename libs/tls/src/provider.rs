// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The cryptography rustls runs on (ADR 0067): `aricode-crypto` for hashes,
//! HMAC (and through it HKDF and the TLS 1.2 PRF), the AEADs, X25519 and
//! the generator; RustCrypto's `p256` and `p384` for the NIST key
//! exchanges, which `aricode-crypto` does not do (it verifies on those
//! curves, without constant time). Laid out as rustls's own providers.

use alloc::boxed::Box;
use alloc::vec::Vec;
use aricode_crypto::aead::AesGcm;
use aricode_crypto::chacha20::Rng;
use aricode_crypto::sha256::Sha256;
use aricode_crypto::sha512::Sha384;
use aricode_crypto::{aead, hmac, sha256, sha512, x25519, SecretBytes};
use aricode_sync::SpinLock;
use rustls::crypto::cipher::{
    make_tls12_aad, make_tls13_aad, AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv,
    KeyBlockShape, MessageDecrypter, MessageEncrypter, Nonce, OutboundOpaqueMessage,
    OutboundPlainMessage, PrefixedPayload, Tls12AeadAlgorithm, Tls13AeadAlgorithm,
    UnsupportedOperationError,
};
use rustls::crypto::tls12::PrfUsingHmac;
use rustls::crypto::tls13::HkdfUsingHmac;
use rustls::crypto::{
    hash, hmac as rhmac, ActiveKeyExchange, CipherSuiteCommon, CryptoProvider, GetRandomFailed,
    KeyExchangeAlgorithm, KeyProvider, SecureRandom, SharedSecret, SupportedKxGroup,
};
use rustls::pki_types::PrivateKeyDer;
use rustls::{
    CipherSuite, ConnectionTrafficSecrets, ContentType, Error, NamedGroup, PeerMisbehaved,
    ProtocolVersion, SignatureScheme, SupportedCipherSuite, Tls12CipherSuite, Tls13CipherSuite,
};

const TAG_LEN: usize = aead::TAG_LEN;
/// Largest plaintext of a TLS record (RFC 8446 section 5.1).
const MAX_FRAGMENT_LEN: usize = 16384;

/// The provider: TLS 1.3 and TLS 1.2 (ECDHE with AEADs only, rustls's
/// subset) suites, X25519 first, then P-256 and P-384.
pub(crate) fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: Vec::from(SUITES),
        kx_groups: Vec::from(KX_GROUPS),
        signature_verification_algorithms: crate::verify::ALGORITHMS,
        secure_random: &RANDOM,
        key_provider: &NoKeys,
    }
}

static SUITES: &[SupportedCipherSuite] = &[
    SupportedCipherSuite::Tls13(&TLS13_AES_128_GCM_SHA256),
    SupportedCipherSuite::Tls13(&TLS13_AES_256_GCM_SHA384),
    SupportedCipherSuite::Tls13(&TLS13_CHACHA20_POLY1305_SHA256),
    SupportedCipherSuite::Tls12(&TLS12_ECDSA_AES_128_GCM),
    SupportedCipherSuite::Tls12(&TLS12_ECDSA_AES_256_GCM),
    SupportedCipherSuite::Tls12(&TLS12_ECDSA_CHACHA20),
    SupportedCipherSuite::Tls12(&TLS12_RSA_AES_128_GCM),
    SupportedCipherSuite::Tls12(&TLS12_RSA_AES_256_GCM),
    SupportedCipherSuite::Tls12(&TLS12_RSA_CHACHA20),
];

static KX_GROUPS: &[&dyn SupportedKxGroup] = &[&X25519, &P256, &P384];

// --- Hashes and HMAC --------------------------------------------------

struct Sha256Hash;
struct Sha384Hash;
static SHA256: Sha256Hash = Sha256Hash;
static SHA384: Sha384Hash = Sha384Hash;

impl hash::Hash for Sha256Hash {
    fn start(&self) -> Box<dyn hash::Context> {
        Box::new(Sha256Context(Sha256::new()))
    }
    fn hash(&self, data: &[u8]) -> hash::Output {
        hash::Output::new(&sha256::sha256(data))
    }
    fn output_len(&self) -> usize {
        sha256::DIGEST_LEN
    }
    fn algorithm(&self) -> hash::HashAlgorithm {
        hash::HashAlgorithm::SHA256
    }
}

impl hash::Hash for Sha384Hash {
    fn start(&self) -> Box<dyn hash::Context> {
        Box::new(Sha384Context(Sha384::new()))
    }
    fn hash(&self, data: &[u8]) -> hash::Output {
        hash::Output::new(&sha512::sha384(data))
    }
    fn output_len(&self) -> usize {
        sha512::SHA384_DIGEST_LEN
    }
    fn algorithm(&self) -> hash::HashAlgorithm {
        hash::HashAlgorithm::SHA384
    }
}

struct Sha256Context(Sha256);
struct Sha384Context(Sha384);

impl hash::Context for Sha256Context {
    fn fork_finish(&self) -> hash::Output {
        hash::Output::new(&self.0.clone().finish())
    }
    fn fork(&self) -> Box<dyn hash::Context> {
        Box::new(Self(self.0.clone()))
    }
    fn finish(self: Box<Self>) -> hash::Output {
        hash::Output::new(&self.0.finish())
    }
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}

impl hash::Context for Sha384Context {
    fn fork_finish(&self) -> hash::Output {
        hash::Output::new(&self.0.clone().finish())
    }
    fn fork(&self) -> Box<dyn hash::Context> {
        Box::new(Self(self.0.clone()))
    }
    fn finish(self: Box<Self>) -> hash::Output {
        hash::Output::new(&self.0.finish())
    }
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}

struct HmacSha256;
struct HmacSha384;
static HMAC_SHA256: HmacSha256 = HmacSha256;
static HMAC_SHA384: HmacSha384 = HmacSha384;

/// An HMAC key, wiped when dropped.
struct HmacKey {
    key: Vec<u8>,
    sign: fn(&[u8], &[&[u8]]) -> rhmac::Tag,
    len: usize,
}

impl Drop for HmacKey {
    fn drop(&mut self) {
        aricode_crypto::wipe(&mut self.key);
    }
}

impl rhmac::Key for HmacKey {
    fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> rhmac::Tag {
        let mut parts: Vec<&[u8]> = Vec::with_capacity(middle.len() + 2);
        parts.push(first);
        parts.extend_from_slice(middle);
        parts.push(last);
        (self.sign)(&self.key, &parts)
    }
    fn tag_len(&self) -> usize {
        self.len
    }
}

impl rhmac::Hmac for HmacSha256 {
    fn with_key(&self, key: &[u8]) -> Box<dyn rhmac::Key> {
        Box::new(HmacKey {
            key: key.to_vec(),
            sign: |k, parts| rhmac::Tag::new(&hmac::hmac_sha256(k, parts)),
            len: 32,
        })
    }
    fn hash_output_len(&self) -> usize {
        32
    }
}

impl rhmac::Hmac for HmacSha384 {
    fn with_key(&self, key: &[u8]) -> Box<dyn rhmac::Key> {
        Box::new(HmacKey {
            key: key.to_vec(),
            sign: |k, parts| rhmac::Tag::new(&hmac::hmac_sha384(k, parts)),
            len: 48,
        })
    }
    fn hash_output_len(&self) -> usize {
        48
    }
}

// --- AEADs ------------------------------------------------------------

/// An AEAD key ready to seal and open in place with a 12-byte nonce.
enum AeadCipher {
    Gcm(Box<AesGcm>),
    ChaCha(SecretBytes<32>),
}

impl AeadCipher {
    fn gcm(key: &AeadKey) -> Self {
        // `key_len()` and `key_block_shape()` give rustls the lengths, so
        // the key is always 16 or 32 bytes.
        Self::Gcm(Box::new(
            AesGcm::new(key.as_ref()).expect("AES-GCM key of 16 or 32 bytes"),
        ))
    }

    fn chacha(key: &AeadKey) -> Self {
        let mut k = SecretBytes([0u8; 32]);
        k.0.copy_from_slice(key.as_ref());
        Self::ChaCha(k)
    }

    fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; TAG_LEN] {
        match self {
            Self::Gcm(g) => g.seal(nonce, aad, data),
            Self::ChaCha(k) => aead::chacha20poly1305_seal(&k.0, nonce, aad, data),
        }
    }

    /// Opens `data` (ciphertext then tag) in place; `Some(plaintext length)`.
    fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> Option<usize> {
        let ct_len = data.len().checked_sub(TAG_LEN)?;
        let (ct, tag) = data.split_at_mut(ct_len);
        let tag: [u8; TAG_LEN] = (&*tag).try_into().ok()?;
        let ok = match self {
            Self::Gcm(g) => g.open(nonce, aad, ct, &tag),
            Self::ChaCha(k) => aead::chacha20poly1305_open(&k.0, nonce, aad, ct, &tag),
        };
        ok.then_some(ct_len)
    }
}

/// The three AEADs, for both versions.
#[derive(Clone, Copy)]
enum AeadKind {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

impl AeadKind {
    fn key_len(self) -> usize {
        match self {
            Self::Aes128Gcm => 16,
            Self::Aes256Gcm | Self::ChaCha20Poly1305 => 32,
        }
    }

    fn cipher(self, key: &AeadKey) -> AeadCipher {
        match self {
            Self::ChaCha20Poly1305 => AeadCipher::chacha(key),
            _ => AeadCipher::gcm(key),
        }
    }

    fn secrets(self, key: AeadKey, iv: Iv) -> ConnectionTrafficSecrets {
        match self {
            Self::Aes128Gcm => ConnectionTrafficSecrets::Aes128Gcm { key, iv },
            Self::Aes256Gcm => ConnectionTrafficSecrets::Aes256Gcm { key, iv },
            Self::ChaCha20Poly1305 => ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv },
        }
    }
}

struct Tls13Aead(AeadKind);

impl Tls13AeadAlgorithm for Tls13Aead {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Box::new(Tls13Encrypter {
            cipher: self.0.cipher(&key),
            iv,
        })
    }
    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Box::new(Tls13Decrypter {
            cipher: self.0.cipher(&key),
            iv,
        })
    }
    fn key_len(&self) -> usize {
        self.0.key_len()
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(self.0.secrets(key, iv))
    }
}

struct Tls13Encrypter {
    cipher: AeadCipher,
    iv: Iv,
}

struct Tls13Decrypter {
    cipher: AeadCipher,
    iv: Iv,
}

impl MessageEncrypter for Tls13Encrypter {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total_len = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        payload.extend_from_chunks(&msg.payload);
        payload.extend_from_slice(&msg.typ.to_array());
        let nonce = Nonce::new(&self.iv, seq).0;
        let tag = self
            .cipher
            .seal(&nonce, &make_tls13_aad(total_len), payload.as_mut());
        payload.extend_from_slice(&tag);
        // TLS 1.3 records carry the TLS 1.2 version (RFC 8446 section 5.1).
        Ok(OutboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            payload,
        ))
    }
    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + 1 + TAG_LEN
    }
}

impl MessageDecrypter for Tls13Decrypter {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls13_aad(payload.len());
        let plain = self
            .cipher
            .open(&nonce, &aad, payload)
            .ok_or(Error::DecryptError)?;
        payload.truncate(plain);
        msg.into_tls13_unpadded_message()
    }
}

/// TLS 1.2 AES-GCM: a 4-byte implicit salt and an 8-byte explicit nonce
/// sent before each record (RFC 5288); TLS 1.2 ChaCha20-Poly1305: the
/// TLS 1.3 nonce construction (RFC 7905).
struct Tls12Aead(AeadKind);

impl Tls12AeadAlgorithm for Tls12Aead {
    fn encrypter(&self, key: AeadKey, iv: &[u8], explicit: &[u8]) -> Box<dyn MessageEncrypter> {
        let cipher = self.0.cipher(&key);
        match self.0 {
            AeadKind::ChaCha20Poly1305 => Box::new(Tls12ChaChaEncrypter {
                cipher,
                iv: Iv::copy(iv),
            }),
            _ => Box::new(Tls12GcmEncrypter {
                cipher,
                iv: gcm_iv(iv, explicit),
            }),
        }
    }
    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        let cipher = self.0.cipher(&key);
        match self.0 {
            AeadKind::ChaCha20Poly1305 => Box::new(Tls12ChaChaDecrypter {
                cipher,
                iv: Iv::copy(iv),
            }),
            _ => {
                let mut salt = [0u8; 4];
                salt.copy_from_slice(iv);
                Box::new(Tls12GcmDecrypter { cipher, salt })
            }
        }
    }
    fn key_block_shape(&self) -> KeyBlockShape {
        match self.0 {
            AeadKind::ChaCha20Poly1305 => KeyBlockShape {
                enc_key_len: 32,
                fixed_iv_len: 12,
                explicit_nonce_len: 0,
            },
            kind => KeyBlockShape {
                enc_key_len: kind.key_len(),
                fixed_iv_len: 4,
                explicit_nonce_len: 8,
            },
        }
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        explicit: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        let iv = match self.0 {
            AeadKind::ChaCha20Poly1305 => Iv::copy(iv),
            _ => gcm_iv(iv, explicit),
        };
        Ok(self.0.secrets(key, iv))
    }
}

/// The GCM nonce: the 4-byte salt, then 8 bytes rustls's providers derive
/// from the key block and XOR with the sequence number.
fn gcm_iv(salt: &[u8], explicit: &[u8]) -> Iv {
    let mut iv = [0u8; 12];
    iv[..4].copy_from_slice(salt);
    iv[4..].copy_from_slice(explicit);
    Iv::new(iv)
}

const GCM_EXPLICIT_NONCE_LEN: usize = 8;

struct Tls12GcmEncrypter {
    cipher: AeadCipher,
    iv: Iv,
}

struct Tls12GcmDecrypter {
    cipher: AeadCipher,
    salt: [u8; 4],
}

impl MessageEncrypter for Tls12GcmEncrypter {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total_len = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_slice(&nonce[4..]);
        payload.extend_from_chunks(&msg.payload);
        let tag = self.cipher.seal(
            &nonce,
            &aad,
            &mut payload.as_mut()[GCM_EXPLICIT_NONCE_LEN..],
        );
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }
    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + GCM_EXPLICIT_NONCE_LEN + TAG_LEN
    }
}

impl MessageDecrypter for Tls12GcmDecrypter {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        if msg.payload.len() < GCM_EXPLICIT_NONCE_LEN + TAG_LEN {
            return Err(Error::DecryptError);
        }
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&self.salt);
        nonce[4..].copy_from_slice(&msg.payload[..GCM_EXPLICIT_NONCE_LEN]);
        let plain_len = msg.payload.len() - GCM_EXPLICIT_NONCE_LEN - TAG_LEN;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, plain_len);
        let payload = &mut msg.payload;
        self.cipher
            .open(&nonce, &aad, &mut payload[GCM_EXPLICIT_NONCE_LEN..])
            .ok_or(Error::DecryptError)?;
        if plain_len > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        // The plaintext follows the explicit nonce: move it to the front.
        payload.copy_within(
            GCM_EXPLICIT_NONCE_LEN..GCM_EXPLICIT_NONCE_LEN + plain_len,
            0,
        );
        payload.truncate(plain_len);
        Ok(msg.into_plain_message())
    }
}

struct Tls12ChaChaEncrypter {
    cipher: AeadCipher,
    iv: Iv,
}

struct Tls12ChaChaDecrypter {
    cipher: AeadCipher,
    iv: Iv,
}

impl MessageEncrypter for Tls12ChaChaEncrypter {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total_len = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_chunks(&msg.payload);
        let tag = self.cipher.seal(&nonce, &aad, payload.as_mut());
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }
    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + TAG_LEN
    }
}

impl MessageDecrypter for Tls12ChaChaDecrypter {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let plain_len = msg
            .payload
            .len()
            .checked_sub(TAG_LEN)
            .ok_or(Error::DecryptError)?;
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, plain_len);
        let payload = &mut msg.payload;
        self.cipher
            .open(&nonce, &aad, payload)
            .ok_or(Error::DecryptError)?;
        if plain_len > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        payload.truncate(plain_len);
        Ok(msg.into_plain_message())
    }
}

// --- Cipher suites ----------------------------------------------------

static AES128_13: Tls13Aead = Tls13Aead(AeadKind::Aes128Gcm);
static AES256_13: Tls13Aead = Tls13Aead(AeadKind::Aes256Gcm);
static CHACHA_13: Tls13Aead = Tls13Aead(AeadKind::ChaCha20Poly1305);
static AES128_12: Tls12Aead = Tls12Aead(AeadKind::Aes128Gcm);
static AES256_12: Tls12Aead = Tls12Aead(AeadKind::Aes256Gcm);
static CHACHA_12: Tls12Aead = Tls12Aead(AeadKind::ChaCha20Poly1305);
static HKDF_SHA256: HkdfUsingHmac = HkdfUsingHmac(&HMAC_SHA256);
static HKDF_SHA384: HkdfUsingHmac = HkdfUsingHmac(&HMAC_SHA384);
static PRF_SHA256: PrfUsingHmac = PrfUsingHmac(&HMAC_SHA256);
static PRF_SHA384: PrfUsingHmac = PrfUsingHmac(&HMAC_SHA384);

/// Records an AES-GCM key may seal before rustls renews it (as its own
/// providers set; RFC 8446 section 5.5).
const GCM_LIMIT: u64 = 1 << 24;

static TLS13_AES_128_GCM_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
        hash_provider: &SHA256,
        confidentiality_limit: GCM_LIMIT,
    },
    hkdf_provider: &HKDF_SHA256,
    aead_alg: &AES128_13,
    quic: None,
};

static TLS13_AES_256_GCM_SHA384: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_256_GCM_SHA384,
        hash_provider: &SHA384,
        confidentiality_limit: GCM_LIMIT,
    },
    hkdf_provider: &HKDF_SHA384,
    aead_alg: &AES256_13,
    quic: None,
};

static TLS13_CHACHA20_POLY1305_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
        hash_provider: &SHA256,
        confidentiality_limit: u64::MAX,
    },
    hkdf_provider: &HKDF_SHA256,
    aead_alg: &CHACHA_13,
    quic: None,
};

/// Server signatures each TLS 1.2 suite family accepts.
static ECDSA_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::ED25519,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ECDSA_NISTP256_SHA256,
];
static RSA_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA256,
];

macro_rules! tls12_suite {
    ($name:ident, $suite:ident, $hash:expr, $limit:expr, $sign:expr, $aead:expr, $prf:expr) => {
        static $name: Tls12CipherSuite = Tls12CipherSuite {
            common: CipherSuiteCommon {
                suite: CipherSuite::$suite,
                hash_provider: $hash,
                confidentiality_limit: $limit,
            },
            kx: KeyExchangeAlgorithm::ECDHE,
            sign: $sign,
            aead_alg: $aead,
            prf_provider: $prf,
        };
    };
}

tls12_suite!(
    TLS12_ECDSA_AES_128_GCM,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    &SHA256,
    GCM_LIMIT,
    ECDSA_SCHEMES,
    &AES128_12,
    &PRF_SHA256
);
tls12_suite!(
    TLS12_ECDSA_AES_256_GCM,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    &SHA384,
    GCM_LIMIT,
    ECDSA_SCHEMES,
    &AES256_12,
    &PRF_SHA384
);
tls12_suite!(
    TLS12_ECDSA_CHACHA20,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    &SHA256,
    u64::MAX,
    ECDSA_SCHEMES,
    &CHACHA_12,
    &PRF_SHA256
);
tls12_suite!(
    TLS12_RSA_AES_128_GCM,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    &SHA256,
    GCM_LIMIT,
    RSA_SCHEMES,
    &AES128_12,
    &PRF_SHA256
);
tls12_suite!(
    TLS12_RSA_AES_256_GCM,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    &SHA384,
    GCM_LIMIT,
    RSA_SCHEMES,
    &AES256_12,
    &PRF_SHA384
);
tls12_suite!(
    TLS12_RSA_CHACHA20,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    &SHA256,
    u64::MAX,
    RSA_SCHEMES,
    &CHACHA_12,
    &PRF_SHA256
);

// --- Key exchange -----------------------------------------------------

#[derive(Debug)]
struct X25519Group;
static X25519: X25519Group = X25519Group;

struct X25519Exchange {
    secret: SecretBytes<32>,
    public: [u8; 32],
}

impl SupportedKxGroup for X25519Group {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let mut secret = SecretBytes([0u8; 32]);
        RANDOM.fill(&mut secret.0)?;
        let public = x25519::public_key(&secret.0);
        Ok(Box::new(X25519Exchange { secret, public }))
    }
    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

impl ActiveKeyExchange for X25519Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        let peer: [u8; 32] = peer
            .try_into()
            .map_err(|_| Error::from(PeerMisbehaved::InvalidKeyShare))?;
        // An all-zero result (a low-order point) is refused (RFC 7748 6.1).
        let shared = x25519::shared(&self.secret.0, &peer)
            .ok_or(Error::from(PeerMisbehaved::InvalidKeyShare))?;
        let secret = SharedSecret::from(&shared[..]);
        let mut shared = shared;
        aricode_crypto::wipe(&mut shared);
        Ok(secret)
    }
    fn pub_key(&self) -> &[u8] {
        &self.public
    }
    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

/// P-256 and P-384 ECDHE (RustCrypto, constant time).
macro_rules! nist_group {
    ($group:ident, $exchange:ident, $static:ident, $curve:ident, $name:ident) => {
        #[derive(Debug)]
        struct $group;
        static $static: $group = $group;

        struct $exchange {
            secret: $curve::ecdh::EphemeralSecret,
            public: Vec<u8>,
        }

        impl SupportedKxGroup for $group {
            fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
                let secret = $curve::ecdh::EphemeralSecret::random(&mut RandomCore);
                let public = $curve::EncodedPoint::from(secret.public_key())
                    .as_bytes()
                    .to_vec();
                Ok(Box::new($exchange { secret, public }))
            }
            fn name(&self) -> NamedGroup {
                NamedGroup::$name
            }
        }

        impl ActiveKeyExchange for $exchange {
            fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
                // Uncompressed points only (RFC 8446 section 4.2.8.2);
                // `from_sec1_bytes` refuses a point off the curve or at
                // infinity.
                if peer.first() != Some(&0x04) || peer.len() != self.public.len() {
                    return Err(PeerMisbehaved::InvalidKeyShare.into());
                }
                let peer = $curve::PublicKey::from_sec1_bytes(peer)
                    .map_err(|_| Error::from(PeerMisbehaved::InvalidKeyShare))?;
                let shared = self.secret.diffie_hellman(&peer);
                Ok(SharedSecret::from(&shared.raw_secret_bytes()[..]))
            }
            fn pub_key(&self) -> &[u8] {
                &self.public
            }
            fn group(&self) -> NamedGroup {
                NamedGroup::$name
            }
        }
    };
}

nist_group!(P256Group, P256Exchange, P256, p256, secp256r1);
nist_group!(P384Group, P384Exchange, P384, p384, secp384r1);

// --- Randomness and keys ----------------------------------------------

/// The generator every connection draws from: `aricode-crypto`'s, seeded
/// and reseeded by [`crate::Client::new`] with the kernel's randomness.
static GENERATOR: SpinLock<Option<Rng>> = SpinLock::new(None);

/// Mixes 32 fresh random bytes into the generator (or seeds it).
pub(crate) fn reseed(seed: &[u8; 32]) {
    let mut g = GENERATOR.lock();
    match g.as_mut() {
        Some(rng) => rng.reseed(seed),
        None => *g = Some(Rng::new(*seed)),
    }
}

#[derive(Debug)]
struct Random;
static RANDOM: Random = Random;

impl SecureRandom for Random {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        match GENERATOR.lock().as_mut() {
            Some(rng) => {
                rng.fill(buf);
                Ok(())
            }
            None => Err(GetRandomFailed),
        }
    }
}

/// The generator as `rand_core` wants it, for the NIST key exchanges.
struct RandomCore;

impl rand_core::RngCore for RandomCore {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // A connection only exists after `Client::new` seeded it.
        RANDOM.fill(dest).expect("generator seeded by Client::new");
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for RandomCore {}

/// The client has no certificate of its own, so it loads no private key.
#[derive(Debug)]
struct NoKeys;

impl KeyProvider for NoKeys {
    fn load_private_key(
        &self,
        _key_der: PrivateKeyDer<'static>,
    ) -> Result<alloc::sync::Arc<dyn rustls::sign::SigningKey>, Error> {
        Err(Error::General(alloc::string::String::from(
            "this client loads no private keys",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit of ADR 0067, L6: NIST key shares must be uncompressed points
    /// (RFC 8446 section 4.2.8.2); an X25519 share must be 32 bytes.
    #[test]
    fn key_share_formats() {
        reseed(&[9; 32]);
        let peer = P256.start().unwrap();
        let uncompressed = peer.pub_key().to_vec();
        let mut compressed = uncompressed[..33].to_vec();
        compressed[0] = 0x02 | (uncompressed[64] & 1);
        assert!(P256.start().unwrap().complete(&compressed).is_err());
        assert!(P256.start().unwrap().complete(&uncompressed).is_ok());
        let p384 = P384.start().unwrap();
        assert!(P384.start().unwrap().complete(p384.pub_key()).is_ok());
        assert!(P384.start().unwrap().complete(&uncompressed).is_err());
        assert!(X25519.start().unwrap().complete(&[9; 31]).is_err());
        // A low-order point gives an all-zero secret, refused.
        assert!(X25519.start().unwrap().complete(&[0; 32]).is_err());
    }
}
