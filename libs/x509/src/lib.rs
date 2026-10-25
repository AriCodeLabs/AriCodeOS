// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! X.509 certificates for the TLS client: a DER parser for the subset of
//! RFC 5280 the web PKI uses, signature verification on top of
//! `aricode-crypto`, hostname matching (RFC 6125) and chain building up to
//! a set of trusted roots ([`roots::DEFAULT`] or the caller's own).
//!
//! Supported keys: RSA, ECDSA P-256/P-384 and Ed25519. Supported signatures:
//! RSA PKCS#1 v1.5 with SHA-256/384/512, RSA-PSS (SHA-256/384/512, salt
//! length = hash length), ECDSA with SHA-256/384 and Ed25519. SHA-1 is not
//! accepted. Not implemented: revocation (CRL/OCSP), name constraints
//! (certificates that mark them critical are rejected), extended key usage
//! and iPAddress subject alternative names.

#![no_std]

extern crate alloc;

mod cert;
mod der;
pub mod roots;

use alloc::vec;
use alloc::vec::Vec;
use aricode_crypto::ecdsa::{self, Curve};
use aricode_crypto::{ed25519, rsa, Hash};
use cert::Parsed;

/// Most intermediates accepted between the leaf and a trust anchor.
pub const MAX_INTERMEDIATES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Invalid DER or certificate structure.
    Malformed,
    /// Valid but outside the supported algorithms or extensions.
    Unsupported,
    Expired,
    NotYetValid,
    BadSignature,
    /// No trusted root or intermediate issued a certificate of the chain.
    UnknownIssuer,
    /// An issuer is not a CA or exceeds its path length constraint.
    NotCa,
    NameMismatch,
    ChainTooLong,
    /// The leaf may not authenticate a TLS server: extendedKeyUsage without
    /// serverAuth, or keyUsage without digitalSignature (RFC 5280 4.2.1.12,
    /// RFC 8446 4.4.2.2); or a CA of the chain may not issue for one: its
    /// extendedKeyUsage lacks serverAuth.
    WrongPurpose,
    /// A CA's nameConstraints do not allow a name of the leaf (RFC 5280
    /// 4.2.1.10).
    NameConstraint,
    /// A certificate of the chain, or the root it ends at, has a key this
    /// system no longer trusts (`roots::DISTRUSTED`, ADR 0054).
    Distrusted,
}

/// The dNSName subtrees of a CA's nameConstraints extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameConstraints<'a> {
    /// If not empty, every name must lie in one of these.
    pub permitted: Vec<&'a str>,
    /// No name may lie in any of these.
    pub excluded: Vec<&'a str>,
    /// The extension also has iPAddress subtrees, which this client does
    /// not evaluate: a host given as an IP address is refused under it.
    pub ip_subtrees: bool,
}

/// Name comparisons one CA's nameConstraints may cost (leaf names ×
/// subtrees): both are chosen by whoever built the chain, and constraints
/// are checked before the chain is trusted. Past it the chain is refused
/// (audit of incident 0029, L3; webpki uses the same kind of budget).
pub const NAME_CONSTRAINT_BUDGET: usize = 16_384;

impl NameConstraints<'_> {
    /// Whether the leaf name `name` (possibly a `*.` wildcard, which stands
    /// for any one label) is allowed: each name it can stand for lies in a
    /// permitted subtree, and none in an excluded one.
    pub fn allows(&self, name: &str) -> bool {
        let name = name.strip_suffix('.').unwrap_or(name);
        let (wildcard, base) = match name.strip_prefix("*.") {
            Some(base) => (true, base),
            None => (false, name),
        };
        // Every expansion `label.base` is inside a subtree iff `base` is.
        let permitted =
            self.permitted.is_empty() || self.permitted.iter().any(|p| within(base, p, wildcard));
        // An expansion can fall inside an excluded subtree if `base` is in
        // it, or (wildcard) if the subtree lies below `base` (only one label
        // below can actually match; deeper is refused too, to stay simple).
        let excluded = self
            .excluded
            .iter()
            .any(|e| within(base, e, wildcard) || (wildcard && strictly_below(trim(e), base)));
        permitted && !excluded
    }
}

