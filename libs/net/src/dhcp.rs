// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Minimal DHCP client (RFC 2131): DISCOVER → OFFER → REQUEST → ACK.

use crate::{be32, Ipv4Addr, MacAddr};

pub const CLIENT_PORT: u16 = 68;
pub const SERVER_PORT: u16 = 67;
const MAGIC: [u8; 4] = [99, 130, 83, 99];
/// Minimum size of a message (fixed header + magic).
pub const FIXED_LEN: usize = 240;
pub const MAX_LEN: usize = 576;

const OPT_SUBNET: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_MSG_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAM_LIST: u8 = 55;
const OPT_END: u8 = 255;

pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const DECLINE: u8 = 4;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;

/// Configuration obtained from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Config {
    pub address: Ipv4Addr,
    pub subnet_mask: Ipv4Addr,
    pub router: Ipv4Addr,
    pub dns: Ipv4Addr,
    pub server: Ipv4Addr,
    pub lease_secs: u32,
}

/// Received message, already parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub msg_type: u8,
    pub xid: u32,
    pub yiaddr: Ipv4Addr,
    pub config: Config,
}

impl Message {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < FIXED_LEN || b[0] != 2 || b[236..240] != MAGIC {
            return None;
        }
        let ip = |o: usize| {
            let mut a = [0u8; 4];
            a.copy_from_slice(&b[o..o + 4]);
            Ipv4Addr(a)
        };
        let mut m = Message {
            msg_type: 0,
            xid: be32(&b[4..8]),
            yiaddr: ip(16),
            config: Config {
                address: ip(16),
                ..Config::default()
            },
        };
        let mut i = FIXED_LEN;
        while i < b.len() {
            let code = b[i];
            if code == OPT_END {
                break;
            }
            if code == 0 {
                i += 1;
                continue;
            }
            let len = *b.get(i + 1)? as usize;
            let val = b.get(i + 2..i + 2 + len)?;
            let ipv = || {
                let mut a = [0u8; 4];
                a.copy_from_slice(&val[..4]);
                Ipv4Addr(a)
            };
            match code {
                OPT_MSG_TYPE if len == 1 => m.msg_type = val[0],
                OPT_SUBNET if len == 4 => m.config.subnet_mask = ipv(),
                OPT_ROUTER if len >= 4 => m.config.router = ipv(),
                OPT_DNS if len >= 4 => m.config.dns = ipv(),
                OPT_SERVER_ID if len == 4 => m.config.server = ipv(),
                OPT_LEASE if len == 4 => m.config.lease_secs = be32(val),
                _ => {}
            }
            i += 2 + len;
        }
        (m.msg_type != 0).then_some(m)
    }
}

fn write_message(
    xid: u32,
    mac: MacAddr,
    msg_type: u8,
    requested: Option<Ipv4Addr>,
    server: Option<Ipv4Addr>,
    out: &mut [u8],
) -> Option<usize> {
    if out.len() < FIXED_LEN + 32 {
        return None;
    }
    out[..FIXED_LEN].fill(0);
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&MAGIC);
    let mut i = FIXED_LEN;
    let mut put = |code: u8, val: &[u8]| {
        out[i] = code;
        out[i + 1] = val.len() as u8;
        out[i + 2..i + 2 + val.len()].copy_from_slice(val);
        i += 2 + val.len();
    };
    put(OPT_MSG_TYPE, &[msg_type]);
    if let Some(r) = requested {
        put(OPT_REQUESTED_IP, &r.0);
    }
    if let Some(s) = server {
        put(OPT_SERVER_ID, &s.0);
    }
    put(OPT_PARAM_LIST, &[OPT_SUBNET, OPT_ROUTER, OPT_DNS]);
    out[i] = OPT_END;
    i += 1;
    // Padding up to the usual BOOTP minimum (300 bytes).
    let len = i.max(300);
    out[i..len].fill(0);
    Some(len)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Init,
    Selecting,
    Requesting,
    /// Acknowledged; the caller probes the address with ARP (RFC 2131
    /// 4.4.1) and then calls `address_free` or `address_in_use`.
    Probing,
    Bound,
}

/// Client state machine. The caller sends whatever `poll`/`on_message`
/// return in a broadcast UDP 68→67 datagram (0.0.0.0 → 255.255.255.255).
pub struct Client {
    mac: MacAddr,
    xid: u32,
    state: State,
    offer: Option<Message>,
    config: Option<Config>,
    retries: u8,
}

