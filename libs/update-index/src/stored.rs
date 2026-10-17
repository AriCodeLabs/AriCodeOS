// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The copy of a verified index that `update` keeps for `upgrade` (one per
//! source slot, all in `updater/indexes`): the source it came from, its
//! signature and the
//! exact signed bytes, so `upgrade` verifies it again instead of trusting
//! AriFS (ADR 0029, decision 4).
//!
//! ```text
//! source = http://10.0.2.2:17010/update/
//! signature = <128 hex digits>
//!
//! <index bytes, as signed>
//! ```

use crate::MAX_SOURCES;
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

/// All slots in one file: per slot `entry LEN\n` and LEN bytes (0 for an
/// empty slot), [`MAX_SOURCES`] entries in order.
pub fn encode_all(slots: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..MAX_SOURCES {
        let bytes = slots.get(i).map_or(&[][..], Vec::as_slice);
        out.extend_from_slice(alloc::format!("entry {}\n", bytes.len()).as_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// The slots of [`encode_all`]; `None` if the framing is damaged (a
/// truncated or hand-edited file), never a partial guess. An empty file
/// is [`MAX_SOURCES`] empty slots.
pub fn decode_all(mut bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    if bytes.is_empty() {
        return Some(alloc::vec![Vec::new(); MAX_SOURCES]);
    }
    let mut slots = Vec::with_capacity(MAX_SOURCES);
    for _ in 0..MAX_SOURCES {
        let nl = bytes.iter().position(|&b| b == b'\n')?;
        let len: usize = core::str::from_utf8(&bytes[..nl])
            .ok()?
            .strip_prefix("entry ")?
            .parse()
            .ok()?;
        let rest = &bytes[nl + 1..];
        if len > rest.len() {
            return None;
        }
        slots.push(rest[..len].to_vec());
        bytes = &rest[len..];
    }
    bytes.is_empty().then_some(slots)
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
    fn all_slots_in_one_file() {
        let a = encode("http://a/", b"ab", b"format = 1\n\nentry 3\n");
        let slots = alloc::vec![a.clone(), Vec::new(), a.clone()];
        let file = encode_all(&slots);
        let back = decode_all(&file).unwrap();
        assert_eq!(back.len(), MAX_SOURCES);
        assert_eq!((&back[0], &back[2]), (&a, &a));
        assert!(back[1].is_empty() && back[3].is_empty());
        assert_eq!(
            decode_all(b"").unwrap(),
            alloc::vec![Vec::<u8>::new(); MAX_SOURCES]
        );
        assert!(decode_all(&file[..file.len() - 1]).is_none(), "truncated");
        let mut longer = file.clone();
        longer.push(b'x');
        assert!(decode_all(&longer).is_none(), "trailing bytes");
        assert!(decode_all(b"entry x\n").is_none());
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