/// A constraint without its trailing dot and its leading one (`.a.com`,
/// a common spelling for "the subdomains of `a.com`", which `within`
/// reads as such).
fn trim(constraint: &str) -> &str {
    let c = constraint.strip_suffix('.').unwrap_or(constraint);
    c.strip_prefix('.').unwrap_or(c)
}

/// Whether `name` lies in the subtree `constraint` (RFC 5280 4.2.1.10: the
/// constraint itself and any name formed by adding labels on its left;
/// the empty constraint holds every name). With a leading dot only the
/// added-label names. `wildcard`: `name` is the base of a `*.` name, whose
/// expansions all have one label more.
fn within(name: &str, constraint: &str, wildcard: bool) -> bool {
    let dot = constraint.starts_with('.');
    let c = trim(constraint);
    if c.is_empty() {
        return true;
    }
    if name.eq_ignore_ascii_case(c) {
        return wildcard || !dot;
    }
    strictly_below(name, c)
}

/// `name` ends with `.parent` (case-insensitive), with a label before it.
fn strictly_below(name: &str, parent: &str) -> bool {
    if parent.is_empty() {
        return !name.is_empty();
    }
    name.len() > parent.len() + 1
        && name.as_bytes()[name.len() - parent.len() - 1] == b'.'
        && name[name.len() - parent.len()..].eq_ignore_ascii_case(parent)
}

/// Subject public key; slices point into the certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicKey<'a> {
    /// Big-endian modulus and exponent without sign bytes.
    Rsa {
        n: &'a [u8],
        e: &'a [u8],
    },
    /// SEC1 uncompressed point (`0x04 || X || Y`).
    Ec {
        curve: Curve,
        point: &'a [u8],
    },
    Ed25519([u8; 32]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    RsaPkcs1Sha256,
    RsaPkcs1Sha384,
    RsaPkcs1Sha512,
    EcdsaSha256,
    EcdsaSha384,
    Ed25519,
    /// RSASSA-PSS with MGF1 over the same hash and salt length = hash length.
    RsaPss(Hash),
}

#[derive(Debug, Clone)]
pub struct Certificate<'a> {
    /// The signed `tbsCertificate` (whole DER element).
    pub tbs: &'a [u8],
    pub signature_algorithm: SignatureAlgorithm,
    pub signature: &'a [u8],
    /// Issuer and subject Names (whole DER elements, compared bytewise).
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    /// Validity period in Unix seconds (inclusive).
    pub not_before: u64,
    pub not_after: u64,
    pub public_key: PublicKey<'a>,
    /// The whole SubjectPublicKeyInfo (DER element): what
    /// `roots::DISTRUSTED` names, by SHA-256.
    pub spki: &'a [u8],
    /// basicConstraints cA and, if keyUsage is present, keyCertSign.
    pub is_ca: bool,
    /// basicConstraints pathLenConstraint.
    pub max_path_len: Option<u32>,
    /// subjectAltName dNSName entries.
    pub dns_names: Vec<&'a str>,
    /// First subject commonName.
    pub common_name: Option<&'a str>,
    /// keyUsage digitalSignature (true without a keyUsage extension). TLS
    /// 1.3 server authentication signs with the key, so it is required.
    pub digital_signature: bool,
    /// extendedKeyUsage includes serverAuth or anyExtendedKeyUsage (true
    /// without the extension).
    pub server_auth: bool,
    /// nameConstraints dNSName subtrees, critical or not.
    pub name_constraints: Option<NameConstraints<'a>>,
}

