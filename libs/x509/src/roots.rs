// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Embedded trust anchors (Mozilla's TLS server roots, `roots/README.md`)
//! and the keys no longer trusted anywhere in a chain.

/// The TLS server roots of Mozilla's store, concatenated DER, extracted by
/// `tools/mozilla-roots.py` (`roots/mozilla.txt` lists them; ADR 0067).
const MOZILLA: &[u8] = include_bytes!("../roots/mozilla.der");

/// The root certificates trusted by default, one DER certificate each.
pub fn default() -> impl Iterator<Item = &'static [u8]> {
    let mut rest = MOZILLA;
    core::iter::from_fn(move || {
        let len = der_len(rest)?;
        let (cert, tail) = rest.split_at(len);
        rest = tail;
        Some(cert)
    })
}

/// Length of the DER element at the start of `der` (tag, length, value),
/// `None` at the end or on a truncated one.
fn der_len(der: &[u8]) -> Option<usize> {
    let (&first, rest) = der.get(1..)?.split_first()?;
    let (header, value) = match first {
        n @ 0..=0x7F => (2, n as usize),
        0x81..=0x83 => {
            let k = (first & 0x7F) as usize;
            let v = rest
                .get(..k)?
                .iter()
                .fold(0usize, |a, &b| (a << 8) | b as usize);
            (2 + k, v)
        }
        _ => return None,
    };
    let total = header + value;
    (total <= der.len()).then_some(total)
}

/// SHA-256 of the SubjectPublicKeyInfo of keys no longer trusted, wherever
/// they appear in a chain: a root, an intermediate cross-signed by a root
/// still trusted, or a leaf (ADR 0054). A release adds one with a comment
/// naming the certificate and the advisory; removing a root from the store
/// alone would not stop an intermediate it cross-signed.
///
/// The eight eMudhra intermediates whose private keys were compromised
/// (October 2026; Mozilla bug 2083449; the same list as Go's
/// `crypto/x509` and rustls-webpki 0.103.16), each issued by an emSign
/// root and cross-signed by Sectigo's AAA Certificate Services. Mozilla's
/// store holds emSign roots, so these entries are what refuses those
/// intermediates.
pub const DISTRUSTED: &[[u8; 32]] = &[
    // CN=EM DV TLS CA - G3B,O=eMudhra BV,C=NL
    [
        0xeb, 0xed, 0x64, 0x75, 0xc2, 0xc2, 0xd8, 0x0f, 0xcb, 0x19, 0x9f, 0x3f, 0xe8, 0xdd, 0x6e,
        0x6b, 0x3c, 0xe6, 0x0b, 0x77, 0xeb, 0x15, 0xd4, 0xe4, 0x2c, 0x3d, 0xec, 0x0c, 0x6f, 0x5b,
        0x51, 0x05,
    ],
    // CN=EM DV TLS CA - G3C,O=eMudhra Inc,C=US
    [
        0x49, 0xdd, 0x35, 0x3d, 0xc2, 0xb9, 0x19, 0xa3, 0x77, 0x06, 0x45, 0x11, 0x7d, 0x0e, 0x60,
        0xc1, 0x61, 0x15, 0xe7, 0xd1, 0xbc, 0x25, 0x9e, 0xef, 0xe7, 0x34, 0xb0, 0x0e, 0xa4, 0x34,
        0xfe, 0x5f,
    ],
    // CN=EM EV TLS CA - G3B,O=eMudhra BV,C=NL
    [
        0xe0, 0x25, 0x4c, 0x7f, 0xcd, 0xc2, 0x65, 0xe2, 0xf0, 0xf8, 0xa4, 0x28, 0xda, 0xbb, 0x75,
        0xab, 0x4a, 0xe0, 0x39, 0xb4, 0x87, 0x2d, 0x62, 0xf5, 0xee, 0x57, 0xf0, 0x87, 0x7d, 0x5a,
        0x8a, 0x41,
    ],
    // CN=EM EV TLS CA - G3C,O=eMudhra Inc,C=US
    [
        0x5c, 0x06, 0x60, 0xa2, 0xb1, 0x64, 0x91, 0x0d, 0x8f, 0xf5, 0x89, 0xc1, 0x8e, 0x14, 0x55,
        0x2e, 0x9e, 0x0e, 0xc2, 0x37, 0xdc, 0xef, 0xe2, 0xcb, 0x73, 0x6d, 0x41, 0xa4, 0x81, 0x1f,
        0x81, 0xa0,
    ],
    // CN=EM OV TLS CA - G3B,O=eMudhra BV,C=NL
    [
        0x43, 0xbf, 0xfa, 0x19, 0xd6, 0xef, 0x65, 0x14, 0x51, 0xf6, 0x84, 0x1e, 0x01, 0x99, 0x31,
        0xe9, 0x6f, 0x6f, 0x2e, 0xae, 0xff, 0x28, 0xab, 0x4b, 0x6f, 0x1c, 0xe4, 0x57, 0x6d, 0xe2,
        0x71, 0x7b,
    ],
    // CN=EM OV TLS CA - G3C,O=eMudhra Inc,C=US
    [
        0x63, 0xec, 0xef, 0xee, 0x43, 0x1d, 0x49, 0x52, 0x3b, 0xc1, 0x13, 0x6c, 0x4e, 0x72, 0xd0,
        0x20, 0x6a, 0x47, 0x70, 0x86, 0xca, 0xf6, 0x06, 0xcd, 0x5c, 0xf0, 0x25, 0x68, 0x6c, 0x10,
        0x9c, 0xf8,
    ],
    // CN=EM OV CS CA - G3B,O=eMudhra BV,C=NL
    [
        0xec, 0x4f, 0xfa, 0x1a, 0xd0, 0xac, 0x88, 0x08, 0x13, 0x14, 0xb7, 0xaa, 0xff, 0xef, 0x6a,
        0x91, 0xf9, 0x7c, 0xd5, 0x72, 0x91, 0xbb, 0x5b, 0xa6, 0x86, 0xc5, 0x41, 0x5f, 0x5e, 0xb4,
        0xf6, 0xb6,
    ],
    // CN=EM OV CS CA - G3C,O=eMudhra Inc,C=US
    [
        0x92, 0xc0, 0x63, 0xd6, 0x7e, 0x8f, 0x14, 0xdb, 0x71, 0xd2, 0x88, 0xe1, 0x12, 0x66, 0xff,
        0xda, 0xf6, 0x9c, 0xbf, 0x11, 0x5f, 0x7b, 0xab, 0x14, 0xf3, 0xbc, 0x2f, 0x68, 0xd2, 0x5e,
        0x0e, 0x30,
    ],
];

#[cfg(test)]
mod tests {
    #[test]
    fn mozilla_store_splits_into_certificates() {
        let certs: Vec<&[u8]> = super::default().collect();
        let listed = include_str!("../roots/mozilla.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .count();
        assert_eq!(certs.len(), listed);
        assert!(certs.len() > 100);
        assert_eq!(
            certs.iter().map(|c| c.len()).sum::<usize>(),
            super::MOZILLA.len()
        );
        // Every listed fingerprint is the SHA-256 of a certificate, in order.
        for (cert, line) in certs.iter().zip(
            include_str!("../roots/mozilla.txt")
                .lines()
                .filter(|l| !l.starts_with('#')),
        ) {
            let mut hex = [0u8; 64];
            assert_eq!(
                &line[..64],
                aricode_crypto::hex(&aricode_crypto::sha256::sha256(cert), &mut hex)
            );
        }
    }

    extern crate std;
    use std::vec::Vec;
}