impl Client {
    pub fn new(mac: MacAddr, xid: u32) -> Self {
        Self {
            mac,
            xid,
            state: State::Init,
            offer: None,
            config: None,
            retries: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The acknowledged address while it is being probed.
    pub fn probing(&self) -> Option<Ipv4Addr> {
        (self.state == State::Probing)
            .then(|| self.config.map(|c| c.address))
            .flatten()
    }

    /// Nobody answered the probe: the lease is in use.
    pub fn address_free(&mut self) {
        if self.state == State::Probing {
            self.state = State::Bound;
        }
    }

    /// Another host answered the probe: `DHCPDECLINE` for the server (the
    /// message to broadcast) and back to the start; the caller should wait
    /// about ten seconds before polling again (RFC 2131 3.1.5).
    pub fn address_in_use(&mut self, out: &mut [u8]) -> Option<usize> {
        let cfg = self.config.take()?;
        self.state = State::Init;
        self.offer = None;
        write_message(
            self.xid,
            self.mac,
            DECLINE,
            Some(cfg.address),
            Some(cfg.server),
            out,
        )
    }

    pub fn config(&self) -> Option<Config> {
        self.config
    }

    pub fn retries(&self) -> u8 {
        self.retries
    }

    /// Message due to be sent now (at startup or when a timeout expires
    /// without a reply). `None` once bound.
    pub fn poll(&mut self, out: &mut [u8]) -> Option<usize> {
        match self.state {
            State::Init | State::Selecting => {
                if self.state == State::Selecting {
                    // Saturating: with no server, it grows for the whole
                    // boot (audit 2026-10-25 B, L2: a debug build panicked
                    // at 256).
                    self.retries = self.retries.saturating_add(1);
                }
                self.state = State::Selecting;
                write_message(self.xid, self.mac, DISCOVER, None, None, out)
            }
            State::Requesting => {
                self.retries = self.retries.saturating_add(1);
                let offer = self.offer?;
                write_message(
                    self.xid,
                    self.mac,
                    REQUEST,
                    Some(offer.yiaddr),
                    Some(offer.config.server),
                    out,
                )
            }
            State::Probing | State::Bound => None,
        }
    }

    /// Processes a received message; returns the reply to send, if any.
    pub fn on_message(&mut self, b: &[u8], out: &mut [u8]) -> Option<usize> {
        let m = Message::parse(b)?;
        if m.xid != self.xid {
            return None;
        }
        match (self.state, m.msg_type) {
            (State::Selecting, OFFER) => {
                self.offer = Some(m);
                self.state = State::Requesting;
                write_message(
                    self.xid,
                    self.mac,
                    REQUEST,
                    Some(m.yiaddr),
                    Some(m.config.server),
                    out,
                )
            }
            (State::Requesting, ACK) => {
                let mut cfg = m.config;
                if cfg.server.is_unspecified() {
                    cfg.server = self.offer.map(|o| o.config.server).unwrap_or_default();
                }
                self.config = Some(cfg);
                self.state = State::Probing;
                None
            }
            (State::Requesting, NAK) => {
                self.state = State::Init;
                self.offer = None;
                self.poll(out)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_message(xid: u32, msg_type: u8, yiaddr: Ipv4Addr) -> [u8; 320] {
        let mut b = [0u8; 320];
        b[0] = 2;
        b[4..8].copy_from_slice(&xid.to_be_bytes());
        b[16..20].copy_from_slice(&yiaddr.0);
        b[236..240].copy_from_slice(&MAGIC);
        let opts: &[u8] = &[
            OPT_MSG_TYPE,
            1,
            msg_type,
            OPT_SUBNET,
            4,
            255,
            255,
            255,
            0,
            OPT_ROUTER,
            4,
            10,
            0,
            2,
            2,
            OPT_DNS,
            4,
            10,
            0,
            2,
            3,
            OPT_SERVER_ID,
            4,
            10,
            0,
            2,
            2,
            OPT_LEASE,
            4,
            0,
            1,
            0x51,
            0x80,
            OPT_END,
        ];
        b[240..240 + opts.len()].copy_from_slice(opts);
        b
    }

    #[test]
    fn full_handshake() {
        let mac = MacAddr([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        let mut c = Client::new(mac, 0xABCD);
        let mut out = [0u8; MAX_LEN];
        let n = c.poll(&mut out).unwrap();
        assert!(n >= 300);
        assert_eq!(out[0], 1);
        assert_eq!(out[FIXED_LEN..FIXED_LEN + 3], [OPT_MSG_TYPE, 1, DISCOVER]);
        assert_eq!(c.state(), State::Selecting);
        // An OFFER with another xid is ignored.
        assert!(c
            .on_message(
                &server_message(1, OFFER, Ipv4Addr::new(10, 0, 2, 15)),
                &mut out
            )
            .is_none());
        let n = c
            .on_message(
                &server_message(0xABCD, OFFER, Ipv4Addr::new(10, 0, 2, 15)),
                &mut out,
            )
            .unwrap();
        assert_eq!(c.state(), State::Requesting);
        assert_eq!(out[FIXED_LEN..FIXED_LEN + 3], [OPT_MSG_TYPE, 1, REQUEST]);
        assert_eq!(
            &out[FIXED_LEN + 3..FIXED_LEN + 9],
            &[OPT_REQUESTED_IP, 4, 10, 0, 2, 15]
        );
        assert!(n >= 300);
        assert!(c
            .on_message(
                &server_message(0xABCD, ACK, Ipv4Addr::new(10, 0, 2, 15)),
                &mut out
            )
            .is_none());
        assert_eq!(c.state(), State::Probing);
        assert_eq!(c.probing(), Some(Ipv4Addr::new(10, 0, 2, 15)));
        assert!(c.poll(&mut out).is_none(), "nothing to send while probing");
        c.address_free();
        assert_eq!(c.state(), State::Bound);
        assert_eq!(c.probing(), None);
        let cfg = c.config().unwrap();
        assert_eq!(cfg.address, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(cfg.subnet_mask, Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(cfg.router, Ipv4Addr::new(10, 0, 2, 2));
        assert_eq!(cfg.dns, Ipv4Addr::new(10, 0, 2, 3));
        assert_eq!(cfg.lease_secs, 86400);
        assert!(c.poll(&mut out).is_none());
    }

    #[test]
    fn retries_and_nak() {
        let mut c = Client::new(MacAddr([1; 6]), 7);
        let mut out = [0u8; MAX_LEN];
        c.poll(&mut out).unwrap();
        c.poll(&mut out).unwrap(); // no reply: retry
        assert_eq!(c.retries(), 1);
        c.on_message(
            &server_message(7, OFFER, Ipv4Addr::new(10, 0, 2, 15)),
            &mut out,
        )
        .unwrap();
        let n = c.on_message(&server_message(7, NAK, Ipv4Addr::UNSPECIFIED), &mut out);
        assert!(n.is_some(), "after NAK it goes back to DISCOVER");
        assert_eq!(c.state(), State::Selecting);
        assert!(Message::parse(&[0u8; 100]).is_none());
    }

    #[test]
    fn address_in_use_is_declined() {
        let mut c = Client::new(MacAddr([1; 6]), 9);
        let mut out = [0u8; MAX_LEN];
        c.poll(&mut out).unwrap();
        c.on_message(
            &server_message(9, OFFER, Ipv4Addr::new(10, 0, 2, 15)),
            &mut out,
        )
        .unwrap();
        c.on_message(
            &server_message(9, ACK, Ipv4Addr::new(10, 0, 2, 15)),
            &mut out,
        );
        assert_eq!(c.probing(), Some(Ipv4Addr::new(10, 0, 2, 15)));
        let n = c.address_in_use(&mut out).unwrap();
        assert!(n >= 300);
        assert_eq!(out[FIXED_LEN..FIXED_LEN + 3], [OPT_MSG_TYPE, 1, DECLINE]);
        assert!(
            out[FIXED_LEN..n]
                .windows(6)
                .any(|w| w == [OPT_REQUESTED_IP, 4, 10, 0, 2, 15]),
            "the declined address goes in the message"
        );
        assert_eq!(c.state(), State::Init);
        assert!(c.config().is_none());
        // Next poll starts over with DISCOVER.
        let n = c.poll(&mut out).unwrap();
        assert_eq!(out[FIXED_LEN..FIXED_LEN + 3], [OPT_MSG_TYPE, 1, DISCOVER]);
        assert!(n >= 300);
    }
}
