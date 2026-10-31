// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Server authentication (ADR 0067): `rustls-webpki` builds and checks the
//! chain, with signatures verified by `aricode-crypto` (its ECDSA and RSA
//! verification measured faster than the alternatives, ADR 0066). On top,
//! this system's policy (`aricode-x509`):
//! - the time a chain is checked at when the clock cannot be trusted;
//! - keys distrusted anywhere in the chain (ADR 0054);
//! - no wildcard over a public suffix (ADR 0062).
//!
//! Names are matched by `rustls-webpki`, on subjectAltName only (ADR 0058).

use alloc::sync::Arc;
use alloc::vec::Vec;
use aricode_crypto::ecdsa::{self, Curve};
use aricode_crypto::{ed25519, rsa, Hash};
use aricode_sync::SpinLock;
use aricode_x509::{Error as CertError, ValidityTime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{
    alg_id, AlgorithmIdentifier, CertificateDer, InvalidSignature, ServerName,
    SignatureVerificationAlgorithm, TrustAnchor, UnixTime,
};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};

// --- Signature algorithms ---------------------------------------------

#[derive(Debug)]
struct EcdsaAlg {
    curve: Curve,
    hash: Hash,
    public_key: AlgorithmIdentifier,
    signature: AlgorithmIdentifier,
}

impl SignatureVerificationAlgorithm for EcdsaAlg {
    fn verify_signature(&self, key: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), InvalidSignature> {
        let mut digest = [0u8; 64];
        let n = self.hash.digest(msg, &mut digest);
        let (r, s) = ecdsa::parse_der_signature(sig).ok_or(InvalidSignature)?;
        ecdsa::verify(self.curve, key, &digest[..n], r, s)
            .then_some(())
            .ok_or(InvalidSignature)
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        self.public_key
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.signature
    }
}

#[derive(Debug)]
struct RsaAlg {
    hash: Hash,
    pss: bool,
    signature: AlgorithmIdentifier,
}

/// RSA moduli accepted, in bits (as rustls-webpki's own providers).
const RSA_BITS: core::ops::RangeInclusive<usize> = 2048..=8192;

impl SignatureVerificationAlgorithm for RsaAlg {
    fn verify_signature(&self, key: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), InvalidSignature> {
        let (n, e) = rsa_public_key(key).ok_or(InvalidSignature)?;
        let bits = match n.first() {
            Some(&top) if top != 0 => n.len() * 8 - top.leading_zeros() as usize,
            _ => return Err(InvalidSignature),
        };
        if !RSA_BITS.contains(&bits) || !acceptable_exponent(e) {
            return Err(InvalidSignature);
        }
        let mut digest = [0u8; 64];
        let len = self.hash.digest(msg, &mut digest);
        let ok = if self.pss {
            rsa::verify_pss(n, e, self.hash, &digest[..len], sig)
        } else {
            rsa::verify_pkcs1v15(n, e, self.hash, &digest[..len], sig)
        };
        ok.then_some(()).ok_or(InvalidSignature)
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.signature
    }
}

/// An odd public exponent of at least 3 and at most 33 bits, as `ring`
/// requires: with e = 1 or an even e, signatures could be forged
/// (audit of ADR 0067, L3).
fn acceptable_exponent(e: &[u8]) -> bool {
    let value = match e {
        [] => return false,
        e if e.len() > 5 => return false,
        e => e.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b)),
    };
    value >= 3 && value & 1 == 1 && value < 1 << 33
}

/// `RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }`
/// (RFC 8017 A.1.1), each without its sign byte.
fn rsa_public_key(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let (seq, rest) = der_element(der, 0x30)?;
    if !rest.is_empty() {
        return None;
    }
    let (n, seq) = der_element(seq, 0x02)?;
    let (e, rest) = der_element(seq, 0x02)?;
    if !rest.is_empty() {
        return None;
    }
    let unsigned = |i: &[u8]| -> Option<usize> {
        // A positive INTEGER: one zero byte only before a high bit.
        match i {
            [] => None,
            [0, b, ..] if b & 0x80 == 0 => None,
            [0, ..] => Some(1),
            [b, ..] if b & 0x80 != 0 => None,
            _ => Some(0),
        }
    };
    Some((&n[unsigned(n)?..], &e[unsigned(e)?..]))
}