/// No moment this system checks certificates at can be earlier than this
/// (2026-10-25 00:00 UTC): a real-time clock that reads earlier (a dead
/// CMOS battery reads 2000) or none at all gives this instead of skipping
/// the validity dates, so a certificate that expired before it is always
/// refused (audit 2026-10-25 D, M1: no clock meant no date check). Raised
/// at each release (docs/WORKFLOW.md).
pub const TIME_FLOOR: u64 = 1_792_886_400;

/// What a chain's validity dates are checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidityTime {
    /// A clock that can be trusted: `notBefore <= t <= notAfter`.
    Clock(u64),
    /// No clock that can be trusted: only `notAfter >= floor`, since a
    /// certificate issued after the floor is no sign of a wrong date
    /// (audit of incident 0036, H1: checking `notBefore` against the floor
    /// refused every certificate renewed after the release).
    AtLeast(u64),
    /// Dates not checked (tests).
    Unchecked,
}

impl From<Option<u64>> for ValidityTime {
    fn from(now: Option<u64>) -> Self {
        now.map_or(ValidityTime::Unchecked, ValidityTime::Clock)
    }
}

/// The time certificates are checked at from the real-time clock's Unix
/// seconds: the clock when it reads at or after `TIME_FLOOR`; otherwise (no
/// clock, or one that reads earlier) `TIME_FLOOR` as a lower bound of
/// every `notAfter`.
pub fn check_time(rtc: Option<u64>) -> ValidityTime {
    match rtc {
        Some(t) if t >= TIME_FLOOR => ValidityTime::Clock(t),
        _ => ValidityTime::AtLeast(TIME_FLOOR),
    }
}

/// Checks `signature` over `message` with `key`. False on any mismatch
/// between key type and algorithm.
pub fn verify_signature(
    key: &PublicKey,
    alg: SignatureAlgorithm,
    message: &[u8],
    signature: &[u8],
) -> bool {
    let hash = match alg {
        SignatureAlgorithm::RsaPkcs1Sha256 | SignatureAlgorithm::EcdsaSha256 => Hash::Sha256,
        SignatureAlgorithm::RsaPkcs1Sha384 | SignatureAlgorithm::EcdsaSha384 => Hash::Sha384,
        SignatureAlgorithm::RsaPkcs1Sha512 => Hash::Sha512,
        SignatureAlgorithm::RsaPss(hash) => hash,
        SignatureAlgorithm::Ed25519 => {
            return match (key, <&[u8; 64]>::try_from(signature)) {
                (PublicKey::Ed25519(pk), Ok(sig)) => ed25519::verify(pk, message, sig),
                _ => false,
            };
        }
    };
    let mut buf = [0u8; 64];
    let len = hash.digest(message, &mut buf);
    let digest = &buf[..len];
    match (key, alg) {
        (PublicKey::Rsa { n, e }, SignatureAlgorithm::RsaPss(_)) => {
            rsa::verify_pss(n, e, hash, digest, signature)
        }
        (
            PublicKey::Rsa { n, e },
            SignatureAlgorithm::RsaPkcs1Sha256
            | SignatureAlgorithm::RsaPkcs1Sha384
            | SignatureAlgorithm::RsaPkcs1Sha512,
        ) => rsa::verify_pkcs1v15(n, e, hash, digest, signature),
        (
            PublicKey::Ec { curve, point },
            SignatureAlgorithm::EcdsaSha256 | SignatureAlgorithm::EcdsaSha384,
        ) => ecdsa::parse_der_signature(signature)
            .is_some_and(|(r, s)| ecdsa::verify(*curve, point, digest, r, s)),
        _ => false,
    }
}

