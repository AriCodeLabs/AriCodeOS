// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Certificate policy of the TLS client (ADR 0067). Chains are verified by
//! `rustls-webpki` (in `aricode-tls`); this crate holds what is this
//! system's own on top of it:
//! - the trust anchors, extracted from Mozilla's root store
//!   ([`roots::default`]), and the keys distrusted anywhere in a chain
//!   ([`roots::DISTRUSTED`], ADR 0054);
//! - the time certificates are checked at when the clock cannot be
//!   trusted ([`check_time`]);
//! - no wildcard over a public suffix ([`wildcard_over_public_suffix`],
//!   ADR 0062);
//! - the reasons a chain is refused ([`Error`]), for messages.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod roots;
mod suffixes;

/// Why a certificate chain was refused.
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
    /// An issuer is not a CA, or a CA is used as the leaf.
    NotCa,
    /// The leaf does not name the host.
    NameMismatch,
    /// More intermediates than the verifier follows.
    ChainTooLong,
    /// The leaf may not authenticate a TLS server, or a CA of the chain may
    /// not issue for one (extendedKeyUsage, keyUsage).
    WrongPurpose,
    /// A CA's nameConstraints do not allow a name of the leaf (RFC 5280
    /// 4.2.1.10).
    NameConstraint,
    /// A certificate of the chain, or the root it ends at, has a key this
    /// system no longer trusts (`roots::DISTRUSTED`, ADR 0054).
    Distrusted,
    /// The leaf names the host only through a wildcard over a public
    /// suffix (`*.co.uk`, ADR 0062).
    WildcardOverSuffix,
}

/// No moment this system checks certificates at can be earlier than this
/// (2026-11-01 00:00 UTC): a real-time clock that reads earlier (a dead
/// CMOS battery reads 2000) or none at all gives this instead of skipping
/// the validity dates, so a certificate that expired before it is always
/// refused (audit 2026-10-25 D, M1: no clock meant no date check). Raised
/// at each release (docs/WORKFLOW.md).
pub const TIME_FLOOR: u64 = 1_793_491_200;

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

/// Whether `spki` (a whole SubjectPublicKeyInfo, DER) is one of
/// [`roots::DISTRUSTED`].
pub fn is_distrusted(spki: &[u8]) -> bool {
    is_distrusted_in(spki, roots::DISTRUSTED)
}

fn is_distrusted_in(spki: &[u8], distrusted: &[[u8; 32]]) -> bool {
    !distrusted.is_empty() && distrusted.contains(&aricode_crypto::sha256::sha256(spki))
}

/// True if `host` (a DNS name) can only be matched by a wildcard standing
/// on a public suffix (ADR 0062): no dNSName of the leaf (`dns_names`)
/// spells the host, and the host's parent, where a wildcard would stand, is
/// a public suffix of the ICANN section or has a single label (`*.com`).
/// Called after the chain and the name were verified, so a wildcard did
/// match.
pub fn wildcard_over_public_suffix<'a>(
    host: &str,
    dns_names: impl Iterator<Item = &'a str>,
) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    let mut names = dns_names;
    if names.any(|n| n.strip_suffix('.').unwrap_or(n).eq_ignore_ascii_case(host)) {
        return false;
    }
    match host.split_once('.') {
        Some((_, parent)) => !parent.contains('.') || suffixes::is_public_suffix(parent),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_without_a_trusted_clock() {
        assert_eq!(
            check_time(Some(TIME_FLOOR + 5)),
            ValidityTime::Clock(TIME_FLOOR + 5)
        );
        assert_eq!(
            check_time(Some(946_684_800)),
            ValidityTime::AtLeast(TIME_FLOOR)
        );
        assert_eq!(check_time(None), ValidityTime::AtLeast(TIME_FLOOR));
    }

    #[test]
    fn wildcards_over_public_suffixes_are_refused() {
        // Matched through `*.co.uk`: refused; through `*.example.co.uk`: fine.
        assert!(wildcard_over_public_suffix(
            "shop.co.uk",
            ["*.co.uk"].into_iter()
        ));
        assert!(!wildcard_over_public_suffix(
            "www.example.co.uk",
            ["*.example.co.uk"].into_iter()
        ));
        // A name spelled exactly is no wildcard match.
        assert!(!wildcard_over_public_suffix(
            "co.uk.example",
            ["co.uk.example"].into_iter()
        ));
        assert!(!wildcard_over_public_suffix(
            "www.example.com.",
            ["www.example.com"].into_iter()
        ));
        // `*.com` stands on a single label.
        assert!(wildcard_over_public_suffix(
            "example.com",
            ["*.com"].into_iter()
        ));
        // `*.kobe.jp` makes `kobe.jp` a suffix (audit of ea368aa, finding 2).
        assert!(wildcard_over_public_suffix(
            "city.kobe.jp",
            ["*.kobe.jp"].into_iter()
        ));
    }

    #[test]
    fn distrusted_keys() {
        let spki = b"some subject public key info";
        let digest = aricode_crypto::sha256::sha256(spki);
        assert!(is_distrusted_in(spki, &[digest]));
        assert!(!is_distrusted_in(spki, &[[0; 32]]));
        assert!(!is_distrusted_in(spki, &[]));
    }
}