/// The value of the DER element with `tag` at the start of `der`, and what
/// follows it. Short and long (up to 3 bytes) definite lengths, minimal.
fn der_element(der: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&t, rest) = der.split_first()?;
    let (&first, rest) = rest.split_first()?;
    if t != tag {
        return None;
    }
    let (len, rest) = match first {
        n @ 0..=0x7F => (n as usize, rest),
        0x81..=0x83 => {
            let k = (first & 0x7F) as usize;
            let bytes = rest.get(..k)?;
            if bytes[0] == 0 || (k == 1 && bytes[0] < 0x80) {
                return None;
            }
            let len = bytes.iter().fold(0usize, |a, &b| (a << 8) | b as usize);
            (len, &rest[k..])
        }
        _ => return None,
    };
    (len <= rest.len()).then(|| rest.split_at(len))
}

#[derive(Debug)]
struct Ed25519Alg;

impl SignatureVerificationAlgorithm for Ed25519Alg {
    fn verify_signature(&self, key: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), InvalidSignature> {
        let key: &[u8; 32] = key.try_into().map_err(|_| InvalidSignature)?;
        let sig: &[u8; 64] = sig.try_into().map_err(|_| InvalidSignature)?;
        ed25519::verify(key, msg, sig)
            .then_some(())
            .ok_or(InvalidSignature)
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::ED25519
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::ED25519
    }
}

macro_rules! ecdsa_alg {
    ($name:ident, $curve:ident, $hash:ident, $pk:ident, $sig:ident) => {
        static $name: EcdsaAlg = EcdsaAlg {
            curve: Curve::$curve,
            hash: Hash::$hash,
            public_key: alg_id::$pk,
            signature: alg_id::$sig,
        };
    };
}

ecdsa_alg!(ECDSA_P256_SHA256, P256, Sha256, ECDSA_P256, ECDSA_SHA256);
ecdsa_alg!(ECDSA_P256_SHA384, P256, Sha384, ECDSA_P256, ECDSA_SHA384);
ecdsa_alg!(ECDSA_P384_SHA256, P384, Sha256, ECDSA_P384, ECDSA_SHA256);
ecdsa_alg!(ECDSA_P384_SHA384, P384, Sha384, ECDSA_P384, ECDSA_SHA384);

macro_rules! rsa_alg {
    ($name:ident, $hash:ident, $pss:expr, $sig:ident) => {
        static $name: RsaAlg = RsaAlg {
            hash: Hash::$hash,
            pss: $pss,
            signature: alg_id::$sig,
        };
    };
}

rsa_alg!(RSA_PKCS1_SHA256, Sha256, false, RSA_PKCS1_SHA256);
rsa_alg!(RSA_PKCS1_SHA384, Sha384, false, RSA_PKCS1_SHA384);
rsa_alg!(RSA_PKCS1_SHA512, Sha512, false, RSA_PKCS1_SHA512);
rsa_alg!(RSA_PSS_SHA256, Sha256, true, RSA_PSS_SHA256);
rsa_alg!(RSA_PSS_SHA384, Sha384, true, RSA_PSS_SHA384);
rsa_alg!(RSA_PSS_SHA512, Sha512, true, RSA_PSS_SHA512);
static ED25519: Ed25519Alg = Ed25519Alg;

/// Every algorithm, for chains, and which serve each handshake signature
/// scheme, in preference order. SHA-1 and P-521 are not accepted.
pub static ALGORITHMS: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        &ECDSA_P256_SHA256,
        &ECDSA_P256_SHA384,
        &ECDSA_P384_SHA256,
        &ECDSA_P384_SHA384,
        &ED25519,
        &RSA_PSS_SHA256,
        &RSA_PSS_SHA384,
        &RSA_PSS_SHA512,
        &RSA_PKCS1_SHA256,
        &RSA_PKCS1_SHA384,
        &RSA_PKCS1_SHA512,
    ],
    mapping: &[
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[&ECDSA_P384_SHA384, &ECDSA_P256_SHA384],
        ),
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[&ECDSA_P256_SHA256, &ECDSA_P384_SHA256],
        ),
        (SignatureScheme::ED25519, &[&ED25519]),
        (SignatureScheme::RSA_PSS_SHA512, &[&RSA_PSS_SHA512]),
        (SignatureScheme::RSA_PSS_SHA384, &[&RSA_PSS_SHA384]),
        (SignatureScheme::RSA_PSS_SHA256, &[&RSA_PSS_SHA256]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[&RSA_PKCS1_SHA512]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[&RSA_PKCS1_SHA384]),
        (SignatureScheme::RSA_PKCS1_SHA256, &[&RSA_PKCS1_SHA256]),
    ],
};

// --- The server certificate verifier ----------------------------------