/// True if `host` is one of the certificate's subjectAltName dNSNames (a
/// single left-most `*` label matches exactly one label), or, only when the
/// certificate has no dNSName, its common name. Case-insensitive; a
/// trailing dot on `host` is ignored.
/// An IP host (an IPv4 address, `matches_hostname`) matches a dNSName
/// that spells it exactly, never a wildcard.
pub fn matches_hostname(cert: &Certificate, host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.contains('*') {
        return false;
    }
    let matches = |pattern: &str| {
        if is_ipv4(host) {
            pattern.strip_suffix('.').unwrap_or(pattern) == host
        } else {
            name_matches(pattern, host)
        }
    };
    if cert.dns_names.is_empty() {
        cert.common_name.is_some_and(matches)
    } else {
        cert.dns_names.iter().any(|name| matches(name))
    }
}

/// Four decimal octets separated by dots.
fn is_ipv4(host: &str) -> bool {
    let mut parts = 0;
    host.split('.').all(|p| {
        parts += 1;
        !p.is_empty()
            && p.len() <= 3
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u8>().is_ok()
    }) && parts == 4
}

/// Checks the leaf's names against a CA's nameConstraints: every dNSName.
/// A constrained chain needs a subjectAltName (a common name is no
/// constrained name type), and an IP host is refused when the CA also
/// constrains IP addresses, which this client does not evaluate.
fn check_name_constraints(
    leaf: &Certificate,
    host: &str,
    nc: Option<&NameConstraints>,
) -> Result<(), Error> {
    let Some(nc) = nc else {
        return Ok(());
    };
    let subtrees = (nc.permitted.len() + nc.excluded.len()).max(1);
    if leaf.dns_names.is_empty()
        || leaf.dns_names.len().saturating_mul(subtrees) > NAME_CONSTRAINT_BUDGET
        || (nc.ip_subtrees && is_ipv4(host.strip_suffix('.').unwrap_or(host)))
        || !leaf.dns_names.iter().all(|n| nc.allows(n))
    {
        return Err(Error::NameConstraint);
    }
    Ok(())
}

fn name_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.strip_suffix('.').unwrap_or(pattern);
    match pattern.strip_prefix("*.") {
        // At least two labels after the wildcard: no `*.com`.
        Some(base) if !base.contains('*') && base.contains('.') => match host.split_once('.') {
            Some((label, rest)) => !label.is_empty() && rest.eq_ignore_ascii_case(base),
            None => false,
        },
        Some(_) => false,
        None => !pattern.contains('*') && pattern.eq_ignore_ascii_case(host),
    }
}

fn check_validity(not_before: u64, not_after: u64, now: ValidityTime) -> Result<(), Error> {
    match now {
        ValidityTime::Clock(t) if t < not_before => Err(Error::NotYetValid),
        ValidityTime::Clock(t) if t > not_after => Err(Error::Expired),
        ValidityTime::AtLeast(floor) if floor > not_after => Err(Error::Expired),
        _ => Ok(()),
    }
}

/// A leaf whose chain is trusted, unless it is itself a CA: a CA
/// certificate is no server's certificate (audit 2026-10-25 D, L2). Checked
/// last, so an untrusted self-signed CA still reads as an unknown issuer.
fn trusted_leaf(leaf: Certificate) -> Result<Certificate, Error> {
    if leaf.is_ca {
        return Err(Error::WrongPurpose);
    }
    Ok(leaf)
}

/// Verifies a server chain: `chain[0]` is the leaf, then intermediates in
/// any order. Each certificate must be signed by an intermediate of the
/// chain (a CA within its path length, valid at `now`) or by one of
/// `roots` (DER trust anchors; roots that do not parse are ignored). A
/// certificate identical to a root is trusted directly. `now`: what the
/// validity dates are checked against (`Some` Unix seconds is a trusted
/// clock, `None` skips them). Returns the parsed leaf.
pub fn verify_chain<'a>(
    chain: &[&'a [u8]],
    roots: &[&[u8]],
    host: &str,
    now: impl Into<ValidityTime>,
) -> Result<Certificate<'a>, Error> {
    verify_chain_with(chain, roots, roots::DISTRUSTED, host, now)
}

