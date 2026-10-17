// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! IPv4 header (RFC 791) without options or fragmentation.

use crate::{be16, checksum, Ipv4Addr};

pub const HEADER_LEN: usize = 20;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_UDP: u8 = 17;
pub const PROTO_TCP: u8 = 6;
const DEFAULT_TTL: u8 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: u8,
    pub ttl: u8,
    pub identification: u16,
    /// Payload length (without the header).
    pub payload_len: u16,
}

impl Header {
    pub fn new(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        protocol: u8,
        identification: u16,
        payload_len: u16,
    ) -> Self {
        Self {
            src,
            dst,
            protocol,
            ttl: DEFAULT_TTL,
            identification,
            payload_len,
        }
    }

    /// Parses and validates (version, IHL, checksum, length); returns the
    /// payload trimmed to `total_length`. Rejects fragments.
    pub fn parse(b: &[u8]) -> Option<(Header, &[u8])> {
        if b.len() < HEADER_LEN || b[0] >> 4 != 4 {
            return None;
        }
        let ihl = (b[0] & 0xF) as usize * 4;
        if ihl < HEADER_LEN || b.len() < ihl {
            return None;
        }
        if checksum(0, &b[..ihl]) != 0 {
            return None;
        }
        let total = be16(&b[2..4]) as usize;
        if total < ihl || total > b.len() {
            return None;
        }
        let flags_frag = be16(&b[6..8]);
        if flags_frag & 0x2000 != 0 || flags_frag & 0x1FFF != 0 {
            return None; // fragment
        }
        let mut src = [0u8; 4];
        let mut dst = [0u8; 4];
        src.copy_from_slice(&b[12..16]);
        dst.copy_from_slice(&b[16..20]);
        Some((
            Header {
                src: Ipv4Addr(src),
                dst: Ipv4Addr(dst),
                protocol: b[9],
                ttl: b[8],
                identification: be16(&b[4..6]),
                payload_len: (total - ihl) as u16,
            },
            &b[ihl..total],
        ))
    }

    /// Writes the header (20 bytes, DF, computed checksum).
    pub fn write(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < HEADER_LEN {
            return None;
        }
        let total = HEADER_LEN as u16 + self.payload_len;
        out[0] = 0x45;
        out[1] = 0;
        out[2..4].copy_from_slice(&total.to_be_bytes());
        out[4..6].copy_from_slice(&self.identification.to_be_bytes());
        out[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // DF
        out[8] = self.ttl;
        out[9] = self.protocol;
        out[10..12].copy_from_slice(&[0, 0]);
        out[12..16].copy_from_slice(&self.src.0);
        out[16..20].copy_from_slice(&self.dst.0);
        let sum = checksum(0, &out[..HEADER_LEN]);
        out[10..12].copy_from_slice(&sum.to_be_bytes());
        Some(HEADER_LEN)
    }
}

/// Partial sum of the IPv4 pseudo-header for UDP/TCP.
pub fn pseudo_header_sum(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, len: u16) -> u32 {
    let mut sum = 0u32;
    for pair in src
        .0
        .as_chunks::<2>()
        .0
        .iter()
        .chain(dst.0.as_chunks::<2>().0.iter())
    {
        sum += u16::from_be_bytes(*pair) as u32;
    }
    sum + protocol as u32 + len as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip_and_validation() {
        let h = Header::new(
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 2),
            PROTO_UDP,
            0x1234,
            8,
        );
        let mut buf = [0u8; 28];
        assert_eq!(h.write(&mut buf), Some(HEADER_LEN));
        buf[20..].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let (parsed, payload) = Header::parse(&buf).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(payload, &[1, 2, 3, 4, 5, 6, 7, 8]);
        // Corrupt checksum.
        let mut bad = buf;
        bad[8] = 1;
        assert!(Header::parse(&bad).is_none());
        // Total length larger than the buffer.
        let mut short = buf;
        short[3] = 0xFF;
        let sum = checksum(0, &short[..HEADER_LEN]);
        // Recompute to isolate the length check.
        short[10..12].copy_from_slice(&[0, 0]);
        let sum2 = checksum(0, &short[..HEADER_LEN]);
        short[10..12].copy_from_slice(&sum2.to_be_bytes());
        let _ = sum;
        assert!(Header::parse(&short).is_none());
        // Fragment.
        let mut frag = buf;
        frag[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        frag[10..12].copy_from_slice(&[0, 0]);
        let s = checksum(0, &frag[..HEADER_LEN]);
        frag[10..12].copy_from_slice(&s.to_be_bytes());
        assert!(Header::parse(&frag).is_none());
    }
}