/// Verifies the server's chain and handshake signatures. Remembers why it
/// last refused a chain, so the client can say so ([`CertError`]).
pub struct Verifier {
    anchors: Vec<TrustAnchor<'static>>,
    time: ValidityTime,
    refused: SpinLock<Option<CertError>>,
}

impl core::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Verifier")
            .field("anchors", &self.anchors.len())
            .field("time", &self.time)
            .finish()
    }
}

impl Verifier {
    pub fn new(anchors: Vec<TrustAnchor<'static>>, time: ValidityTime) -> Arc<Self> {
        Arc::new(Self {
            anchors,
            time,
            refused: SpinLock::new(None),
        })
    }

    /// Why the last chain was refused, if it was.
    pub fn refusal(&self) -> Option<CertError> {
        *self.refused.lock()
    }

    fn refuse(&self, why: CertError) -> Error {
        *self.refused.lock() = Some(why);
        Error::InvalidCertificate(match why {
            CertError::Expired => CertificateError::Expired,
            CertError::NotYetValid => CertificateError::NotValidYet,
            CertError::UnknownIssuer => CertificateError::UnknownIssuer,
            CertError::BadSignature => CertificateError::BadSignature,
            CertError::NameMismatch | CertError::WildcardOverSuffix => {
                CertificateError::NotValidForName
            }
            CertError::WrongPurpose => CertificateError::InvalidPurpose,
            CertError::Malformed => CertificateError::BadEncoding,
            _ => CertificateError::ApplicationVerificationFailure,
        })
    }

    /// The chain at the policy's time. Without a trusted clock only
    /// `notAfter >= floor` is required (audit of incident 0036, H1): a
    /// certificate issued after the floor is checked at its own
    /// `notBefore` instead, so a renewal after the release is not refused.
    fn verify_chain<'a>(
        &'a self,
        leaf: &'a webpki::EndEntityCert<'a>,
        intermediates: &'a [CertificateDer<'a>],
    ) -> Result<webpki::VerifiedPath<'a>, webpki::Error> {
        let (mut at, floor) = match self.time {
            ValidityTime::Clock(t) => (t, false),
            ValidityTime::AtLeast(f) => (f, true),
        };
        // A path through a distrusted key (ADR 0054) is rejected while the
        // path is built, so `rustls-webpki` goes on to try other paths
        // (audit of ADR 0067, L4).
        let no_distrusted_key = |path: &webpki::VerifiedPath<'_>| {
            let distrusted =
                aricode_x509::is_distrusted(path.end_entity().subject_public_key_info().as_ref())
                    || path
                        .intermediate_certificates()
                        .any(|c| aricode_x509::is_distrusted(c.subject_public_key_info().as_ref()))
                    || aricode_x509::is_distrusted(&anchor_spki(path.anchor()));
            match distrusted {
                true => Err(webpki::Error::CertDistrusted),
                false => Ok(()),
            }
        };
        // Each round moves `at` forward to a later `notBefore`; a chain of
        // n certificates needs at most n.
        for _ in 0..=intermediates.len() + 1 {
            let r = leaf.verify_for_usage(
                ALGORITHMS.all,
                &self.anchors,
                intermediates,
                UnixTime::since_unix_epoch(core::time::Duration::from_secs(at)),
                webpki::KeyUsage::server_auth(),
                None,
                Some(&no_distrusted_key),
            );
            match r {
                Err(webpki::Error::CertNotValidYet { not_before, .. })
                    if floor && not_before.as_secs() > at =>
                {
                    at = not_before.as_secs();
                }
                r => return r,
            }
        }
        Err(webpki::Error::CertNotValidYet {
            time: UnixTime::since_unix_epoch(core::time::Duration::from_secs(at)),
            not_before: UnixTime::since_unix_epoch(core::time::Duration::from_secs(at)),
        })
    }
}

