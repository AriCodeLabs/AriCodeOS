// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Minimal network stack (Phase 22, section 38). Encoders and decoders
//! without `alloc` over caller-supplied buffers, plus the small state
//! machines a ring 3 network service needs (ARP cache, DHCP
//! client, TCP and its secrets). All the logic is pure and tested on the
//! host; the `netd` service only connects it to the virtio-net driver and
//! to IPC.

#![no_std]

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod eth;
pub mod icmp;
pub mod ipv4;
pub mod isn;
pub mod tcp;
pub mod udp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xFF; 6]);
    pub const ZERO: MacAddr = MacAddr([0; 6]);
    pub fn is_broadcast(&self) -> bool {
        *self == Self::BROADCAST
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr([0; 4]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255; 4]);

    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self([a, b, c, d])
    }
    pub fn from_u32(v: u32) -> Self {
        Self(v.to_be_bytes())
    }
    pub fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
    pub fn is_unspecified(&self) -> bool {
        *self == Self::UNSPECIFIED
    }
    /// Class D, 224.0.0.0/4.
    pub fn is_multicast(&self) -> bool {
        self.0[0] & 0xF0 == 0xE0
    }
    /// Directed broadcast of the subnet this address is in under `mask`.
    pub fn subnet_broadcast(&self, mask: Ipv4Addr) -> Ipv4Addr {
        Self::from_u32(self.to_u32() | !mask.to_u32())
    }
    /// An address that cannot be the source of a connection (RFC 1122
    /// section 3.2.1.3): unspecified, multicast, the limited broadcast or
    /// the broadcast of the subnet `local` is in under `mask`. A SYN from
    /// one is dropped, never answered.
    pub fn is_invalid_source(&self, local: Ipv4Addr, mask: Ipv4Addr) -> bool {
        self.is_unspecified()
            || self.is_multicast()
            || *self == Self::BROADCAST
            || *self == local.subnet_broadcast(mask)
    }
    /// Same subnet as `other` under `mask`.
    pub fn same_subnet(&self, other: Ipv4Addr, mask: Ipv4Addr) -> bool {
        (self.to_u32() & mask.to_u32()) == (other.to_u32() & mask.to_u32())
    }
    /// Parses `a.b.c.d`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut out = [0u8; 4];
        let mut parts = s.split('.');
        for slot in &mut out {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *slot = p.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(out))
    }
    /// Writes `a.b.c.d` into `buf` and returns the used portion.
    pub fn format<'a>(&self, buf: &'a mut [u8; 15]) -> &'a [u8] {
        let mut n = 0;
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                buf[n] = b'.';
                n += 1;
            }
            if *b >= 100 {
                buf[n] = b'0' + b / 100;
                n += 1;
            }
            if *b >= 10 {
                buf[n] = b'0' + (b / 10) % 10;
                n += 1;
            }
            buf[n] = b'0' + b % 10;
            n += 1;
        }
        &buf[..n]
    }
}

/// Internet checksum (RFC 1071) over `data`, with `initial`
/// already accumulated (for the pseudo-header).
pub fn checksum(initial: u32, data: &[u8]) -> u16 {
    let mut sum = initial;
    let (pairs, rest) = data.as_chunks::<2>();
    for c in pairs {
        sum += u16::from_be_bytes(*c) as u32;
    }
    if let [last] = rest {
        sum += (*last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_parse_and_format() {
        let a = Ipv4Addr::parse("10.0.2.15").unwrap();
        assert_eq!(a, Ipv4Addr::new(10, 0, 2, 15));
        let mut buf = [0u8; 15];
        assert_eq!(a.format(&mut buf), b"10.0.2.15");
        assert_eq!(
            Ipv4Addr::new(255, 200, 0, 7).format(&mut buf),
            b"255.200.0.7"
        );
        assert!(Ipv4Addr::parse("10.0.2").is_none());
        assert!(Ipv4Addr::parse("10.0.2.256").is_none());
        assert!(Ipv4Addr::parse("10.0.2.15.1").is_none());
        assert!(Ipv4Addr::parse("a.b.c.d").is_none());
        assert!(a.same_subnet(Ipv4Addr::new(10, 0, 2, 2), Ipv4Addr::new(255, 255, 255, 0)));
        assert!(!a.same_subnet(Ipv4Addr::new(10, 1, 2, 2), Ipv4Addr::new(255, 255, 255, 0)));
    }

    #[test]
    fn checksum_rfc1071_example() {
        // Classic example: an IPv4 header with checksum 0 must yield 0xb861.
        let hdr = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(checksum(0, &hdr), 0xb861);
        let mut with = hdr;
        with[10..12].copy_from_slice(&0xb861u16.to_be_bytes());
        assert_eq!(checksum(0, &with), 0);
        assert_eq!(checksum(0, &[0x01]), !0x0100u16);
    }
}

#[cfg(test)]
mod source_tests {
    use super::Ipv4Addr;

    /// Incident 0013 audit: sources that cannot be a host (RFC 1122 section
    /// 3.2.1.3) are told apart from any unicast address, the local one
    /// included.
    #[test]
    fn invalid_sources_are_recognised() {
        let local = Ipv4Addr::new(10, 0, 2, 15);
        let mask = Ipv4Addr::new(255, 255, 255, 0);
        assert_eq!(local.subnet_broadcast(mask), Ipv4Addr::new(10, 0, 2, 255));
        for bad in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(10, 0, 2, 255),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(239, 255, 255, 250),
        ] {
            assert!(bad.is_invalid_source(local, mask), "{bad:?}");
        }
        for ok in [
            Ipv4Addr::new(10, 0, 2, 2),
            Ipv4Addr::new(10, 0, 3, 255),
            Ipv4Addr::new(223, 255, 255, 255),
            Ipv4Addr::new(240, 0, 0, 1),
            local,
        ] {
            assert!(!ok.is_invalid_source(local, mask), "{ok:?}");
        }
    }
}
