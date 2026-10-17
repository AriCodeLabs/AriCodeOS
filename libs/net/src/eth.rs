// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Ethernet II frames.

use crate::{be16, MacAddr};

pub const HEADER_LEN: usize = 14;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
/// Usual MTU: maximum payload of a frame.
pub const MTU: usize = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
}

impl Header {
    pub fn parse(frame: &[u8]) -> Option<(Header, &[u8])> {
        if frame.len() < HEADER_LEN {
            return None;
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&frame[..6]);
        src.copy_from_slice(&frame[6..12]);
        Some((
            Header {
                dst: MacAddr(dst),
                src: MacAddr(src),
                ethertype: be16(&frame[12..14]),
            },
            &frame[HEADER_LEN..],
        ))
    }

    /// Writes the header at the start of `out`.
    pub fn write(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < HEADER_LEN {
            return None;
        }
        out[..6].copy_from_slice(&self.dst.0);
        out[6..12].copy_from_slice(&self.src.0);
        out[12..14].copy_from_slice(&self.ethertype.to_be_bytes());
        Some(HEADER_LEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = Header {
            dst: MacAddr::BROADCAST,
            src: MacAddr([0x52, 0x54, 0, 0x12, 0x34, 0x56]),
            ethertype: ETHERTYPE_ARP,
        };
        let mut buf = [0u8; 20];
        assert_eq!(h.write(&mut buf), Some(HEADER_LEN));
        let (parsed, rest) = Header::parse(&buf).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(rest.len(), 6);
        assert!(Header::parse(&buf[..10]).is_none());
        assert!(h.write(&mut buf[..5]).is_none());
    }
}
