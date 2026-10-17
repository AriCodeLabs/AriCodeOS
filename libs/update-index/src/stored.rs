// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The copy of a verified index that `update` keeps for `upgrade`
//! (`updater/index-N`): the source it came from, its signature and the
//! exact signed bytes, so `upgrade` verifies it again instead of trusting
//! AriFS (ADR 0029, decision 4).
//!
//! ```text
//! source = http://10.0.2.2:17010/update/
//! signature = <128 hex digits>
//!
//! <index bytes, as signed>
//! ```

use alloc::vec::Vec;

/// A decoded stored index; the index is still unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored<'a> {
    pub source: &'a str,
    /// The signature as hex (the `index.sig` encoding).
    pub signature: &'a [u8],
    pub index: &'a [u8],
}

/// Encodes a stored index.
pub fn encode(source: &str, signature_hex: &[u8], index: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len() + signature_hex.len() + index.len() + 32);
    out.extend_from_slice(b"source = ");
    out.extend_from_slice(source.as_bytes());
    out.extend_from_slice(b"\nsignature = ");
    out.extend_from_slice(trim(signature_hex));
    out.extend_from_slice(b"\n\n");
    out.extend_from_slice(index);
    out
}

fn trim(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &b[start..end]
}

/// Decodes a stored index; `None` if the header is malformed (an empty
/// file is a slot with nothing stored).
pub fn decode(bytes: &[u8]) -> Option<Stored<'_>> {
    let split = bytes.windows(2).position(|w| w == b"\n\n")?;
    let header = core::str::from_utf8(&bytes[..split]).ok()?;
    let index = &bytes[split + 2..];
    let mut lines = header.lines();
    let source = lines.next()?.strip_prefix("source = ")?;
    let signature = lines.next()?.strip_prefix("signature = ")?.as_bytes();
    if lines.next().is_some() || source.is_empty() || signature.is_empty() {
        return None;
    }
    Some(Stored {
        source,
        signature,
        index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let index = b"format = 1\nserial = 9\n\n# blank lines inside the index survive\n";
        let bytes = encode("http://h/", b"abcd\n", index);
        let s = decode(&bytes).unwrap();
        assert_eq!(s.source, "http://h/");
        assert_eq!(s.signature, b"abcd");
        assert_eq!(s.index, index);
    }

    #[test]
    fn malformed_is_none() {
        assert!(decode(b"").is_none());
        assert!(decode(b"source = x\n").is_none());
        assert!(decode(b"signature = ab\nsource = x\n\nformat = 1\n").is_none());
        assert!(decode(b"source = x\nsignature = ab\nextra = 1\n\nformat = 1\n").is_none());
        assert!(decode(b"source = \nsignature = ab\n\n").is_none());
    }
}
