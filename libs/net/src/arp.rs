//! ARP for IPv4 over Ethernet (RFC 826) and a small cache.

use crate::{be16, Ipv4Addr, MacAddr};

pub const PACKET_LEN: usize = 28;
pub const OP_REQUEST: u16 = 1;
pub const OP_REPLY: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub op: u16,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl Packet {
    pub fn request(sender_mac: MacAddr, sender_ip: Ipv4Addr, target_ip: Ipv4Addr) -> Self {
        Self {
            op: OP_REQUEST,
            sender_mac,
            sender_ip,
            target_mac: MacAddr::ZERO,
            target_ip,
        }
    }

    /// Reply to `request` from `mac`/`ip`.
    pub fn reply_to(request: &Packet, mac: MacAddr, ip: Ipv4Addr) -> Self {
        Self {
            op: OP_REPLY,
            sender_mac: mac,
            sender_ip: ip,
            target_mac: request.sender_mac,
            target_ip: request.sender_ip,
        }
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < PACKET_LEN
            || be16(&b[0..2]) != 1
            || be16(&b[2..4]) != 0x0800
            || b[4] != 6
            || b[5] != 4
        {
            return None;
        }
        let mac = |o: usize| {
            let mut m = [0u8; 6];
            m.copy_from_slice(&b[o..o + 6]);
            MacAddr(m)
        };
        let ip = |o: usize| {
            let mut a = [0u8; 4];
            a.copy_from_slice(&b[o..o + 4]);
            Ipv4Addr(a)
        };
        Some(Self {
            op: be16(&b[6..8]),
            sender_mac: mac(8),
            sender_ip: ip(14),
            target_mac: mac(18),
            target_ip: ip(24),
        })
    }

    pub fn write(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < PACKET_LEN {
            return None;
        }
        out[0..2].copy_from_slice(&1u16.to_be_bytes());
        out[2..4].copy_from_slice(&0x0800u16.to_be_bytes());
        out[4] = 6;
        out[5] = 4;
        out[6..8].copy_from_slice(&self.op.to_be_bytes());
        out[8..14].copy_from_slice(&self.sender_mac.0);
        out[14..18].copy_from_slice(&self.sender_ip.0);
        out[18..24].copy_from_slice(&self.target_mac.0);
        out[24..28].copy_from_slice(&self.target_ip.0);
        Some(PACKET_LEN)
    }
}

/// Fixed-size ARP cache that replaces the oldest entry.
pub struct Cache<const N: usize> {
    entries: [Option<(Ipv4Addr, MacAddr)>; N],
    next: usize,
}

impl<const N: usize> Default for Cache<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Cache<N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            next: 0,
        }
    }

    pub fn lookup(&self, ip: Ipv4Addr) -> Option<MacAddr> {
        self.entries
            .iter()
            .flatten()
            .find(|(i, _)| *i == ip)
            .map(|(_, m)| *m)
    }

    pub fn insert(&mut self, ip: Ipv4Addr, mac: MacAddr) {
        if let Some(e) = self.entries.iter_mut().flatten().find(|(i, _)| *i == ip) {
            e.1 = mac;
            return;
        }
        if let Some(slot) = self.entries.iter_mut().find(|e| e.is_none()) {
            *slot = Some((ip, mac));
            return;
        }
        self.entries[self.next] = Some((ip, mac));
        self.next = (self.next + 1) % N;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_reply_round_trip() {
        let me = MacAddr([2, 0, 0, 0, 0, 1]);
        let ip = Ipv4Addr::new(10, 0, 2, 15);
        let gw = Ipv4Addr::new(10, 0, 2, 2);
        let req = Packet::request(me, ip, gw);
        let mut buf = [0u8; PACKET_LEN];
        assert_eq!(req.write(&mut buf), Some(PACKET_LEN));
        let parsed = Packet::parse(&buf).unwrap();
        assert_eq!(parsed, req);
        let reply = Packet::reply_to(&parsed, MacAddr([2, 0, 0, 0, 0, 2]), gw);
        assert_eq!(reply.op, OP_REPLY);
        assert_eq!(reply.target_mac, me);
        assert_eq!(reply.target_ip, ip);
        buf[4] = 8; // wrong hardware length
        assert!(Packet::parse(&buf).is_none());
    }

    #[test]
    fn cache_replaces_oldest() {
        let mut c: Cache<2> = Cache::new();
        let m = |x| MacAddr([x; 6]);
        c.insert(Ipv4Addr::new(1, 1, 1, 1), m(1));
        c.insert(Ipv4Addr::new(2, 2, 2, 2), m(2));
        c.insert(Ipv4Addr::new(1, 1, 1, 1), m(9)); // updates
        assert_eq!(c.lookup(Ipv4Addr::new(1, 1, 1, 1)), Some(m(9)));
        c.insert(Ipv4Addr::new(3, 3, 3, 3), m(3)); // evicts the oldest (slot 0)
        assert_eq!(c.lookup(Ipv4Addr::new(1, 1, 1, 1)), None);
        assert_eq!(c.lookup(Ipv4Addr::new(2, 2, 2, 2)), Some(m(2)));
        assert_eq!(c.lookup(Ipv4Addr::new(3, 3, 3, 3)), Some(m(3)));
    }
}