/// Whether `spki` (a whole SubjectPublicKeyInfo) is one of `distrusted`
/// (SHA-256 digests).
fn is_distrusted(spki: &[u8], distrusted: &[[u8; 32]]) -> bool {
    !distrusted.is_empty() && distrusted.contains(&aricode_crypto::sha256::sha256(spki))
}

/// [`verify_chain`] with an explicit list of distrusted keys: no
/// certificate on the path, leaf, intermediate or root, may have one of
/// them, whatever signed it (ADR 0054).
pub fn verify_chain_with<'a>(
    chain: &[&'a [u8]],
    roots: &[&[u8]],
    distrusted: &[[u8; 32]],
    host: &str,
    now: impl Into<ValidityTime>,
) -> Result<Certificate<'a>, Error> {
    let now = now.into();
    let leaf_der = *chain.first().ok_or(Error::Malformed)?;
    let leaf = Certificate::parse(leaf_der)?;
    if is_distrusted(leaf.spki, distrusted) {
        return Err(Error::Distrusted);
    }
    check_validity(leaf.not_before, leaf.not_after, now)?;
    if !matches_hostname(&leaf, host) {
        return Err(Error::NameMismatch);
    }
    if !leaf.server_auth || !leaf.digital_signature {
        return Err(Error::WrongPurpose);
    }
    let anchors: Vec<Parsed> = roots.iter().filter_map(|r| Parsed::parse(r).ok()).collect();
    let mut used = vec![false; chain.len()];
    used[0] = true;
    let mut current = leaf.clone();
    let mut current_der = leaf_der;
    let mut intermediates = 0;
    loop {
        if roots.contains(&current_der) {
            return trusted_leaf(leaf);
        }
        let signed_by = |key: &PublicKey| {
            verify_signature(
                key,
                current.signature_algorithm,
                current.tbs,
                current.signature,
            )
        };
        let mut error = Error::UnknownIssuer;
        for anchor in anchors.iter().filter(|a| a.subject == current.issuer) {
            if is_distrusted(anchor.spki, distrusted) {
                error = Error::Distrusted;
                continue;
            }
            if signed_by(&anchor.public_key) {
                check_validity(anchor.not_before, anchor.not_after, now)?;
                if !anchor.server_auth {
                    return Err(Error::WrongPurpose);
                }
                check_name_constraints(&leaf, host, anchor.name_constraints.as_ref())?;
                return trusted_leaf(leaf);
            }
            error = Error::BadSignature;
        }
        let mut next = None;
        for (i, der) in chain.iter().enumerate() {
            if used[i] {
                continue;
            }
            let candidate = match Certificate::parse(der) {
                Ok(c) => c,
                Err(e) => {
                    if error == Error::UnknownIssuer {
                        error = e;
                    }
                    continue;
                }
            };
            if candidate.subject != current.issuer {
                continue;
            }
            let outcome = if is_distrusted(candidate.spki, distrusted) {
                Err(Error::Distrusted)
            } else if !signed_by(&candidate.public_key) {
                Err(Error::BadSignature)
            } else if !candidate.is_ca
                || candidate
                    .max_path_len
                    .is_some_and(|max| intermediates > max as usize)
            {
                Err(Error::NotCa)
            } else if !candidate.server_auth {
                // An issuer limited to other purposes cannot vouch for a
                // TLS server (audit 2026-10-25 D, H2).
                Err(Error::WrongPurpose)
            } else {
                check_validity(candidate.not_before, candidate.not_after, now).and_then(|()| {
                    check_name_constraints(&leaf, host, candidate.name_constraints.as_ref())
                })
            };
            match outcome {
                Ok(()) => {
                    next = Some((i, candidate));
                    break;
                }
                Err(e) => error = e,
            }
        }
        let (i, issuer) = next.ok_or(error)?;
        intermediates += 1;
        if intermediates > MAX_INTERMEDIATES {
            return Err(Error::ChainTooLong);
        }
        used[i] = true;
        current = issuer;
        current_der = chain[i];
    }
}

#[cfg(test)]
mod tests;