/// `rustls-webpki`'s reasons, as this system words them. Its deprecated
/// variants are still matched: older paths may return them.
#[allow(deprecated)]
fn reason(e: &webpki::Error) -> CertError {
    use webpki::Error as W;
    match e {
        W::CertExpired { .. } => CertError::Expired,
        W::CertNotValidYet { .. } => CertError::NotYetValid,
        W::UnknownIssuer => CertError::UnknownIssuer,
        W::CertNotValidForName(_) => CertError::NameMismatch,
        W::InvalidSignatureForPublicKey
        | W::UnsupportedSignatureAlgorithm
        | W::UnsupportedSignatureAlgorithmContext(_)
        | W::UnsupportedSignatureAlgorithmForPublicKey
        | W::UnsupportedSignatureAlgorithmForPublicKeyContext(_) => CertError::BadSignature,
        W::RequiredEkuNotFound | W::RequiredEkuNotFoundContext(_) => CertError::WrongPurpose,
        W::CaUsedAsEndEntity | W::EndEntityUsedAsCa => CertError::NotCa,
        W::NameConstraintViolation => CertError::NameConstraint,
        W::MaximumPathDepthExceeded | W::MaximumPathBuildCallsExceeded => CertError::ChainTooLong,
        W::UnsupportedCertVersion | W::UnsupportedCriticalExtension => CertError::Unsupported,
        W::CertDistrusted => CertError::Distrusted,
        _ => CertError::Malformed,
    }
}

/// The whole SubjectPublicKeyInfo of a trust anchor, whose field holds
/// only the SEQUENCE's contents.
fn anchor_spki(anchor: &TrustAnchor<'_>) -> Vec<u8> {
    let body = anchor.subject_public_key_info.as_ref();
    let mut der = Vec::with_capacity(body.len() + 4);
    der.push(0x30);
    match body.len() {
        n @ 0..=0x7F => der.push(n as u8),
        n @ 0x80..=0xFF => der.extend_from_slice(&[0x81, n as u8]),
        n => der.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]),
    }
    der.extend_from_slice(body);
    der
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let leaf =
            webpki::EndEntityCert::try_from(end_entity).map_err(|e| self.refuse(reason(&e)))?;
        self.verify_chain(&leaf, intermediates)
            .map_err(|e| self.refuse(reason(&e)))?;
        leaf.verify_is_valid_for_subject_name(server_name)
            .map_err(|e| self.refuse(reason(&e)))?;
        if let ServerName::DnsName(host) = server_name {
            if aricode_x509::wildcard_over_public_suffix(host.as_ref(), leaf.valid_dns_names()) {
                return Err(self.refuse(CertError::WildcardOverSuffix));
            }
        }
        *self.refused.lock() = None;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &ALGORITHMS)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &ALGORITHMS)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ALGORITHMS.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsa_public_keys_parse_strictly() {
        // SEQUENCE { INTEGER 0x00 0x80.., INTEGER 0x01 0x00 0x01 }
        let mut der =
            alloc::vec![0x30, 0x0A, 0x02, 0x03, 0x00, 0x80, 0x01, 0x02, 0x03, 0x01, 0x00, 0x01];
        assert_eq!(
            rsa_public_key(&der),
            Some((&[0x80, 0x01][..], &[0x01, 0x00, 0x01][..]))
        );
        // A needless leading zero, a negative modulus, trailing bytes.
        der[4..6].copy_from_slice(&[0x00, 0x01]);
        assert_eq!(rsa_public_key(&der), None);
        der[4..6].copy_from_slice(&[0x80, 0x01]);
        assert_eq!(rsa_public_key(&der), None);
        let mut long = alloc::vec![
            0x30, 0x0A, 0x02, 0x03, 0x00, 0x80, 0x01, 0x02, 0x03, 0x01, 0x00, 0x01, 0x00
        ];
        assert_eq!(rsa_public_key(&long), None);
        long.truncate(11);
        assert_eq!(rsa_public_key(&long), None);
    }

    /// Audit of ADR 0067, L3: e = 1 and even exponents forge signatures.
    #[test]
    fn rsa_exponents() {
        assert!(acceptable_exponent(&[0x01, 0x00, 0x01]));
        assert!(acceptable_exponent(&[3]));
        assert!(!acceptable_exponent(&[1]));
        assert!(!acceptable_exponent(&[4]));
        assert!(!acceptable_exponent(&[]));
        assert!(!acceptable_exponent(&[0x02, 0, 0, 0, 1]));
        assert!(acceptable_exponent(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF]));
        assert!(!acceptable_exponent(&[1, 0, 0, 0, 0, 1]));
    }

    #[test]
    fn anchor_spki_rebuilds_the_sequence() {
        for len in [5usize, 127, 128, 255, 256, 600] {
            let body = alloc::vec![7u8; len];
            let anchor = TrustAnchor {
                subject: (&[][..]).into(),
                subject_public_key_info: (&body[..]).into(),
                name_constraints: None,
            };
            let der = anchor_spki(&anchor);
            let (inner, rest) = der_element(&der, 0x30).expect("one element");
            assert!(rest.is_empty());
            assert_eq!(inner, &body[..]);
        }
    }
}
