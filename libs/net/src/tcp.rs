// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! TCP (RFC 9293) without `alloc`: header and options, and a connection
//! over buffers its caller supplies (ADR 0070), with:
//! - a retransmission timeout from measured round trips (RFC 6298, Karn's
//!   rule), and every unacknowledged segment resent after it;
//! - slow start, congestion avoidance, fast retransmit and NewReno fast
//!   recovery (RFC 5681, RFC 6582), with the holes to resend chosen from
//!   the peer's SACK blocks when both sides permit them (RFC 2018);
//! - segments that arrive out of order kept and reported with SACK;
//! - the peer's MSS, window probes and delayed acknowledgements;
//! - segments checked against blind resets and injection (RFC 5961).
//!
//! The connection sends nothing on its own: `poll` produces at most one
//! segment per call and the caller transmits it. Initial sequence numbers
//! come from the caller ([`crate::isn`], RFC 6528): the connection never
//! derives one from the clock or from the peer's number.

use crate::ipv4::{pseudo_header_sum, PROTO_TCP};
use crate::{be16, be32, checksum, Ipv4Addr};

pub const HEADER_LEN: usize = 20;
pub const FLAG_FIN: u8 = 0x01;
pub const FLAG_SYN: u8 = 0x02;
pub const FLAG_RST: u8 = 0x04;
pub const FLAG_PSH: u8 = 0x08;
pub const FLAG_ACK: u8 = 0x10;

/// Maximum segment size we announce, and the largest we send. With 40
/// bytes of headers and up to 36 of options a segment stays under the
/// 1500-byte Ethernet MTU.
pub const MSS: usize = 1400;
/// The peer's MSS when its SYN announces none (RFC 9293 section 3.7.1).
const DEFAULT_PEER_MSS: usize = 536;
/// SACK blocks parsed from a segment, and the most we send.
pub const MAX_SACK: usize = 4;
/// Longest options we write: four SACK blocks (2 + 2 + 32 bytes).
pub const MAX_OPTIONS: usize = 36;
/// Largest segment `poll` produces (header, options and data).
pub const MAX_SEGMENT: usize = HEADER_LEN + MAX_OPTIONS + MSS;
/// Buffer sizes the service gives each connection (ADR 0070): the
/// receive window field holds 65 535 bytes without window scaling.
pub const SEND_BUF: usize = 32 * 1024;
pub const RECV_BUF: usize = 64 * 1024;
const INITIAL_RTO_MS: u64 = 500;
/// Floor of the measured timeout, Linux's: RFC 6298's 1 s was chosen for
/// clocks coarser than this one.
const MIN_RTO_MS: u64 = 200;
const MAX_RTO_MS: u64 = 8000;
/// How long a SYN, data, a FIN or a window probe may go unanswered, from
/// when it was first sent, before the connection gives up
/// (`CloseReason::Timeout`):
/// the schedule of six retransmissions with the RTO doubling from 500 ms
/// to 8 s (0.5, 1, 2, 4, 8, 8, 8 s). A time, not a count, so a measured
/// RTO of 200 ms does not make it shorter (Linux turns `tcp_retries2`
/// into a time the same way).
pub const GIVE_UP_MS: u64 = 500 + 1000 + 2000 + 4000 + 8000 + 8000 + 8000;
/// Retransmissions of the SYN|ACK of a passive connection before it gives
/// up (incident 0013, ADR 0035): 0.5, 1, 2 s and a last wait of 4 s, about
/// 7.5 s in all. A SYN whose sender never answers the SYN|ACK (unreachable,
/// spoofed, or a scanner) must not hold one of the service's few slots for
/// the full `GIVE_UP_MS` schedule (31 s); three retransmissions still
/// cover a lost SYN|ACK and a slow LAN peer. Linux keeps five
/// (`tcp_synack_retries`, about 63 s) with a table of thousands of slots.
pub const SYN_ACK_MAX_RETRIES: u8 = 3;
const TIME_WAIT_MS: u64 = 2000;
/// Longest wait in FIN-WAIT-2 for the peer's FIN once ours is acknowledged
/// (audit of incident 0027, M1): a peer that never closes its side would
/// otherwise hold the slot until the next boot. The service's close is
/// always a full close, so nothing more can arrive for anyone; Linux waits
/// 60 s (`tcp_fin_timeout`) with a table of thousands of slots.
pub const FIN_WAIT2_MS: u64 = 30_000;
/// Longest a closed connection may take to deliver its last data and FIN
/// and have the FIN acknowledged (FIN-WAIT-1, CLOSING, LAST-ACK): a peer
/// that answers just enough to keep it from timing out (a zero window, or
/// a FIN that crosses ours while data is pending) would otherwise hold the
/// slot until the next boot, and eight such peers every TCP slot, sshd's
/// included (audit 2026-10-25 B, L3). Ends in RST, as FIN-WAIT-2 does.
pub const CLOSING_MS: u64 = 30_000;
/// Longest an in-order data segment waits for its acknowledgement when no
/// second segment follows (RFC 5681 section 4.2: at most 500 ms, and at
/// least every second full segment acknowledged at once).
const DELAYED_ACK_MS: u64 = 20;
/// At most one challenge ACK per connection in this time (RFC 5961
/// section 7): a forged segment cannot make the connection send a flood
/// to the real peer.
const CHALLENGE_ACK_MS: u64 = 1000;
/// Initial congestion window in segments (RFC 6928).
const INITIAL_WINDOW: usize = 10;
/// Duplicate acknowledgements that start a fast retransmit (RFC 5681).
const DUPACK_THRESHOLD: u8 = 3;
/// Out-of-order ranges kept by the receiver, and SACKed ranges kept by
/// the sender. A segment that would need one more is dropped and
/// retransmitted by the peer.
const MAX_RANGES: usize = 4;

/// Connections a listener may hold before its owner accepts them, half-open
/// (`SynReceived`) and established alike, for a table of `table_slots`
/// (incident 0013, ADR 0035). Half of the table: a burst of SYNs to one
/// port can never take every slot, so active opens (the shell, the updater,
/// an SSH client of the owner) always find at least `table_slots / 2 - 1`
/// slots free of that listener's children (one slot is the listener). A
/// SYN beyond it is answered with a reset ([`reset_for`]) and recorded by
/// the service.
pub const fn listen_backlog(table_slots: usize) -> usize {
    table_slots / 2
}

/// Reset answering a segment of `payload_len` bytes for which no
/// connection exists (RFC 793 section 3.4, "if the connection does not
/// exist (CLOSED) then a reset is sent in response to any incoming segment
/// except another reset"): with an ACK in the segment the reset takes its
/// sequence number from the ACK field; otherwise it has sequence number
/// zero and acknowledges the segment's sequence number plus its length,
/// SYN and FIN counted, so the sender can match it. A reset is never sent
/// for a reset: the caller ignores any segment with `FLAG_RST` before it
/// decides to answer (the service does so before looking for a listener;
/// `on_segment` in LISTEN ignores it too, see
/// `listener_ignores_a_syn_carrying_rst`).
pub fn reset_for(h: &Header, payload_len: usize) -> Header {
    let (seq, ack, flags) = if h.flags & FLAG_ACK != 0 {
        (h.ack, 0, FLAG_RST)
    } else {
        let len = payload_len as u32
            + u32::from(h.flags & FLAG_SYN != 0)
            + u32::from(h.flags & FLAG_FIN != 0);
        (0, h.seq.wrapping_add(len), FLAG_RST | FLAG_ACK)
    };
    Header {
        src_port: h.dst_port,
        dst_port: h.src_port,
        seq,
        ack,
        flags,
        window: 0,
        opts: Options::default(),
    }
}

/// The options this stack reads and writes: MSS, SACK-permitted and SACK
/// blocks (RFC 9293, RFC 2018). Others (window scale, timestamps) are
/// skipped when parsed and never offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub mss: Option<u16>,
    pub sack_permitted: bool,
    /// SACK blocks `[start, end)`; the first `sack_len` are valid.
    pub sack: [(u32, u32); MAX_SACK],
    pub sack_len: u8,
}

impl Options {
    pub fn sack_blocks(&self) -> &[(u32, u32)] {
        &self.sack[..self.sack_len as usize]
    }

    /// Reads the options area. Parsing stops at an end-of-list option or
    /// at the first malformed one (a length below 2 or past the area), as
    /// Linux does; what was read before it is kept.
    fn parse(b: &[u8]) -> Options {
        let mut o = Options::default();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                0 => break,
                1 => i += 1,
                kind => {
                    let Some(&len) = b.get(i + 1) else { break };
                    let len = len as usize;
                    if len < 2 || i + len > b.len() {
                        break;
                    }
                    let body = &b[i + 2..i + len];
                    match (kind, body.len()) {
                        (2, 2) => o.mss = Some(be16(body)),
                        (4, 0) => o.sack_permitted = true,
                        (5, n) if n % 8 == 0 => {
                            for block in body
                                .as_chunks::<8>()
                                .0
                                .iter()
                                .take(MAX_SACK - o.sack_len as usize)
                            {
                                o.sack[o.sack_len as usize] =
                                    (be32(&block[..4]), be32(&block[4..]));
                                o.sack_len += 1;
                            }
                        }
                        _ => {}
                    }
                    i += len;
                }
            }
        }
        o
    }

    /// Bytes the options take on the wire, a multiple of 4.
    fn len(&self) -> usize {
        let mut n = 0;
        if self.mss.is_some() {
            n += 4;
        }
        if self.sack_permitted {
            n += 4;
        }
        if self.sack_len > 0 {
            n += 4 + 8 * self.sack_len as usize;
        }
        n
    }

    fn write(&self, out: &mut [u8]) {
        let mut i = 0;
        if let Some(m) = self.mss {
            out[i..i + 4].copy_from_slice(&[2, 4, (m >> 8) as u8, m as u8]);
            i += 4;
        }
        if self.sack_permitted {
            out[i..i + 4].copy_from_slice(&[1, 1, 4, 2]);
            i += 4;
        }
        if self.sack_len > 0 {
            out[i..i + 4].copy_from_slice(&[1, 1, 5, 2 + 8 * self.sack_len]);
            i += 4;
            for (s, e) in self.sack_blocks() {
                out[i..i + 4].copy_from_slice(&s.to_be_bytes());
                out[i + 4..i + 8].copy_from_slice(&e.to_be_bytes());
                i += 8;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub opts: Options,
}

impl Header {
    /// Parses and validates (data offset, checksum). Returns the payload.
    pub fn parse(src: Ipv4Addr, dst: Ipv4Addr, b: &[u8]) -> Option<(Header, &[u8])> {
        if b.len() < HEADER_LEN {
            return None;
        }
        let off = (b[12] >> 4) as usize * 4;
        if off < HEADER_LEN || off > b.len() {
            return None;
        }
        if checksum(pseudo_header_sum(src, dst, PROTO_TCP, b.len() as u16), b) != 0 {
            return None;
        }
        Some((
            Header {
                src_port: be16(&b[0..2]),
                dst_port: be16(&b[2..4]),
                seq: be32(&b[4..8]),
                ack: be32(&b[8..12]),
                flags: b[13],
                window: be16(&b[14..16]),
                opts: Options::parse(&b[HEADER_LEN..off]),
            },
            &b[off..],
        ))
    }

    /// Writes the complete segment (header, options, payload, checksum).
    pub fn write(
        &self,
        src: Ipv4Addr,
        dst: Ipv4Addr,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let off = HEADER_LEN + self.opts.len();
        let len = off + payload.len();
        if out.len() < len || len > u16::MAX as usize {
            return None;
        }
        out[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        out[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        out[4..8].copy_from_slice(&self.seq.to_be_bytes());
        out[8..12].copy_from_slice(&self.ack.to_be_bytes());
        out[12] = ((off / 4) as u8) << 4;
        out[13] = self.flags;
        out[14..16].copy_from_slice(&self.window.to_be_bytes());
        out[16..20].fill(0);
        self.opts.write(&mut out[HEADER_LEN..off]);
        out[off..len].copy_from_slice(payload);
        let sum = checksum(
            pseudo_header_sum(src, dst, PROTO_TCP, len as u16),
            &out[..len],
        );
        out[16..18].copy_from_slice(&sum.to_be_bytes());
        Some(len)
    }
}

fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}
fn seq_le(a: u32, b: u32) -> bool {
    !seq_lt(b, a)
}
fn seq_max(a: u32, b: u32) -> u32 {
    if seq_lt(a, b) {
        b
    } else {
        a
    }
}
fn seq_min(a: u32, b: u32) -> u32 {
    if seq_lt(a, b) {
        a
    } else {
        b
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

/// Why a connection was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Orderly close completed.
    Graceful,
    /// The peer sent RST.
    Reset,
    /// Too many retransmissions without a reply.
    Timeout,
}

/// Byte ring over a caller's buffer. Data is copied in at most two
/// slices, never byte by byte.
struct Ring<B> {
    buf: B,
    head: usize,
    len: usize,
}

impl<B: AsRef<[u8]> + AsMut<[u8]>> Ring<B> {
    fn new(buf: B) -> Self {
        Self {
            buf,
            head: 0,
            len: 0,
        }
    }
    fn cap(&self) -> usize {
        self.buf.as_ref().len()
    }
    fn free(&self) -> usize {
        self.cap() - self.len
    }
    /// Where the byte `offset` places after the first one held lives.
    fn index(&self, offset: usize) -> usize {
        (self.head + offset) % self.cap()
    }
    /// Copies `data` to `offset` bytes after the first byte held, without
    /// changing how much is held: `offset + data.len() <= cap`.
    fn write_at(&mut self, offset: usize, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let start = self.index(offset);
        let first = data.len().min(self.cap() - start);
        let buf = self.buf.as_mut();
        buf[start..start + first].copy_from_slice(&data[..first]);
        buf[..data.len() - first].copy_from_slice(&data[first..]);
    }
    /// Copies `out.len()` bytes from `offset`, which must be held.
    fn read_at(&self, offset: usize, out: &mut [u8]) {
        if out.is_empty() {
            return;
        }
        let start = self.index(offset);
        let first = out.len().min(self.cap() - start);
        let buf = self.buf.as_ref();
        out[..first].copy_from_slice(&buf[start..start + first]);
        let rest = out.len() - first;
        out[first..].copy_from_slice(&buf[..rest]);
    }
    fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free());
        self.write_at(self.len, &data[..n]);
        self.len += n;
        n
    }
    /// Counts `n` more bytes as held: they were written with `write_at`.
    fn extend(&mut self, n: usize) {
        self.len += n;
    }
    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = self.peek(0, out);
        self.drop_front(n);
        n
    }
    fn drop_front(&mut self, n: usize) {
        let n = n.min(self.len);
        if n > 0 {
            self.head = self.index(n);
            self.len -= n;
        }
    }
    /// Copies `[offset, offset + out.len())` without consuming.
    fn peek(&self, offset: usize, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len.saturating_sub(offset));
        self.read_at(offset, &mut out[..n]);
        n
    }
}

/// Up to `MAX_RANGES` disjoint sequence ranges `[start, end)`, sorted, and
/// which one changed last (RFC 2018: the first SACK block reports the most
/// recent segment).
#[derive(Clone, Copy)]
struct Ranges {
    r: [(u32, u32); MAX_RANGES],
    n: usize,
    latest: usize,
}

impl Ranges {
    const fn new() -> Self {
        Self {
            r: [(0, 0); MAX_RANGES],
            n: 0,
            latest: 0,
        }
    }
    fn as_slice(&self) -> &[(u32, u32)] {
        &self.r[..self.n]
    }
    fn clear(&mut self) {
        self.n = 0;
    }
    /// Adds `[s, e)`, merging what it touches. `false`, and nothing
    /// changed, when it would need a range more than there is room for.
    fn insert(&mut self, s: u32, e: u32) -> bool {
        if !seq_lt(s, e) {
            return true;
        }
        let (mut s, mut e) = (s, e);
        let mut out = [(0u32, 0u32); MAX_RANGES + 1];
        let mut k = 0;
        let mut placed = None;
        for &(a, b) in self.as_slice() {
            if seq_lt(b, s) {
                out[k] = (a, b);
                k += 1;
            } else if seq_lt(e, a) {
                if placed.is_none() {
                    placed = Some(k);
                    out[k] = (s, e);
                    k += 1;
                }
                out[k] = (a, b);
                k += 1;
            } else {
                // Overlapping or adjacent: absorb.
                if seq_lt(a, s) {
                    s = a;
                }
                if seq_lt(e, b) {
                    e = b;
                }
            }
        }
        if placed.is_none() {
            placed = Some(k);
            out[k] = (s, e);
            k += 1;
        }
        if k > MAX_RANGES {
            return false;
        }
        self.r[..k].copy_from_slice(&out[..k]);
        self.n = k;
        self.latest = placed.unwrap_or(0);
        true
    }
    /// Forgets everything below `seq`.
    fn trim_below(&mut self, seq: u32) {
        let mut k = 0;
        let mut latest = 0;
        for i in 0..self.n {
            let (a, b) = self.r[i];
            if seq_le(b, seq) {
                continue;
            }
            if i == self.latest {
                latest = k;
            }
            self.r[k] = (seq_max(a, seq), b);
            k += 1;
        }
        self.n = k;
        self.latest = latest;
    }
    /// End of the first range when it starts at or before `seq`, removing it.
    fn take_front(&mut self, seq: u32) -> Option<u32> {
        if self.n == 0 || seq_lt(seq, self.r[0].0) {
            return None;
        }
        let end = self.r[0].1;
        self.r.copy_within(1..self.n, 0);
        self.n -= 1;
        self.latest = self.latest.saturating_sub(1);
        Some(end)
    }
    fn highest(&self) -> Option<u32> {
        self.as_slice().last().map(|r| r.1)
    }
    /// The blocks to report, the latest first.
    fn sack(&self) -> ([(u32, u32); MAX_SACK], u8) {
        let mut out = [(0, 0); MAX_SACK];
        let mut k = 0;
        if self.n > 0 {
            out[0] = self.r[self.latest];
            k = 1;
            for (i, &r) in self.as_slice().iter().enumerate() {
                if i != self.latest && k < MAX_SACK {
                    out[k] = r;
                    k += 1;
                }
            }
        }
        (out, k as u8)
    }
}

/// One TCP connection over a send and a receive buffer of the caller's
/// (`B`: an array, a slice, a boxed slice). Their sizes set how much the
/// connection holds unacknowledged and the window it announces.
pub struct Connection<B> {
    local_ip: Ipv4Addr,
    remote_ip: Ipv4Addr,
    local_port: u16,
    remote_port: u16,
    state: State,
    reason: Option<CloseReason>,

    /// First unacknowledged byte and next byte to send. After a timeout
    /// `snd_nxt` goes back to `snd_una` and `snd_max` keeps the highest
    /// sequence number sent.
    snd_una: u32,
    snd_nxt: u32,
    snd_max: u32,
    /// Window announced by the peer, the largest it ever announced
    /// (RFC 5961 section 5.2), and the segment that last updated it.
    snd_wnd: u32,
    max_snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    /// Data accepted and not yet acknowledged (from `snd_una`).
    send: Ring<B>,
    /// Largest payload we send: the peer's MSS, at most ours.
    smss: usize,
    /// The peer's SYN offered SACK: we report out-of-order data with it
    /// and use the blocks it sends.
    sack_ok: bool,
    /// Ranges above `snd_una` the peer reported received.
    sacked: Ranges,
    cwnd: usize,
    ssthresh: usize,
    /// Bytes acknowledged towards the next congestion avoidance increase.
    ca_acked: usize,
    dupacks: u8,
    /// Fast recovery runs until `recover` is acknowledged (RFC 6582); a
    /// new one starts only above it.
    in_recovery: bool,
    recover: u32,
    /// Holes below this were already resent in this recovery.
    rexmit_next: u32,
    /// A hole is to be resent in the next `poll`.
    rexmit_due: bool,
    /// Smoothed round trip time ×8 and variance ×4 in ms (RFC 6298).
    srtt8: u64,
    rttvar4: u64,
    has_rtt: bool,
    /// Sequence number being timed and when it left (Karn: never a
    /// retransmitted one).
    timed: Option<(u32, u64)>,
    rto_ms: u64,
    retransmit_at: Option<u64>,
    retries: u8,
    /// When the oldest unacknowledged segment of a series of timeouts was
    /// sent: the connection gives up `GIVE_UP_MS` after it.
    series_start: Option<u64>,
    /// When the next window probe goes out (zero window, data waiting).
    persist_at: Option<u64>,

    rcv_nxt: u32,
    recv: Ring<B>,
    /// Data held beyond `rcv_nxt`, waiting for what is missing before it.
    ooo: Ranges,
    /// Sequence number of a FIN that arrived ahead of data still missing.
    fin_at: Option<u32>,
    /// Right edge of the window last announced.
    rcv_adv: u32,

    /// SYN must be sent (active, or passive SYN|ACK).
    syn_pending: bool,
    /// The user asked to close: FIN after the data.
    fin_queued: bool,
    /// Our FIN has been given its sequence number, `snd_una + send.len`
    /// once the data is acknowledged.
    fin_sent: bool,
    /// The peer closed its side.
    peer_fin: bool,
    /// Send an acknowledgement in this round.
    ack_now: bool,
    /// In-order data segments not yet acknowledged, and when the delayed
    /// acknowledgement for them is due.
    unacked_segments: u8,
    delayed_ack_at: Option<u64>,
    /// No challenge ACK before this time.
    challenge_after: u64,
    /// Send RST|ACK in the next `poll` (after `abort` or a deadline).
    rst_pending: bool,
    /// Answer an unacceptable acknowledgement with RST at this sequence
    /// number (RFC 9293: SYN-SENT and SYN-RECEIVED).
    reply_rst: Option<u32>,
    time_wait_until: u64,
    /// When FIN-WAIT-2 gives up on the peer's FIN (`FIN_WAIT2_MS`).
    fin_wait2_until: u64,
    /// When a connection closed on our side gives up delivering its data
    /// and FIN (`CLOSING_MS`); set at the first `poll` after `close`.
    closing_until: Option<u64>,
}

impl<B: AsRef<[u8]> + AsMut<[u8]>> Connection<B> {
    pub fn new(
        local_ip: Ipv4Addr,
        local_port: u16,
        remote_ip: Ipv4Addr,
        remote_port: u16,
        send_buf: B,
        recv_buf: B,
    ) -> Self {
        Self {
            local_ip,
            remote_ip,
            local_port,
            remote_port,
            state: State::Closed,
            reason: None,
            snd_una: 0,
            snd_nxt: 0,
            snd_max: 0,
            snd_wnd: 0,
            max_snd_wnd: 0,
            snd_wl1: 0,
            snd_wl2: 0,
            send: Ring::new(send_buf),
            smss: DEFAULT_PEER_MSS,
            sack_ok: false,
            sacked: Ranges::new(),
            cwnd: INITIAL_WINDOW * DEFAULT_PEER_MSS,
            ssthresh: usize::MAX,
            ca_acked: 0,
            dupacks: 0,
            in_recovery: false,
            recover: 0,
            rexmit_next: 0,
            rexmit_due: false,
            srtt8: 0,
            rttvar4: 0,
            has_rtt: false,
            timed: None,
            rto_ms: INITIAL_RTO_MS,
            retransmit_at: None,
            retries: 0,
            series_start: None,
            persist_at: None,
            rcv_nxt: 0,
            recv: Ring::new(recv_buf),
            ooo: Ranges::new(),
            fin_at: None,
            rcv_adv: 0,
            syn_pending: false,
            fin_queued: false,
            fin_sent: false,
            peer_fin: false,
            ack_now: false,
            unacked_segments: 0,
            delayed_ack_at: None,
            challenge_after: 0,
            rst_pending: false,
            reply_rst: None,
            time_wait_until: 0,
            fin_wait2_until: 0,
            closing_until: None,
        }
    }

    /// Gives the buffers back to their owner, leaving empty ones (the
    /// service returns them to its pool when a socket goes away).
    pub fn take_buffers(&mut self) -> (B, B)
    where
        B: Default,
    {
        let s = core::mem::take(&mut self.send.buf);
        let r = core::mem::take(&mut self.recv.buf);
        self.send = Ring::new(B::default());
        self.recv = Ring::new(B::default());
        (s, r)
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.reason
    }
    pub fn remote(&self) -> (Ipv4Addr, u16) {
        (self.remote_ip, self.remote_port)
    }
    pub fn local_port(&self) -> u16 {
        self.local_port
    }
    /// The connection has fully ended (TIME-WAIT included).
    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }
    pub fn is_established(&self) -> bool {
        matches!(self.state, State::Established | State::CloseWait)
    }
    /// Bytes ready for `recv`.
    pub fn recv_available(&self) -> usize {
        self.recv.len
    }
    /// The peer closed and there is nothing left to read.
    pub fn recv_finished(&self) -> bool {
        self.peer_fin && self.recv.len == 0
    }
    pub fn send_capacity(&self) -> usize {
        self.send.free()
    }
    pub fn retries(&self) -> u8 {
        self.retries
    }
    /// The current retransmission timeout.
    pub fn rto_ms(&self) -> u64 {
        self.rto_ms
    }

    /// Active open: sends SYN on the next `poll`.
    pub fn connect(&mut self, iss: u32, now: u64) {
        self.set_iss(iss);
        self.state = State::SynSent;
        self.syn_pending = true;
        self.arm_timer(now);
    }

    /// Passive open: waits for a SYN and answers it with SYN|ACK carrying
    /// `iss` as our initial sequence number (`isn::initial_seq` for this
    /// connection's 4-tuple at the time of the SYN; RFC 6528 section 3, not
    /// a function of the peer's number). A listener whose SYNs the caller
    /// hands to connections of their own never sends it.
    pub fn listen(&mut self, iss: u32) {
        self.set_iss(iss);
        self.state = State::Listen;
    }

    fn set_iss(&mut self, iss: u32) {
        self.snd_una = iss;
        self.snd_nxt = iss;
        self.snd_max = iss;
        self.recover = iss;
        self.rexmit_next = iss;
    }

    /// Accepts user data; returns how much fits.
    pub fn send(&mut self, data: &[u8]) -> usize {
        if !self.is_established() || self.fin_queued {
            return 0;
        }
        self.send.push(data)
    }

    /// Delivers received data in order. Announces the window it frees
    /// once that is worth a segment of its own (RFC 9293 section
    /// 3.8.6.2.2, silly window avoidance): two full segments or half the
    /// buffer. A delayed acknowledgement goes at once when the reader has
    /// taken everything (Linux's `tcp_cleanup_rbuf`): a sender holding a
    /// short segment back for it (Nagle) would otherwise wait out the
    /// delay at every exchange.
    pub fn recv(&mut self, out: &mut [u8]) -> usize {
        let n = self.recv.pop(out);
        if n > 0 && self.recv.len == 0 && self.unacked_segments > 0 {
            self.ack_now = true;
        }
        if n > 0 {
            let edge = self.rcv_nxt.wrapping_add(u32::from(self.window()));
            let grown = edge.wrapping_sub(self.rcv_adv) as usize;
            if seq_lt(self.rcv_adv, edge) && grown >= (2 * MSS).min(self.recv.cap() / 2) {
                self.ack_now = true;
            }
        }
        n
    }

    /// Orderly close: FIN after draining the send buffer.
    pub fn close(&mut self) {
        match self.state {
            State::Closed | State::Listen => self.finish(CloseReason::Graceful),
            State::SynSent => self.finish(CloseReason::Graceful),
            State::Established => {
                self.fin_queued = true;
                self.state = State::FinWait1;
            }
            // The FIN waits for our SYN to be acknowledged; until then the
            // SYN|ACK and its short budget stay (audit of ADR 0070, L1).
            State::SynReceived => self.fin_queued = true,
            State::CloseWait => {
                self.fin_queued = true;
                self.state = State::LastAck;
            }
            _ => {}
        }
    }

    /// Aborts: the next `poll` emits RST and the connection is closed.
    pub fn abort(&mut self) {
        if self.state != State::Closed {
            self.reason = Some(CloseReason::Reset);
            self.state = State::Closed;
            self.rst_pending = true;
        }
    }

    fn finish(&mut self, reason: CloseReason) {
        self.state = State::Closed;
        self.reason = Some(reason);
        self.retransmit_at = None;
        self.persist_at = None;
        self.delayed_ack_at = None;
        self.ack_now = false;
    }

    fn arm_timer(&mut self, now: u64) {
        if self.retransmit_at.is_none() {
            self.retransmit_at = Some(now + self.rto_ms);
        }
    }

    fn window(&self) -> u16 {
        self.recv.free().min(u16::MAX as usize) as u16
    }

    /// Sequence number one past the last byte of data queued. The SYN is
    /// acknowledged before any data is queued.
    fn data_end(&self) -> u32 {
        self.snd_una.wrapping_add(self.send.len as u32)
    }

    fn header(&self, flags: u8, seq: u32) -> Header {
        Header {
            src_port: self.local_port,
            dst_port: self.remote_port,
            seq,
            ack: self.rcv_nxt,
            flags,
            window: self.window(),
            opts: Options::default(),
        }
    }

    /// SACK blocks for an acknowledgement, when the peer offered SACK and
    /// data is held out of order.
    fn ack_options(&self) -> Options {
        let mut o = Options::default();
        if self.sack_ok && self.ooo.n > 0 {
            let (blocks, n) = self.ooo.sack();
            o.sack = blocks;
            o.sack_len = n;
        }
        o
    }

    /// Writes the segment; one carrying ACK settles every acknowledgement
    /// owed.
    fn emit(&mut self, mut h: Header, payload: &[u8], out: &mut [u8]) -> Option<usize> {
        if h.flags & FLAG_ACK != 0 {
            if h.opts == Options::default() {
                h.opts = self.ack_options();
            }
            self.ack_now = false;
            self.unacked_segments = 0;
            self.delayed_ack_at = None;
            self.rcv_adv = self.rcv_nxt.wrapping_add(u32::from(h.window));
        }
        h.write(self.local_ip, self.remote_ip, payload, out)
    }

    /// Bytes sent and not yet acknowledged, counting from `snd_nxt`.
    fn inflight(&self) -> usize {
        self.snd_nxt.wrapping_sub(self.snd_una) as usize
    }

    fn rtt_sample(&mut self, r: u64) {
        let r = r.min(60_000);
        if self.has_rtt {
            let srtt = self.srtt8 / 8;
            let delta = srtt.abs_diff(r);
            self.rttvar4 = self.rttvar4 - self.rttvar4 / 4 + delta;
            self.srtt8 = self.srtt8 - self.srtt8 / 8 + r;
        } else {
            self.srtt8 = r * 8;
            self.rttvar4 = r * 2;
            self.has_rtt = true;
        }
        self.rto_ms = (self.srtt8 / 8 + self.rttvar4.max(1)).clamp(MIN_RTO_MS, MAX_RTO_MS);
    }

    /// The handshake's round trip, when the SYN was not retransmitted;
    /// otherwise the initial timeout again (ADR 0035's schedule; RFC 6298
    /// section 5.7 would take 3 s).
    fn established_rtt(&mut self, now: u64) {
        match self.timed.take() {
            Some((_, t)) => self.rtt_sample(now - t),
            None => self.rto_ms = INITIAL_RTO_MS,
        }
    }

    fn challenge(&mut self, now: u64) {
        if now >= self.challenge_after {
            self.ack_now = true;
            self.challenge_after = now + CHALLENGE_ACK_MS;
        }
    }

    /// Next segment to transmit, if any. Call until `None`.
    pub fn poll(&mut self, now: u64, out: &mut [u8]) -> Option<usize> {
        // A peer that never sends its FIN, or keeps ours from being
        // delivered and acknowledged: reset and let the slot go.
        if matches!(
            self.state,
            State::FinWait1 | State::Closing | State::LastAck
        ) {
            let until = *self.closing_until.get_or_insert(now + CLOSING_MS);
            if now >= until {
                self.finish(CloseReason::Timeout);
                self.rst_pending = true;
            }
        }
        if self.state == State::FinWait2 && now >= self.fin_wait2_until {
            self.finish(CloseReason::Timeout);
            self.rst_pending = true;
        }
        // RST after abort or a deadline.
        if self.state == State::Closed {
            if self.rst_pending {
                self.rst_pending = false;
                let h = self.header(FLAG_RST | FLAG_ACK, self.snd_max);
                return h.write(self.local_ip, self.remote_ip, &[], out);
            }
            return None;
        }
        if let Some(seq) = self.reply_rst.take() {
            let mut h = self.header(FLAG_RST, seq);
            h.ack = 0;
            h.window = 0;
            return h.write(self.local_ip, self.remote_ip, &[], out);
        }
        if self.state == State::TimeWait && now >= self.time_wait_until {
            self.finish(CloseReason::Graceful);
            return None;
        }
        if self.series_start.is_some_and(|t| now >= t + GIVE_UP_MS) {
            self.finish(CloseReason::Timeout);
            return None;
        }
        if self.retransmit_at.is_some_and(|at| now >= at) {
            return self.on_timeout(now, out);
        }
        if self.persist_at.is_some_and(|at| now >= at) {
            // Window probe: an old sequence number the peer answers with
            // an ACK carrying its window (as Linux probes). Unanswered, it
            // gives up like a retransmission.
            self.retries += 1;
            self.series_start.get_or_insert(now);
            self.persist_at = Some(now + (self.rto_ms << self.retries.min(5)).min(MAX_RTO_MS));
            let h = self.header(FLAG_ACK, self.snd_una.wrapping_sub(1));
            return self.emit(h, &[], out);
        }
        // Initial SYN.
        if self.syn_pending {
            self.syn_pending = false;
            let flags = if self.state == State::SynReceived {
                FLAG_SYN | FLAG_ACK
            } else {
                FLAG_SYN
            };
            let mut h = self.header(flags, self.snd_una);
            h.opts.mss = Some(MSS as u16);
            // Offered in an active SYN, echoed in a SYN|ACK (RFC 2018).
            h.opts.sack_permitted = self.state == State::SynSent || self.sack_ok;
            if self.snd_max == self.snd_una {
                self.timed = Some((self.snd_una, now));
            }
            self.snd_nxt = self.snd_una.wrapping_add(1);
            self.snd_max = self.snd_nxt;
            self.arm_timer(now);
            return self.emit(h, &[], out);
        }
        if matches!(
            self.state,
            State::Established
                | State::CloseWait
                | State::FinWait1
                | State::Closing
                | State::LastAck
        ) {
            if self.rexmit_due && self.in_recovery {
                self.rexmit_due = false;
                if let Some(seg) = self.retransmit_hole(now, out) {
                    return Some(seg);
                }
            }
            if let Some(seg) = self.send_new(now, out) {
                return Some(seg);
            }
        }
        let ack_due = self.ack_now || self.delayed_ack_at.is_some_and(|at| now >= at);
        if ack_due && !matches!(self.state, State::Listen | State::SynSent) {
            // At `snd_max`, not `snd_nxt`: after a timeout `snd_nxt` goes
            // back below what the peer has received, and a segment there
            // is out of its window, its ACK ignored (audit of ADR 0070,
            // H1; BSD does the same).
            let h = self.header(FLAG_ACK, self.snd_max);
            return self.emit(h, &[], out);
        }
        None
    }

    /// The retransmission timer ran out: back off, collapse the window to
    /// one segment and resend from `snd_una` (RFC 6298, RFC 5681).
    fn on_timeout(&mut self, now: u64, out: &mut [u8]) -> Option<usize> {
        self.retries += 1;
        if self.state == State::SynReceived {
            if self.retries > SYN_ACK_MAX_RETRIES {
                self.finish(CloseReason::Timeout);
                return None;
            }
        } else if self.series_start.is_none() {
            self.series_start = Some(now.saturating_sub(self.rto_ms));
        }
        self.rto_ms = (self.rto_ms * 2).min(MAX_RTO_MS);
        self.retransmit_at = Some(now + self.rto_ms);
        self.timed = None;
        match self.state {
            State::SynSent => {
                let mut h = self.header(FLAG_SYN, self.snd_una);
                h.opts.mss = Some(MSS as u16);
                h.opts.sack_permitted = true;
                h.write(self.local_ip, self.remote_ip, &[], out)
            }
            State::SynReceived => {
                let mut h = self.header(FLAG_SYN | FLAG_ACK, self.snd_una);
                h.opts.mss = Some(MSS as u16);
                h.opts.sack_permitted = self.sack_ok;
                self.emit(h, &[], out)
            }
            _ => {
                if self.snd_una == self.snd_max {
                    self.retransmit_at = None;
                    self.retries = 0;
                    self.series_start = None;
                    return None;
                }
                // ssthresh only on the first timeout of a series
                // (RFC 5681 section 3.1, equation 4).
                if self.retries == 1 {
                    self.ssthresh = (self.inflight() / 2).max(2 * self.smss);
                }
                self.cwnd = self.smss;
                self.ca_acked = 0;
                self.dupacks = 0;
                self.in_recovery = false;
                self.rexmit_due = false;
                self.recover = self.snd_max;
                self.sacked.clear();
                self.snd_nxt = self.snd_una;
                // The front segment goes even into a closed window.
                let end = self.data_end();
                let n = (end.wrapping_sub(self.snd_una) as usize).min(self.payload_room());
                if n > 0 {
                    self.send_data(self.snd_una, n, now, out)
                } else {
                    // Only our FIN is outstanding.
                    self.snd_nxt = self.snd_una.wrapping_add(1);
                    let h = self.header(FLAG_FIN | FLAG_ACK, self.snd_una);
                    self.emit(h, &[], out)
                }
            }
        }
    }

    /// Writes `n` bytes of data from `seq` and moves `snd_nxt` past them
    /// when they start there.
    fn send_data(&mut self, seq: u32, n: usize, now: u64, out: &mut [u8]) -> Option<usize> {
        let offset = seq.wrapping_sub(self.snd_una) as usize;
        let mut payload = [0u8; MSS];
        let n = self.send.peek(offset, &mut payload[..n]);
        let h = self.header(FLAG_ACK | FLAG_PSH, seq);
        if seq == self.snd_nxt {
            self.snd_nxt = seq.wrapping_add(n as u32);
            if seq == self.snd_max {
                if self.timed.is_none() {
                    self.timed = Some((seq, now));
                }
                self.snd_max = self.snd_nxt;
            } else if seq_lt(self.snd_max, self.snd_nxt) {
                self.snd_max = self.snd_nxt;
            }
        }
        self.arm_timer(now);
        self.emit(h, &payload[..n], out)
    }

    /// Room for data in a segment that also carries our SACK blocks.
    fn payload_room(&self) -> usize {
        (self.smss - self.ack_options().len()).min(MSS)
    }

    /// New data, or the FIN once all of it is out.
    fn send_new(&mut self, now: u64, out: &mut [u8]) -> Option<usize> {
        let end = self.data_end();
        let unsent = end.wrapping_sub(self.snd_nxt) as usize;
        let unsent = if seq_lt(end, self.snd_nxt) { 0 } else { unsent };
        let window = (self.snd_wnd as usize).min(self.cwnd);
        let room = window.saturating_sub(self.inflight());
        let n = unsent.min(room).min(self.payload_room());
        if n > 0 {
            self.persist_at = None;
            return self.send_data(self.snd_nxt, n, now, out);
        }
        // A closed window with data waiting and nothing in flight: probe.
        if unsent > 0 && self.snd_wnd == 0 && self.inflight() == 0 && self.persist_at.is_none() {
            self.persist_at = Some(now + self.rto_ms);
        }
        if self.fin_queued
            && unsent == 0
            && self.snd_nxt == end
            && (!self.fin_sent || seq_lt(self.snd_nxt, self.snd_max))
        {
            self.fin_sent = true;
            self.snd_nxt = end.wrapping_add(1);
            if seq_lt(self.snd_max, self.snd_nxt) {
                self.snd_max = self.snd_nxt;
            }
            self.arm_timer(now);
            let h = self.header(FLAG_FIN | FLAG_ACK, end);
            return self.emit(h, &[], out);
        }
        None
    }

    /// The first hole at or above `rexmit_next` that the peer has not
    /// reported received: `snd_una` itself without SACK.
    fn next_hole(&self) -> Option<(u32, u32)> {
        let end = seq_min(self.data_end(), self.snd_max);
        let mut cur = seq_max(self.snd_una, self.rexmit_next);
        let limit = if self.sacked.n > 0 {
            self.sacked.highest()?
        } else if cur == self.snd_una {
            end
        } else {
            return None;
        };
        for &(a, b) in self.sacked.as_slice() {
            if seq_le(b, cur) {
                continue;
            }
            if seq_lt(cur, a) {
                return Some((cur, a));
            }
            cur = b;
        }
        seq_lt(cur, limit).then_some((cur, limit))
    }

    /// Resends the next hole of a fast recovery.
    fn retransmit_hole(&mut self, now: u64, out: &mut [u8]) -> Option<usize> {
        let (s, e) = self.next_hole()?;
        let n = (e.wrapping_sub(s) as usize).min(self.payload_room());
        if n == 0 {
            return None;
        }
        self.rexmit_next = s.wrapping_add(n as u32);
        self.timed = None;
        let offset = s.wrapping_sub(self.snd_una) as usize;
        let mut payload = [0u8; MSS];
        let n = self.send.peek(offset, &mut payload[..n]);
        if n == 0 {
            return None;
        }
        self.arm_timer(now);
        let h = self.header(FLAG_ACK | FLAG_PSH, s);
        self.emit(h, &payload[..n], out)
    }

    /// Processes a segment addressed to this connection.
    pub fn on_segment(&mut self, h: &Header, payload: &[u8], now: u64) {
        match self.state {
            State::Closed => {}
            State::Listen => {
                if h.flags & FLAG_RST != 0 {
                    return;
                }
                if h.flags & FLAG_SYN != 0 && h.flags & FLAG_ACK == 0 {
                    self.rcv_nxt = h.seq.wrapping_add(1);
                    self.rcv_adv = self.rcv_nxt;
                    self.peer_options(h);
                    self.snd_wnd = u32::from(h.window);
                    self.max_snd_wnd = self.snd_wnd;
                    self.snd_wl1 = h.seq;
                    // `snd_una`/`snd_nxt` hold the ISN given to `listen`.
                    self.state = State::SynReceived;
                    self.syn_pending = true;
                    self.retransmit_at = None;
                    self.arm_timer(now);
                }
            }
            State::SynSent => self.on_syn_sent(h, now),
            _ => self.on_synchronized(h, payload, now),
        }
    }

    /// MSS and SACK-permitted from the peer's SYN.
    fn peer_options(&mut self, h: &Header) {
        let peer = h.opts.mss.map_or(DEFAULT_PEER_MSS, usize::from);
        // A peer announcing less than the options we may add would leave
        // no room for data; 64 bytes is RFC 9293's smallest datagram.
        self.smss = peer.clamp(MAX_OPTIONS + 64, MSS);
        self.sack_ok = h.opts.sack_permitted;
        self.cwnd = INITIAL_WINDOW * self.smss;
    }

    fn on_syn_sent(&mut self, h: &Header, now: u64) {
        let has_ack = h.flags & FLAG_ACK != 0;
        // RFC 9293 section 3.10.7.3: an ACK of anything but our SYN is
        // answered with a reset (unless it is one) and changes nothing.
        if has_ack && h.ack != self.snd_nxt {
            if h.flags & FLAG_RST == 0 {
                self.reply_rst = Some(h.ack);
            }
            return;
        }
        if h.flags & FLAG_RST != 0 {
            // Only a reset that acknowledges our SYN is believed.
            if has_ack {
                self.finish(CloseReason::Reset);
            }
            return;
        }
        if h.flags & FLAG_SYN != 0 && has_ack {
            self.rcv_nxt = h.seq.wrapping_add(1);
            self.rcv_adv = self.rcv_nxt;
            self.snd_una = h.ack;
            self.recover = h.ack;
            self.rexmit_next = h.ack;
            self.peer_options(h);
            self.snd_wnd = u32::from(h.window);
            self.max_snd_wnd = self.snd_wnd;
            self.snd_wl1 = h.seq;
            self.snd_wl2 = h.ack;
            self.state = State::Established;
            self.established_rtt(now);
            self.retransmit_at = None;
            self.retries = 0;
            self.series_start = None;
            self.ack_now = true;
        }
    }

    /// Whether the segment overlaps the receive window (RFC 9293 section
    /// 3.10.7.4, first check). A FIN alone at the next sequence number is
    /// taken even into a closed window: it needs no buffer (audit of
    /// ADR 0070, L5).
    fn acceptable(&self, seq: u32, seg_len: usize, fin_only: bool) -> bool {
        let wnd = u32::from(self.window());
        let in_window =
            |x: u32| seq_le(self.rcv_nxt, x) && seq_lt(x, self.rcv_nxt.wrapping_add(wnd));
        match (seg_len, wnd) {
            (0, 0) => seq == self.rcv_nxt,
            (1, 0) if fin_only => seq == self.rcv_nxt,
            // At the right edge too, as Linux takes it: a peer that filled
            // our window sends its ACKs there, and refusing them while we
            // wait for a hole stalls both sides (audit of ADR 0070, H1).
            (0, _) => in_window(seq) || seq == self.rcv_nxt.wrapping_add(wnd),
            (_, 0) => false,
            (n, _) => in_window(seq) || in_window(seq.wrapping_add(n as u32 - 1)),
        }
    }

    fn on_synchronized(&mut self, h: &Header, payload: &[u8], now: u64) {
        let syn = h.flags & FLAG_SYN != 0;
        let fin = h.flags & FLAG_FIN != 0;
        let seg_len = payload.len() + usize::from(syn) + usize::from(fin);
        if !self.acceptable(h.seq, seg_len, fin && !syn && payload.is_empty()) {
            if h.flags & FLAG_RST != 0 {
                // RFC 5961 section 3.2: silently dropped.
                return;
            }
            if self.state == State::SynReceived && syn && h.seq.wrapping_add(1) == self.rcv_nxt {
                // The peer resent its SYN: our SYN|ACK was lost.
                if now >= self.challenge_after {
                    self.challenge_after = now + CHALLENGE_ACK_MS;
                    self.syn_pending = true;
                }
                return;
            }
            if self.state == State::TimeWait && fin {
                self.time_wait_until = now + TIME_WAIT_MS;
            }
            if !payload.is_empty() || fin {
                // A retransmission whose ACK was lost: acknowledge it at
                // once, never rate-limited (smoltcp 0.14, #1162).
                self.ack_now = true;
            } else {
                // A keep-alive, a window probe or a stray ACK.
                self.challenge(now);
            }
            return;
        }
        if h.flags & FLAG_RST != 0 {
            // RFC 5961 section 3.2: only the exact next sequence number
            // resets; anything else in the window gets a challenge ACK the
            // real peer answers with its own reset.
            if h.seq == self.rcv_nxt {
                self.finish(CloseReason::Reset);
            } else {
                self.challenge(now);
            }
            return;
        }
        if syn {
            // RFC 5961 section 4.2: a SYN on a synchronized connection is
            // never believed; the challenge ACK lets a restarted peer reset.
            self.challenge(now);
            return;
        }
        if h.flags & FLAG_ACK == 0 {
            return;
        }
        if self.state == State::SynReceived {
            if h.ack != self.snd_una.wrapping_add(1) {
                self.reply_rst = Some(h.ack);
                return;
            }
            // The ACK covers our SYN, which takes no room in the buffer.
            self.snd_una = h.ack;
            self.retransmit_at = None;
            self.retries = 0;
            self.series_start = None;
            self.state = if self.fin_queued {
                State::FinWait1
            } else {
                State::Established
            };
            self.established_rtt(now);
        }
        // RFC 5961 section 5.2: acknowledgements of data never sent, or
        // older than any window the peer could have had, are blind
        // injection attempts; the segment is dropped.
        if seq_lt(self.snd_max, h.ack) || seq_lt(h.ack, self.snd_una.wrapping_sub(self.max_snd_wnd))
        {
            self.challenge(now);
            return;
        }
        self.on_ack(h, payload, now);
        self.on_data(h, payload, now);
    }

    fn on_ack(&mut self, h: &Header, payload: &[u8], now: u64) {
        let window = u32::from(h.window);
        let old_wnd = self.snd_wnd;
        if self.persist_at.is_some() {
            // An answer to a window probe: the peer is alive.
            self.retries = 0;
            self.series_start = None;
        }
        // RFC 9293 section 3.10.7.4: only a segment newer than the last
        // window update changes the window.
        if seq_le(self.snd_una, h.ack)
            && (seq_lt(self.snd_wl1, h.seq)
                || (self.snd_wl1 == h.seq && seq_le(self.snd_wl2, h.ack)))
        {
            self.snd_wnd = window;
            self.max_snd_wnd = self.max_snd_wnd.max(window);
            self.snd_wl1 = h.seq;
            self.snd_wl2 = h.ack;
            if window > 0 {
                self.persist_at = None;
            }
        }
        if self.sack_ok {
            for &(s, e) in h.opts.sack_blocks() {
                if seq_lt(self.snd_una, e) && seq_le(e, self.snd_max) && seq_lt(s, e) {
                    // A block that does not fit is simply not used.
                    let _ = self.sacked.insert(seq_max(s, self.snd_una), e);
                }
            }
        }
        if seq_lt(self.snd_una, h.ack) {
            self.on_new_ack(h.ack, now);
        } else if h.ack == self.snd_una
            && payload.is_empty()
            && h.flags & FLAG_FIN == 0
            && window == old_wnd
            && seq_lt(self.snd_una, self.snd_max)
        {
            self.on_dupack();
        }
    }

    fn on_new_ack(&mut self, ack: u32, now: u64) {
        let mut acked = ack.wrapping_sub(self.snd_una) as usize;
        // Our SYN takes one sequence number before the data, our FIN one
        // after it.
        let fin_acked = self.fin_sent && ack == self.data_end().wrapping_add(1);
        acked -= usize::from(fin_acked);
        self.send.drop_front(acked);
        self.snd_una = ack;
        self.retries = 0;
        self.series_start = None;
        if seq_lt(self.snd_nxt, ack) {
            self.snd_nxt = ack;
        }
        self.sacked.trim_below(ack);
        if let Some((seq, t)) = self.timed {
            if seq_lt(seq, ack) {
                self.timed = None;
                self.rtt_sample(now - t);
            }
        }
        self.retransmit_at = (self.snd_una != self.snd_max).then_some(now + self.rto_ms);
        if self.in_recovery {
            if seq_le(self.recover, ack) {
                // Full acknowledgement (RFC 6582 section 3.2, step 3).
                self.in_recovery = false;
                self.rexmit_due = false;
                self.dupacks = 0;
                self.cwnd = self.ssthresh.min(self.inflight() + self.smss);
            } else {
                // Partial: the next hole is lost too (step 4).
                self.rexmit_due = true;
                self.cwnd = self.cwnd.saturating_sub(acked)
                    + if acked >= self.smss { self.smss } else { 0 };
            }
        } else {
            self.dupacks = 0;
            if self.cwnd < self.ssthresh {
                // Slow start, appropriate byte counting with L = 2 (RFC 3465).
                self.cwnd += acked.min(2 * self.smss);
            } else {
                self.ca_acked += acked;
                if self.ca_acked >= self.cwnd {
                    self.ca_acked -= self.cwnd;
                    self.cwnd += self.smss;
                }
            }
            self.cwnd = self.cwnd.min(4 * SEND_BUF);
        }
        match self.state {
            State::FinWait1 if fin_acked => {
                self.state = State::FinWait2;
                self.fin_wait2_until = now + FIN_WAIT2_MS;
            }
            State::Closing if fin_acked => self.enter_time_wait(now),
            State::LastAck if fin_acked => self.finish(CloseReason::Graceful),
            _ => {}
        }
    }

    fn on_dupack(&mut self) {
        self.dupacks = self.dupacks.saturating_add(1);
        if self.in_recovery {
            // Each duplicate is a segment that left the network: room for
            // one more, a hole the peer reported missing when there is one
            // (it is resent instead of new data), new data otherwise.
            if self.sack_ok && self.next_hole().is_some() {
                self.rexmit_due = true;
            } else {
                self.cwnd += self.smss;
            }
        } else if self.dupacks == DUPACK_THRESHOLD && seq_le(self.recover, self.snd_una) {
            // Fast retransmit (RFC 5681 section 3.2, RFC 6582).
            self.in_recovery = true;
            self.recover = self.snd_max;
            self.ssthresh = (self.inflight() / 2).max(2 * self.smss);
            self.cwnd = self.ssthresh + 3 * self.smss;
            self.rexmit_next = self.snd_una;
            self.rexmit_due = true;
            self.timed = None;
        }
    }

    fn on_data(&mut self, h: &Header, payload: &[u8], now: u64) {
        let fin = h.flags & FLAG_FIN != 0;
        if !payload.is_empty()
            && matches!(
                self.state,
                State::Established | State::FinWait1 | State::FinWait2
            )
            && !self.peer_fin
        {
            // Trim to the window.
            let mut seq = h.seq;
            let mut data = payload;
            if seq_lt(seq, self.rcv_nxt) {
                let skip = (self.rcv_nxt.wrapping_sub(seq) as usize).min(data.len());
                data = &data[skip..];
                seq = self.rcv_nxt;
            }
            let off = seq.wrapping_sub(self.rcv_nxt) as usize;
            let room = self.recv.free().saturating_sub(off);
            let data = &data[..data.len().min(room)];
            if data.is_empty() {
                self.ack_now = true;
            } else if off == 0 {
                self.recv.write_at(self.recv.len, data);
                self.recv.extend(data.len());
                self.rcv_nxt = self.rcv_nxt.wrapping_add(data.len() as u32);
                let had_holes = self.ooo.n > 0;
                while let Some(end) = self.ooo.take_front(self.rcv_nxt) {
                    if seq_lt(self.rcv_nxt, end) {
                        self.recv.extend(end.wrapping_sub(self.rcv_nxt) as usize);
                        self.rcv_nxt = end;
                    }
                }
                if had_holes {
                    // RFC 5681 section 4.2: a segment that fills a hole is
                    // acknowledged at once.
                    self.ack_now = true;
                } else {
                    self.unacked_segments = self.unacked_segments.saturating_add(1);
                    if self.unacked_segments >= 2 {
                        self.ack_now = true;
                    } else {
                        self.delayed_ack_at.get_or_insert(now + DELAYED_ACK_MS);
                    }
                }
            } else {
                // Out of order: kept when a range is free, and the
                // duplicate ACK (with SACK) goes out at once.
                let end = seq.wrapping_add(data.len() as u32);
                let mut probe = self.ooo;
                if probe.insert(seq, end) {
                    self.recv.write_at(self.recv.len + off, data);
                    self.ooo = probe;
                }
                self.ack_now = true;
            }
        }
        if fin && !self.peer_fin {
            let at = h.seq.wrapping_add(payload.len() as u32);
            if seq_lt(self.rcv_nxt, at) {
                self.fin_at = Some(at);
            }
        }
        let fin_now = (fin && h.seq.wrapping_add(payload.len() as u32) == self.rcv_nxt)
            || self.fin_at == Some(self.rcv_nxt);
        if fin_now && !self.peer_fin {
            self.fin_at = None;
            self.peer_fin = true;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            self.ack_now = true;
            match self.state {
                State::Established | State::SynReceived => self.state = State::CloseWait,
                State::FinWait1 => {
                    if self.fin_sent && self.snd_una == self.snd_max {
                        self.enter_time_wait(now);
                    } else {
                        self.state = State::Closing;
                    }
                }
                State::FinWait2 => self.enter_time_wait(now),
                _ => {}
            }
        }
    }

    fn enter_time_wait(&mut self, now: u64) {
        self.state = State::TimeWait;
        self.time_wait_until = now + TIME_WAIT_MS;
        self.retransmit_at = None;
        self.persist_at = None;
    }

    /// The next time `poll` has timed work to do (ADR 0044): a
    /// retransmission, giving up on one, a window probe, a delayed
    /// acknowledgement, the end of TIME-WAIT, or giving up FIN-WAIT-2. `None` when only a segment
    /// or a call from the user can change anything, so a service may sleep
    /// until then; work that is due at once (a first SYN, new data, an ACK)
    /// is what `poll` emits in the same round and needs no timer.
    pub fn next_timer(&self) -> Option<u64> {
        let earliest = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        };
        let common = earliest(
            earliest(self.retransmit_at, self.persist_at),
            earliest(
                self.delayed_ack_at,
                self.series_start.map(|t| t + GIVE_UP_MS),
            ),
        );
        match self.state {
            State::Closed => None,
            State::TimeWait => Some(self.time_wait_until),
            State::FinWait2 => earliest(Some(self.fin_wait2_until), self.delayed_ack_at),
            State::FinWait1 | State::Closing | State::LastAck => {
                earliest(common, self.closing_until)
            }
            _ => common,
        }
    }
}

/// Of `(slot, closed by its client, connection)`, the closed connection in
/// TIME-WAIT that ends soonest: the slot a full table may give to a new
/// connection (incident 0027). TIME-WAIT guards that connection's own
/// address and port pair, which the new one does not share. A connection
/// its client still holds is never chosen.
pub fn recyclable<'a, B: AsRef<[u8]> + AsMut<[u8]> + 'a>(
    socks: impl Iterator<Item = (usize, bool, &'a Connection<B>)>,
) -> Option<usize> {
    socks
        .filter(|(_, closed, c)| *closed && c.state() == State::TimeWait)
        .min_by_key(|(_, _, c)| c.next_timer().unwrap_or(0))
        .map(|(slot, _, _)| slot)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::isn;
    use std::vec;
    use std::vec::Vec;

    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
    const SECRET: isn::Secret = [0x5a; isn::SECRET_LEN];

    type Conn = Connection<Vec<u8>>;

    fn conn(local: Ipv4Addr, lp: u16, remote: Ipv4Addr, rp: u16) -> Conn {
        Connection::new(local, lp, remote, rp, vec![0; SEND_BUF], vec![0; RECV_BUF])
    }

    /// Two endpoints and a wire with controlled losses. Each side starts at
    /// the RFC 6528 ISN of its own 4-tuple, as the service does.
    struct Pair {
        a: Conn,
        b: Conn,
        now: u64,
        /// Returns `true` to drop the n-th segment.
        drop: fn(usize) -> bool,
        count: usize,
        delivered: usize,
    }

    impl Pair {
        fn new(drop: fn(usize) -> bool) -> Self {
            let mut a = conn(A, 40000, B, 80);
            let mut b = conn(B, 80, A, 40000);
            b.listen(isn::initial_seq(&SECRET, B, 80, A, 40000, 0));
            a.connect(isn::initial_seq(&SECRET, A, 40000, B, 80, 0), 0);
            Self {
                a,
                b,
                now: 0,
                drop,
                count: 0,
                delivered: 0,
            }
        }

        /// One exchange: drains A's output towards B and B's towards A.
        fn step(&mut self) -> bool {
            let mut moved = false;
            let mut buf = [0u8; MAX_SEGMENT];
            for dir in 0..2 {
                let (from, to, src, dst) = if dir == 0 {
                    (&mut self.a, &mut self.b, A, B)
                } else {
                    (&mut self.b, &mut self.a, B, A)
                };
                while let Some(n) = from.poll(self.now, &mut buf) {
                    moved = true;
                    self.count += 1;
                    if (self.drop)(self.count) {
                        continue;
                    }
                    let (h, payload) = Header::parse(src, dst, &buf[..n]).expect("checksum");
                    to.on_segment(&h, payload, self.now);
                    self.delivered += 1;
                }
            }
            moved
        }

        fn run(&mut self, ms: u64) {
            let end = self.now + ms;
            while self.now < end {
                while self.step() {}
                self.now += 10;
            }
        }

        /// Sends `data` from A to B, reading as it arrives; the time taken.
        fn transfer(&mut self, data: &[u8], limit_ms: u64) -> (Vec<u8>, u64) {
            let start = self.now;
            let mut sent = 0;
            let mut got = Vec::new();
            let mut out = vec![0u8; 8192];
            while self.now - start < limit_ms && got.len() < data.len() {
                if sent < data.len() {
                    sent += self.a.send(&data[sent..]);
                }
                while self.step() {}
                loop {
                    let n = self.b.recv(&mut out);
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&out[..n]);
                }
                while self.step() {}
                self.now += 1;
            }
            (got, self.now - start)
        }
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn handshake_data_and_close() {
        let mut p = Pair::new(|_| false);
        p.run(10);
        assert_eq!(p.a.state(), State::Established);
        assert_eq!(p.b.state(), State::Established);
        assert_eq!(p.a.send(b"hola tcp"), 8);
        p.run(10);
        let mut out = [0u8; 32];
        assert_eq!(p.b.recv(&mut out), 8);
        assert_eq!(&out[..8], b"hola tcp");
        assert_eq!(p.b.send(b"respuesta"), 9);
        p.run(10);
        assert_eq!(p.a.recv(&mut out), 9);
        assert_eq!(&out[..9], b"respuesta");
        // Close initiated by A.
        p.a.close();
        p.run(10);
        assert_eq!(p.a.state(), State::FinWait2);
        assert_eq!(p.b.state(), State::CloseWait);
        assert!(p.b.recv_finished());
        p.b.close();
        p.run(10);
        assert_eq!(p.b.state(), State::Closed);
        assert_eq!(p.b.close_reason(), Some(CloseReason::Graceful));
        assert_eq!(p.a.state(), State::TimeWait);
        p.run(TIME_WAIT_MS + 20);
        assert!(p.a.is_closed());
    }

    /// A pair whose side A closed first at `at` and sits in TIME-WAIT.
    fn time_wait_from(at: u64) -> Conn {
        let mut p = Pair::new(|_| false);
        p.run(at + 10);
        p.a.close();
        p.run(10);
        p.b.close();
        p.run(10);
        assert_eq!(p.a.state(), State::TimeWait);
        p.a
    }

    #[test]
    fn a_peer_that_never_closes_releases_fin_wait_2() {
        let mut p = Pair::new(|_| false);
        p.run(10);
        p.a.close();
        p.run(10);
        assert_eq!(
            p.a.state(),
            State::FinWait2,
            "our FIN acknowledged, theirs never comes"
        );
        assert_eq!(p.b.state(), State::CloseWait);
        assert!(
            p.a.next_timer().is_some_and(|t| t > p.now),
            "a timer to wake the service"
        );
        p.run(FIN_WAIT2_MS - 100);
        assert_eq!(p.a.state(), State::FinWait2, "not before its time");
        p.run(200);
        assert!(p.a.is_closed());
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
        assert_eq!(
            p.b.close_reason(),
            Some(CloseReason::Reset),
            "the peer got a reset"
        );
    }

    #[test]
    fn a_full_table_recycles_the_time_wait_that_ends_soonest() {
        let older = time_wait_from(0);
        let newer = time_wait_from(500);
        let open = Pair::new(|_| false).a;
        // Slots: 0 open, 1 newer, 2 older (closed), 3 older but still held.
        let socks = [
            (0, true, &open),
            (1, true, &newer),
            (2, true, &older),
            (3, false, &older),
        ];
        assert_eq!(recyclable(socks.into_iter()), Some(2));
        assert_eq!(recyclable([(1, true, &newer)].into_iter()), Some(1));
        assert_eq!(
            recyclable([(3, false, &older), (0, true, &open)].into_iter()),
            None
        );
    }

    #[test]
    fn retransmits_lost_syn_and_data() {
        // The first SYN and the fifth segment are lost (the data: SYN
        // resent = 2, SYN|ACK = 3, ACK = 4).
        let mut p = Pair::new(|n| n == 1 || n == 5);
        p.run(10);
        assert_eq!(p.a.state(), State::SynSent, "SYN lost, still no reply");
        p.run(INITIAL_RTO_MS + 20);
        assert_eq!(p.a.state(), State::Established, "SYN retransmitted");
        assert!(
            p.a.retries() == 0,
            "retransmissions are forgotten once acknowledged"
        );
        assert_eq!(p.a.rto_ms(), INITIAL_RTO_MS, "no sample from a resent SYN");
        p.a.send(b"datos");
        p.run(10);
        let mut out = [0u8; 16];
        assert_eq!(p.b.recv(&mut out), 0, "the data segment was lost");
        p.run(INITIAL_RTO_MS + 20);
        assert_eq!(p.b.recv(&mut out), 5);
        assert_eq!(&out[..5], b"datos");
    }

    /// Like `Pair::run`, but time jumps from one timer to the next
    /// (`next_timer`, ADR 0044) instead of advancing 10 ms at a time, as
    /// a service that sleeps until its next deadline drives it. Stops at
    /// `end` or when neither side has a timer left.
    fn run_on_timers(p: &mut Pair, end: u64) {
        loop {
            while p.step() {}
            let next = match (p.a.next_timer(), p.b.next_timer()) {
                (Some(x), Some(y)) => x.min(y),
                (x, y) => match x.or(y) {
                    Some(t) => t,
                    None => return,
                },
            };
            assert!(
                next > p.now,
                "a timer at {next} ms is already due at {}",
                p.now
            );
            if next > end {
                return;
            }
            p.now = next;
        }
    }

    /// Sleeping until `next_timer` loses nothing: the lost SYN and data
    /// are retransmitted, a delayed ACK goes out, TIME-WAIT ends, and an
    /// unanswered connection gives up exactly at the end of its schedule.
    #[test]
    fn timers_alone_drive_retransmission_time_wait_and_give_up() {
        let mut p = Pair::new(|n| n == 1 || n == 5);
        run_on_timers(&mut p, INITIAL_RTO_MS);
        assert_eq!(
            p.a.state(),
            State::Established,
            "SYN retransmitted at its timer"
        );
        p.a.send(b"datos");
        let end = p.now + 2 * INITIAL_RTO_MS;
        run_on_timers(&mut p, end);
        let mut out = [0u8; 16];
        assert_eq!(p.b.recv(&mut out), 5, "data retransmitted at its timer");
        assert_eq!(p.a.next_timer(), None, "everything acknowledged: no timer");
        assert_eq!(p.b.next_timer(), None, "the delayed ACK went out");
        p.a.close();
        while p.step() {}
        p.b.close();
        while p.step() {}
        assert_eq!(p.a.state(), State::TimeWait);
        assert_eq!(p.a.next_timer(), Some(p.now + TIME_WAIT_MS));
        run_on_timers(&mut p, u64::MAX);
        assert!(p.a.is_closed() && p.a.next_timer().is_none());

        let mut p = Pair::new(|_| true);
        run_on_timers(&mut p, u64::MAX);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
        assert_eq!(p.now, GIVE_UP_MS, "gave up at the end of its schedule");
    }

    #[test]
    fn gives_up_after_its_six_retransmissions() {
        let mut p = Pair::new(|_| true);
        p.run(60_000);
        assert_eq!(p.a.state(), State::Closed);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
        assert_eq!(p.a.retries(), 6, "the SYN and six retransmissions");
    }

    /// Incident 0013: a passive connection whose SYN|ACK is never answered
    /// frees itself after `SYN_ACK_MAX_RETRIES` retransmissions (0.5, 1, 2 s
    /// and a last 4 s wait: 7.5 s), not after the 31 s data schedule.
    #[test]
    fn unanswered_syn_ack_gives_up_within_the_short_budget() {
        let mut b = conn(B, 80, A, 40000);
        b.listen(isn::initial_seq(&SECRET, B, 80, A, 40000, 0));
        let syn = Header {
            src_port: 40000,
            dst_port: 80,
            seq: 1000,
            flags: FLAG_SYN,
            window: 4096,
            ..Header::default()
        };
        b.on_segment(&syn, &[], 0);
        assert_eq!(b.state(), State::SynReceived);
        let mut buf = [0u8; MAX_SEGMENT];
        let mut sent = 0;
        let mut now = 0;
        let closed_at = loop {
            while b.poll(now, &mut buf).is_some() {
                sent += 1;
            }
            if b.is_closed() {
                break now;
            }
            now += 10;
            assert!(now <= GIVE_UP_MS, "still holding its slot at {now} ms");
        };
        assert_eq!(b.close_reason(), Some(CloseReason::Timeout));
        assert_eq!(
            sent,
            1 + SYN_ACK_MAX_RETRIES as usize,
            "SYN|ACK and its retransmissions"
        );
        let budget: u64 = 500 + 1000 + 2000 + 4000;
        assert!(
            (budget..budget + 20).contains(&closed_at),
            "gave up at {closed_at} ms, expected about {budget}"
        );
    }

    /// The short budget is for the SYN|ACK only: an active open keeps the
    /// full schedule (its caller has a deadline of its own) and so does
    /// data on an established connection, even with a measured RTO far
    /// below the initial one.
    #[test]
    fn active_open_and_data_keep_the_full_schedule() {
        let mut p = Pair::new(|_| true);
        p.run(GIVE_UP_MS - 100);
        assert_eq!(p.a.state(), State::SynSent, "still trying before 31.5 s");
        p.run(200);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));

        let mut p = Pair::new(|_| false);
        p.run(10);
        assert!(p.a.is_established() && p.b.is_established());
        assert_eq!(p.a.rto_ms(), MIN_RTO_MS, "measured on a fast link");
        p.drop = |_| true;
        let start = p.now;
        assert_eq!(p.a.send(b"lost"), 4);
        p.run(GIVE_UP_MS - 100);
        assert!(p.a.is_established(), "still retransmitting before 31.5 s");
        p.run(200);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
        assert!(p.now - start <= GIVE_UP_MS + 120);
    }

    /// A SYN|ACK that is answered leaves `SynReceived` with the counters
    /// reset: the short budget never counts against data later.
    #[test]
    fn answered_syn_ack_resets_the_retry_count() {
        // Segments 2 and 3 are lost: the first SYN|ACK, then A's
        // retransmitted SYN; the second SYN|ACK answers A's third SYN.
        let mut p = Pair::new(|n| n == 2 || n == 3);
        p.run(2000);
        assert!(p.a.is_established() && p.b.is_established());
        assert_eq!(p.b.retries(), 0);
    }

    /// Incident 0013 audit: a SYN that also carries RST (nmap's
    /// `--scanflags SYNRST`) is a reset for a connection that does not
    /// exist. A listener stays in LISTEN, arms no timer and emits nothing,
    /// so a caller that created a connection for it would hold a slot
    /// forever: the service drops such segments before creating anything.
    #[test]
    fn listener_ignores_a_syn_carrying_rst() {
        let mut b = conn(B, 22, A, 40000);
        b.listen(isn::initial_seq(&SECRET, B, 22, A, 40000, 0));
        let syn_rst = Header {
            src_port: 40000,
            dst_port: 22,
            seq: 1000,
            flags: FLAG_SYN | FLAG_RST,
            window: 4096,
            ..Header::default()
        };
        b.on_segment(&syn_rst, &[], 0);
        assert_eq!(b.state(), State::Listen);
        assert!(!b.is_closed() && !b.is_established());
        let mut buf = [0u8; MAX_SEGMENT];
        for now in (0..GIVE_UP_MS).step_by(500) {
            assert_eq!(b.poll(now, &mut buf), None, "nothing to send at {now}");
        }
        assert_eq!(
            b.state(),
            State::Listen,
            "and it never leaves LISTEN on its own"
        );
    }

    #[test]
    fn listen_backlog_leaves_half_the_table_to_active_opens() {
        assert_eq!(listen_backlog(8), 4);
        assert_eq!(listen_backlog(2), 1);
        for slots in 2..64 {
            // Listener plus its backlog never exceed half the table plus one.
            assert!(1 + listen_backlog(slots) <= slots / 2 + 1);
        }
    }

    /// RFC 793 section 3.4: the reset to a SYN acknowledges the SYN (seq +
    /// 1), the one to a segment with ACK takes the ACK as its sequence
    /// number, and the ports are swapped.
    #[test]
    fn reset_for_follows_rfc_793_section_3_4() {
        let syn = Header {
            src_port: 40000,
            dst_port: 22,
            seq: 0xdead_beef,
            flags: FLAG_SYN,
            window: 65535,
            ..Header::default()
        };
        let r = reset_for(&syn, 0);
        assert_eq!((r.src_port, r.dst_port), (22, 40000));
        assert_eq!(r.flags, FLAG_RST | FLAG_ACK);
        assert_eq!(r.seq, 0);
        assert_eq!(r.ack, 0xdead_bef0);
        assert_eq!(r.window, 0);
        // Data and FIN count towards the acknowledged length; wrapping.
        let fin = Header {
            flags: FLAG_FIN,
            seq: u32::MAX - 2,
            ..syn
        };
        assert_eq!(reset_for(&fin, 5).ack, 3);
        // With an ACK the reset carries it as its sequence number.
        let ack = Header {
            flags: FLAG_ACK | FLAG_PSH,
            ack: 777,
            ..syn
        };
        let r = reset_for(&ack, 10);
        assert_eq!((r.seq, r.ack, r.flags), (777, 0, FLAG_RST));
        // The reset is a valid segment on the wire.
        let mut buf = [0u8; 64];
        let n = r.write(B, A, &[], &mut buf).unwrap();
        let (parsed, _) = Header::parse(B, A, &buf[..n]).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn random_loss_keeps_stream_in_order() {
        fn lossy(n: usize) -> bool {
            // Deterministic pseudo-random: ~25 % losses.
            (n.wrapping_mul(2654435761) >> 7) % 4 == 0
        }
        let mut p = Pair::new(lossy);
        p.run(2000);
        let data = pattern(200_000);
        let (got, _) = p.transfer(&data, 600_000);
        assert_eq!(got.len(), data.len(), "everything arrives");
        assert_eq!(got, data, "in order and without duplicates");
    }

    #[test]
    fn multiple_independent_connections() {
        let mut pairs: Vec<Pair> = (0..3).map(|_| Pair::new(|_| false)).collect();
        for (i, p) in pairs.iter_mut().enumerate() {
            p.run(10);
            let msg = [b'a' + i as u8; 4];
            assert_eq!(p.a.send(&msg), 4);
            p.run(10);
            let mut out = [0u8; 8];
            assert_eq!(p.b.recv(&mut out), 4);
            assert_eq!(out[..4], msg);
        }
    }

    /// Incident 0014: the SYN|ACK carries the ISN the caller chose for the
    /// 4-tuple, whatever sequence number the peer put in its SYN.
    #[test]
    fn passive_isn_is_the_callers_not_a_function_of_the_peers_seq() {
        let iss = isn::initial_seq(&SECRET, B, 80, A, 40000, 1234);
        let mut buf = [0u8; MAX_SEGMENT];
        let mut seen = Vec::new();
        for peer_seq in [0u32, 1, 1000, 0x8000_0000, u32::MAX] {
            let mut b = conn(B, 80, A, 40000);
            b.listen(iss);
            let syn = Header {
                src_port: 40000,
                dst_port: 80,
                seq: peer_seq,
                flags: FLAG_SYN,
                window: 4096,
                ..Header::default()
            };
            b.on_segment(&syn, &[], 1234);
            let n = b.poll(1234, &mut buf).expect("SYN|ACK");
            let (h, _) = Header::parse(B, A, &buf[..n]).unwrap();
            assert_eq!(h.flags, FLAG_SYN | FLAG_ACK);
            assert_eq!(h.ack, peer_seq.wrapping_add(1));
            assert_eq!(h.seq, iss, "our ISN does not move with the peer's");
            seen.push(h.seq);
        }
        assert!(seen.iter().all(|&s| s == iss));
    }

    #[test]
    fn reset_closes_immediately() {
        let mut p = Pair::new(|_| false);
        p.run(10);
        p.b.abort();
        p.run(10);
        assert_eq!(p.a.state(), State::Closed);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Reset));
    }

    #[test]
    fn header_and_options_round_trip_and_bad_checksum() {
        let mut h = Header {
            src_port: 40000,
            dst_port: 80,
            seq: 1,
            ack: 2,
            flags: FLAG_SYN,
            window: 4096,
            ..Header::default()
        };
        h.opts.mss = Some(1400);
        h.opts.sack_permitted = true;
        let mut buf = [0u8; 64];
        let n = h.write(A, B, b"", &mut buf).unwrap();
        assert_eq!(n, 28);
        let (parsed, payload) = Header::parse(A, B, &buf[..n]).unwrap();
        assert_eq!(parsed, h);
        assert!(payload.is_empty());
        assert!(
            Header::parse(A, Ipv4Addr::new(10, 0, 2, 3), &buf[..n]).is_none(),
            "another address"
        );
        let mut s = Header {
            flags: FLAG_ACK,
            ..Header::default()
        };
        s.opts.sack = [(10, 20), (30, 40), (u32::MAX - 5, 3), (0, 0)];
        s.opts.sack_len = 3;
        let n = s.write(A, B, b"data", &mut buf).unwrap();
        assert_eq!(n, HEADER_LEN + 4 + 24 + 4);
        let (parsed, payload) = Header::parse(A, B, &buf[..n]).unwrap();
        assert_eq!(parsed.opts.sack_blocks(), &s.opts.sack[..3]);
        assert_eq!(payload, b"data");
        assert!(seq_lt(u32::MAX - 1, 1) && !seq_lt(1, u32::MAX - 1));
    }

    /// Options parsing never reads past the area and keeps what came
    /// before a malformed option; unknown options are skipped.
    #[test]
    fn malformed_options_are_cut_short_not_trusted() {
        // Window scale (skipped), then MSS.
        let o = Options::parse(&[3, 3, 7, 2, 4, 0x05, 0xb4, 0]);
        assert_eq!(o.mss, Some(1460));
        // A zero length stops parsing (it would loop forever).
        let o = Options::parse(&[2, 4, 0x02, 0x18, 5, 0, 1, 1]);
        assert_eq!((o.mss, o.sack_len), (Some(536), 0));
        // A length past the end stops it too.
        assert_eq!(Options::parse(&[1, 1, 2, 40, 0, 0]), Options::default());
        // A SACK option whose length is not 2 + 8n is ignored; one with
        // more than four blocks gives four.
        assert_eq!(Options::parse(&[5, 7, 0, 0, 0, 0, 0]).sack_len, 0);
        let mut five = vec![5u8, 42];
        five.extend((0..40).map(|i| i as u8));
        assert_eq!(Options::parse(&five).sack_len, 4);
        // A one-byte area with a kind but no length.
        assert_eq!(Options::parse(&[2]), Options::default());
    }

    /// A connection on port 22 that sent 10 of 100 bytes into a 10-byte
    /// window and was closed by its owner.
    fn closed_with_data_pending(seg: &mut [u8]) -> (Conn, u32) {
        let me = Ipv4Addr::new(10, 0, 2, 15);
        let peer = Ipv4Addr::new(10, 0, 2, 2);
        let mut c = conn(me, 22, peer, 4000);
        c.listen(1000);
        let h = |seq, ack, flags, window| Header {
            src_port: 4000,
            dst_port: 22,
            seq,
            ack,
            flags,
            window,
            ..Header::default()
        };
        c.on_segment(&h(5000, 0, FLAG_SYN, 10), &[], 0);
        let n = c.poll(0, seg).unwrap();
        let una = Header::parse(me, peer, &seg[..n])
            .unwrap()
            .0
            .seq
            .wrapping_add(1);
        c.on_segment(&h(5001, una, FLAG_ACK, 10), &[], 1);
        c.send(&[0x41; 100]);
        c.close();
        while c.poll(2, seg).is_some() {}
        (c, una)
    }

    /// Audit 2026-10-25 B, L3 (reproduced by the audit): a FIN from the
    /// peer while our data and FIN were pending left the connection in
    /// CLOSING with nothing ever sent and no timer: the slot was held
    /// until the next boot.
    #[test]
    fn closing_sends_what_is_pending() {
        let mut seg = [0u8; MAX_SEGMENT];
        let (mut c, una) = closed_with_data_pending(&mut seg);
        let fin = Header {
            src_port: 4000,
            dst_port: 22,
            seq: 5001,
            ack: una.wrapping_add(10),
            flags: FLAG_ACK | FLAG_FIN,
            window: 65535,
            ..Header::default()
        };
        c.on_segment(&fin, &[], 3);
        assert_eq!(c.state(), State::Closing);
        let mut sent = 0;
        let mut fin_sent = false;
        while let Some(n) = c.poll(4, &mut seg) {
            let (h, p) = Header::parse(
                Ipv4Addr::new(10, 0, 2, 15),
                Ipv4Addr::new(10, 0, 2, 2),
                &seg[..n],
            )
            .unwrap();
            sent += p.len();
            fin_sent |= h.flags & FLAG_FIN != 0;
        }
        assert_eq!((sent, fin_sent), (90, true));
        assert!(
            c.next_timer().is_some(),
            "a timer while the FIN waits for its ACK"
        );
    }

    /// Audit 2026-10-25 B, L3: a peer that never opens its window keeps a
    /// closed connection in FIN-WAIT-1. It is probed meanwhile, and ends in
    /// RST after `CLOSING_MS`.
    #[test]
    fn a_closed_connection_gives_up_after_closing_ms() {
        let (me, peer) = (Ipv4Addr::new(10, 0, 2, 15), Ipv4Addr::new(10, 0, 2, 2));
        let mut seg = [0u8; MAX_SEGMENT];
        let (mut c, una) = closed_with_data_pending(&mut seg);
        // The peer acknowledges the 10 bytes and closes its window.
        let ack = Header {
            src_port: 4000,
            dst_port: 22,
            seq: 5001,
            ack: una.wrapping_add(10),
            flags: FLAG_ACK,
            window: 0,
            ..Header::default()
        };
        c.on_segment(&ack, &[], 3);
        assert_eq!(c.state(), State::FinWait1);
        // The service polls after every segment: that arms the probe.
        assert_eq!(c.poll(3, &mut seg), None, "no data into a zero window");
        let mut probes = 0;
        let mut now = 3;
        let rst = loop {
            let at = c.next_timer().expect("a deadline");
            assert!(at > now && at <= 2 + CLOSING_MS);
            now = at;
            let n = c.poll(now, &mut seg).expect("a probe or the RST");
            let (h, p) = Header::parse(me, peer, &seg[..n]).unwrap();
            assert!(p.is_empty(), "no data into a zero window");
            if h.flags & FLAG_RST != 0 {
                break h;
            }
            assert_eq!(h.seq, una.wrapping_add(9), "a window probe");
            probes += 1;
        };
        assert!(rst.flags & FLAG_RST != 0 && probes >= 3);
        assert!(c.is_closed());
        assert_eq!(c.close_reason(), Some(CloseReason::Timeout));
        assert_eq!(now, 2 + CLOSING_MS);
    }

    /// The FIN sent from CLOSING follows the data, acknowledges the peer's
    /// FIN, and its acknowledgement leads to TIME-WAIT.
    #[test]
    fn closing_fin_is_in_sequence_and_acknowledged() {
        let mut seg = [0u8; MAX_SEGMENT];
        let (me, peer) = (Ipv4Addr::new(10, 0, 2, 15), Ipv4Addr::new(10, 0, 2, 2));
        let (mut c, una) = closed_with_data_pending(&mut seg);
        let h = |ack, flags| Header {
            src_port: 4000,
            dst_port: 22,
            seq: 5001,
            ack,
            flags,
            window: 65535,
            ..Header::default()
        };
        c.on_segment(&h(una.wrapping_add(10), FLAG_ACK | FLAG_FIN), &[], 3);
        let mut fin = None;
        while let Some(n) = c.poll(4, &mut seg) {
            let (sh, _) = Header::parse(me, peer, &seg[..n]).unwrap();
            if sh.flags & FLAG_FIN != 0 {
                fin = Some(sh);
            }
        }
        let fin = fin.expect("a FIN");
        assert_eq!(fin.seq, una.wrapping_add(100));
        assert_eq!(fin.ack, 5002);
        let mut ack = h(una.wrapping_add(101), FLAG_ACK);
        ack.seq = 5002;
        c.on_segment(&ack, &[], 5);
        assert_eq!(c.state(), State::TimeWait);
    }

    /// LAST-ACK has the same deadline: a peer that never acknowledges our
    /// FIN (here, never opens its window for it) gets an RST.
    #[test]
    fn last_ack_gives_up_after_closing_ms() {
        let mut seg = [0u8; MAX_SEGMENT];
        let (me, peer) = (Ipv4Addr::new(10, 0, 2, 15), Ipv4Addr::new(10, 0, 2, 2));
        let mut c = conn(me, 22, peer, 4000);
        c.listen(1000);
        let h = |seq, ack, flags, window| Header {
            src_port: 4000,
            dst_port: 22,
            seq,
            ack,
            flags,
            window,
            ..Header::default()
        };
        c.on_segment(&h(5000, 0, FLAG_SYN, 0), &[], 0);
        let n = c.poll(0, &mut seg).unwrap();
        let una = Header::parse(me, peer, &seg[..n])
            .unwrap()
            .0
            .seq
            .wrapping_add(1);
        c.on_segment(&h(5001, una, FLAG_ACK | FLAG_FIN, 0), &[], 1);
        assert_eq!(c.state(), State::CloseWait);
        c.send(b"bye");
        c.close();
        assert_eq!(c.state(), State::LastAck);
        while c.poll(2, &mut seg).is_some() {}
        let at = c.next_timer().expect("a deadline");
        assert!(at <= 2 + CLOSING_MS);
        let n = c.poll(2 + CLOSING_MS, &mut seg).expect("an RST");
        assert!(Header::parse(me, peer, &seg[..n]).unwrap().0.flags & FLAG_RST != 0);
        assert!(c.is_closed());
    }

    // ---- ADR 0070: recovery, SACK and validation ----

    /// A scripted peer facing one connection (`me` on B:80, the peer on
    /// A:40000), to send exact segments and read the answers.
    struct Script {
        c: Conn,
        now: u64,
        /// Next sequence number the peer sends and expects.
        snd: u32,
        rcv: u32,
    }

    impl Script {
        /// An established passive connection; the peer offered `mss` and,
        /// when `sack`, SACK.
        fn new(mss: Option<u16>, sack: bool) -> Self {
            let mut c = conn(B, 80, A, 40000);
            c.listen(7000);
            let mut s = Script {
                c,
                now: 1000,
                snd: 1000,
                rcv: 0,
            };
            let mut syn = s.seg(FLAG_SYN, 0);
            syn.opts.mss = mss;
            syn.opts.sack_permitted = sack;
            s.c.on_segment(&syn, &[], s.now);
            let synack = s
                .out()
                .into_iter()
                .find(|(h, _)| h.flags & FLAG_SYN != 0)
                .unwrap()
                .0;
            assert_eq!(
                synack.opts.sack_permitted, sack,
                "SACK echoed only if offered"
            );
            s.snd += 1;
            s.rcv = synack.seq.wrapping_add(1);
            s.ack(s.rcv, 65535);
            assert!(s.c.is_established());
            s
        }
        fn seg(&self, flags: u8, ack: u32) -> Header {
            Header {
                src_port: 40000,
                dst_port: 80,
                seq: self.snd,
                ack,
                flags,
                window: 65535,
                ..Header::default()
            }
        }
        fn ack(&mut self, ack: u32, window: u16) {
            let mut h = self.seg(FLAG_ACK, ack);
            h.window = window;
            self.c.on_segment(&h, &[], self.now);
        }
        fn sack(&mut self, ack: u32, blocks: &[(u32, u32)]) {
            let mut h = self.seg(FLAG_ACK, ack);
            h.opts.sack[..blocks.len()].copy_from_slice(blocks);
            h.opts.sack_len = blocks.len() as u8;
            self.c.on_segment(&h, &[], self.now);
        }
        fn data(&mut self, seq: u32, data: &[u8]) {
            let mut h = self.seg(FLAG_ACK | FLAG_PSH, self.rcv);
            h.seq = seq;
            self.c.on_segment(&h, data, self.now);
        }
        /// Everything the connection sends now.
        fn out(&mut self) -> Vec<(Header, Vec<u8>)> {
            let mut buf = [0u8; MAX_SEGMENT];
            let mut v = Vec::new();
            while let Some(n) = self.c.poll(self.now, &mut buf) {
                let (h, p) = Header::parse(B, A, &buf[..n]).unwrap();
                v.push((h, p.to_vec()));
            }
            v
        }
    }

    /// Blind attacks (RFC 5961): a reset must carry exactly the next
    /// sequence number; inside the window it only draws a challenge ACK,
    /// outside it nothing.
    #[test]
    fn only_an_exact_reset_closes() {
        let mut s = Script::new(Some(1460), true);
        let mut rst = s.seg(FLAG_RST, 0);
        rst.seq = s.snd.wrapping_add(1_000_000);
        s.c.on_segment(&rst, &[], s.now);
        assert!(
            s.c.is_established() && s.out().is_empty(),
            "out of window: ignored"
        );
        rst.seq = s.snd.wrapping_add(100);
        s.c.on_segment(&rst, &[], s.now);
        assert!(s.c.is_established(), "in the window but not exact");
        let ch = s.out();
        assert!(
            ch.len() == 1 && ch[0].0.flags == FLAG_ACK && ch[0].0.ack == s.snd,
            "challenge ACK"
        );
        // A second forged reset within the second is not answered.
        rst.seq = s.snd.wrapping_add(200);
        s.c.on_segment(&rst, &[], s.now + 10);
        assert!(
            s.c.is_established() && s.out().is_empty(),
            "challenge ACKs are rate-limited"
        );
        rst.seq = s.snd;
        s.c.on_segment(&rst, &[], s.now);
        assert_eq!(s.c.close_reason(), Some(CloseReason::Reset));
    }

    #[test]
    fn a_syn_on_an_established_connection_is_challenged() {
        let mut s = Script::new(Some(1460), true);
        let syn = s.seg(FLAG_SYN, 0);
        s.c.on_segment(&syn, &[], s.now);
        assert!(s.c.is_established());
        let out = s.out();
        assert!(
            out.len() == 1 && out[0].0.flags == FLAG_ACK,
            "a challenge ACK, no reset"
        );
    }

    /// RFC 5961 section 5.2: data whose ACK acknowledges bytes never sent,
    /// or bytes older than any window, is not delivered.
    #[test]
    fn data_with_an_impossible_ack_is_dropped() {
        let mut s = Script::new(Some(1460), true);
        let mut h = s.seg(FLAG_ACK | FLAG_PSH, s.rcv.wrapping_add(1_000_000));
        s.c.on_segment(&h, b"evil", s.now);
        h.ack = s.rcv.wrapping_sub(1_000_000);
        s.c.on_segment(&h, b"evil", s.now + 2000);
        let mut out = [0u8; 16];
        assert_eq!(s.c.recv(&mut out), 0);
        // A real segment still goes through.
        s.data(s.snd, b"good");
        assert_eq!(s.c.recv(&mut out), 4);
    }

    /// RFC 9293 section 3.10.7.3: in SYN-SENT only a reset acknowledging
    /// our SYN is believed, and a SYN|ACK for another SYN is reset.
    #[test]
    fn syn_sent_believes_only_a_reset_for_its_syn() {
        let mut c = conn(A, 40000, B, 80);
        c.connect(5000, 0);
        let mut buf = [0u8; MAX_SEGMENT];
        c.poll(0, &mut buf).expect("SYN");
        let from_b = |flags, ack| Header {
            src_port: 80,
            dst_port: 40000,
            seq: 9000,
            ack,
            flags,
            window: 1000,
            ..Header::default()
        };
        c.on_segment(&from_b(FLAG_RST | FLAG_ACK, 5077), &[], 1);
        c.on_segment(&from_b(FLAG_RST, 0), &[], 1);
        assert_eq!(c.state(), State::SynSent);
        assert_eq!(c.poll(1, &mut buf), None, "no answer to a reset");
        c.on_segment(&from_b(FLAG_SYN | FLAG_ACK, 5077), &[], 2);
        assert_eq!(c.state(), State::SynSent);
        let n = c.poll(2, &mut buf).expect("a reset for the wrong SYN|ACK");
        let (h, _) = Header::parse(A, B, &buf[..n]).unwrap();
        assert_eq!((h.flags, h.seq), (FLAG_RST, 5077));
        c.on_segment(&from_b(FLAG_RST | FLAG_ACK, 5001), &[], 3);
        assert_eq!(c.close_reason(), Some(CloseReason::Reset));
    }

    #[test]
    fn the_peers_mss_bounds_every_segment() {
        let mut s = Script::new(Some(536), true);
        s.c.send(&[7u8; 5000]);
        let out = s.out();
        assert!(!out.is_empty());
        assert!(out.iter().all(|(_, p)| p.len() <= 536));
        // Without an MSS option RFC 9293's default applies.
        let mut s = Script::new(None, false);
        s.c.send(&[7u8; 5000]);
        assert!(s.out().iter().all(|(_, p)| p.len() <= DEFAULT_PEER_MSS));
    }

    /// Out-of-order data is kept and reported with SACK, the latest block
    /// first; the segment that fills the hole delivers everything.
    #[test]
    fn out_of_order_data_is_kept_and_sacked() {
        let mut s = Script::new(Some(1460), true);
        let base = s.snd;
        s.data(base.wrapping_add(1000), &[2u8; 500]);
        let ack = s.out();
        assert_eq!(ack.len(), 1, "an immediate duplicate ACK");
        assert_eq!(ack[0].0.ack, base);
        assert_eq!(ack[0].0.opts.sack_blocks(), &[(base + 1000, base + 1500)]);
        s.data(base.wrapping_add(2000), &[3u8; 500]);
        let ack = s.out();
        assert_eq!(
            ack[0].0.opts.sack_blocks(),
            &[(base + 2000, base + 2500), (base + 1000, base + 1500)],
            "the latest block first"
        );
        s.data(base.wrapping_add(1500), &[9u8; 500]);
        s.out();
        s.data(base, &[1u8; 1000]);
        let ack = s.out();
        assert_eq!(
            ack[0].0.ack,
            base.wrapping_add(2500),
            "all of it acknowledged at once"
        );
        assert_eq!(ack[0].0.opts.sack_len, 0);
        let mut got = vec![0u8; 4000];
        assert_eq!(s.c.recv(&mut got), 2500);
        assert!(got[..1000].iter().all(|&b| b == 1) && got[1000..1500].iter().all(|&b| b == 2));
        assert!(got[1500..2000].iter().all(|&b| b == 9) && got[2000..2500].iter().all(|&b| b == 3));
    }

    /// Without SACK from the peer, out-of-order data is still kept, and
    /// no SACK block is ever sent.
    #[test]
    fn no_sack_blocks_to_a_peer_that_did_not_offer_them() {
        let mut s = Script::new(Some(1460), false);
        let base = s.snd;
        s.data(base.wrapping_add(100), &[2u8; 100]);
        assert_eq!(s.out()[0].0.opts.sack_len, 0);
        s.data(base, &[1u8; 100]);
        let mut got = [0u8; 300];
        assert_eq!(s.c.recv(&mut got), 200);
    }

    /// A FIN that arrives before data still missing is remembered and
    /// takes effect when the hole fills.
    #[test]
    fn a_fin_ahead_of_a_hole_waits_for_it() {
        let mut s = Script::new(Some(1460), true);
        let base = s.snd;
        let mut h = s.seg(FLAG_ACK | FLAG_FIN, s.rcv);
        h.seq = base.wrapping_add(100);
        s.c.on_segment(&h, &[2u8; 100], s.now);
        assert_eq!(s.c.state(), State::Established);
        s.data(base, &[1u8; 100]);
        assert_eq!(s.c.state(), State::CloseWait);
        assert_eq!(
            s.out()[0].0.ack,
            base.wrapping_add(201),
            "data and FIN acknowledged"
        );
    }

    /// RFC 5681 section 4.2: every second segment is acknowledged at once,
    /// a lone one after `DELAYED_ACK_MS`.
    #[test]
    fn acknowledgements_are_delayed_for_one_segment_only() {
        let mut s = Script::new(Some(1460), true);
        let base = s.snd;
        s.data(base, &[1u8; 100]);
        assert!(s.out().is_empty(), "the first segment waits");
        assert_eq!(s.c.next_timer(), Some(s.now + DELAYED_ACK_MS));
        s.data(base.wrapping_add(100), &[1u8; 100]);
        assert_eq!(s.out().len(), 1, "the second is acknowledged at once");
        s.data(base.wrapping_add(200), &[1u8; 100]);
        assert!(s.out().is_empty());
        s.now += DELAYED_ACK_MS;
        let out = s.out();
        assert_eq!((out.len(), out[0].0.ack), (1, base.wrapping_add(300)));
    }

    /// A reader that takes everything gets the delayed ACK sent at once:
    /// a sender waiting for it before a short segment (Nagle) loses no
    /// time.
    #[test]
    fn reading_everything_sends_the_delayed_ack() {
        let mut s = Script::new(Some(1460), true);
        s.data(s.snd, &[1u8; 100]);
        assert!(s.out().is_empty(), "delayed");
        let mut got = [0u8; 50];
        assert_eq!(s.c.recv(&mut got), 50);
        assert!(s.out().is_empty(), "data still unread: still delayed");
        assert_eq!(s.c.recv(&mut got), 50);
        let out = s.out();
        assert_eq!((out.len(), out[0].0.ack), (1, s.snd.wrapping_add(100)));
    }

    /// A zero window with data waiting is probed, so a lost window update
    /// cannot stall the connection for good.
    #[test]
    fn a_closed_window_is_probed_until_it_opens() {
        let mut s = Script::new(Some(1460), true);
        s.ack(s.rcv, 0);
        s.c.send(b"waiting");
        assert!(s.out().is_empty(), "nothing into a zero window");
        let at = s.c.next_timer().expect("a probe timer");
        s.now = at;
        let probe = s.out();
        assert_eq!(probe.len(), 1);
        assert_eq!(
            probe[0].0.seq,
            s.rcv.wrapping_sub(1),
            "an old sequence number"
        );
        assert!(probe[0].1.is_empty());
        // The answer opens the window: the data goes out.
        s.ack(s.rcv, 1000);
        let out = s.out();
        assert_eq!(out[0].1, b"waiting");
    }

    /// RFC 5681 and 6582: three duplicate ACKs resend the lost segment at
    /// once, long before the timeout; a partial ACK resends the next.
    #[test]
    fn three_duplicates_trigger_a_fast_retransmit() {
        let mut s = Script::new(Some(1000), false);
        s.c.send(&pattern(10_000));
        let sent = s.out();
        assert_eq!(sent.len(), INITIAL_WINDOW, "the initial window");
        let first = s.rcv;
        // Segments 1 and 3 are lost: the peer acknowledges up to 1.
        for _ in 0..3 {
            s.ack(first, 65535);
        }
        let rexmit = s.out();
        assert_eq!(rexmit[0].0.seq, first, "the lost segment, before any timer");
        assert!(s.c.retries() == 0);
        // Partial ACK up to segment 3 (lost too): resent at once.
        s.ack(first.wrapping_add(2000), 65535);
        let rexmit = s.out();
        assert_eq!(rexmit[0].0.seq, first.wrapping_add(2000));
    }

    /// With SACK, one recovery resends every hole the peer reports, one
    /// per duplicate ACK, instead of one hole per round trip.
    #[test]
    fn sack_resends_several_holes_in_one_round() {
        let mut s = Script::new(Some(1000), true);
        s.c.send(&pattern(10_000));
        assert_eq!(s.out().len(), INITIAL_WINDOW);
        let b = s.rcv;
        let blk = |from: u32, to: u32| (b.wrapping_add(from * 1000), b.wrapping_add(to * 1000));
        // Segments 0, 2 and 5 lost.
        s.sack(b, &[blk(1, 2)]);
        s.sack(b, &[blk(3, 5), blk(1, 2)]);
        s.sack(b, &[blk(3, 5), blk(1, 2)]);
        let mut resent: Vec<u32> = s.out().iter().map(|(h, _)| h.seq).collect();
        assert_eq!(resent, [b], "the first hole on the third duplicate");
        s.sack(b, &[blk(6, 7), blk(3, 5), blk(1, 2)]);
        resent.extend(s.out().iter().map(|(h, _)| h.seq));
        s.sack(b, &[blk(6, 8), blk(3, 5), blk(1, 2)]);
        resent.extend(s.out().iter().map(|(h, _)| h.seq));
        assert_eq!(
            resent[..3],
            [b, b.wrapping_add(2000), b.wrapping_add(5000)],
            "each hole once, in order"
        );
    }

    /// After a timeout everything outstanding is resent as the window
    /// grows again (go-back-N), not one segment per timeout.
    #[test]
    fn a_timeout_resends_the_whole_flight() {
        let mut s = Script::new(Some(1000), false);
        s.c.send(&pattern(8000));
        assert_eq!(s.out().len(), 8);
        s.now = s.c.next_timer().unwrap();
        let first = s.out();
        assert_eq!(
            (first.len(), first[0].0.seq),
            (1, s.rcv),
            "one segment after the timeout"
        );
        s.ack(s.rcv.wrapping_add(1000), 65535);
        let next = s.out();
        assert_eq!(next.len(), 2, "slow start: one acknowledged opens two");
        assert_eq!(next[0].0.seq, s.rcv.wrapping_add(1000));
    }

    /// RFC 6298: the timeout follows the measured round trip, floored at
    /// `MIN_RTO_MS`, and backs off on a timeout.
    #[test]
    fn the_timeout_follows_the_round_trip() {
        let mut s = Script::new(Some(1000), false);
        // The handshake's sample (0 ms) fades after a few dozen.
        for _ in 0..40 {
            s.c.send(&[1u8; 1000]);
            let seg = s.out();
            s.now += 300;
            s.ack(seg[0].0.seq.wrapping_add(1000), 65535);
            s.rcv = s.rcv.wrapping_add(1000);
        }
        let rto = s.c.rto_ms();
        assert!(
            (300..=400).contains(&rto),
            "about the 300 ms round trip: {rto}"
        );
        s.c.send(&[1u8; 1000]);
        s.out();
        s.now = s.c.next_timer().unwrap();
        s.out();
        assert_eq!(s.c.rto_ms(), 2 * rto);
    }

    /// A retransmitted FIN in TIME-WAIT (our last ACK was lost) is
    /// acknowledged again.
    #[test]
    fn time_wait_acknowledges_a_retransmitted_fin() {
        let mut p = Pair::new(|_| false);
        p.run(10);
        p.a.close();
        p.run(10);
        p.b.close();
        let mut buf = [0u8; MAX_SEGMENT];
        let n = p.b.poll(p.now, &mut buf).expect("B's FIN");
        let (fin, _) = Header::parse(B, A, &buf[..n]).unwrap();
        p.a.on_segment(&fin, &[], p.now);
        assert_eq!(p.a.state(), State::TimeWait);
        assert!(p.a.poll(p.now, &mut buf).is_some(), "the ACK");
        p.a.on_segment(&fin, &[], p.now + 100);
        let n = p.a.poll(p.now + 100, &mut buf).expect("the ACK again");
        let (ack, _) = Header::parse(A, B, &buf[..n]).unwrap();
        assert_eq!(ack.ack, fin.seq.wrapping_add(1));
    }

    /// Loss recovery on a long transfer: 1 % and 5 % random loss.
    #[test]
    fn bulk_transfer_survives_loss_quickly() {
        fn one_in_100(n: usize) -> bool {
            (n.wrapping_mul(2654435761) >> 9).is_multiple_of(100)
        }
        fn one_in_20(n: usize) -> bool {
            (n.wrapping_mul(2654435761) >> 9).is_multiple_of(20)
        }
        for (loss, limit) in [
            (one_in_100 as fn(usize) -> bool, 20_000),
            (one_in_20, 60_000),
        ] {
            let mut p = Pair::new(|_| false);
            p.run(10);
            p.drop = loss;
            let data = pattern(1 << 20);
            let (got, ms) = p.transfer(&data, limit);
            assert_eq!(got, data, "everything, in order");
            assert!(ms < limit, "took {ms} ms");
        }
    }

    /// Both directions at once over a wire that delays (5 to 34 ms),
    /// reorders, duplicates and loses up to 14 %, with irregular reads
    /// (audit of ADR 0070, H1): after a timeout each side's ACKs must
    /// still be acceptable to the other, or both resend into segments the
    /// other drops until one gives up. 41 of 200 seeds failed before.
    #[test]
    fn both_directions_survive_a_bad_wire() {
        let seeds: u64 = std::env::var("TCP_WIRE_SEEDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        for seed in 1..=seeds {
            let mut x = seed.wrapping_mul(0x9e37_79b9) | 1;
            let mut rng = move || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let mut a = conn(A, 40000, B, 80);
            let mut b = conn(B, 80, A, 40000);
            b.listen(0xffff_f000u32.wrapping_add(seed as u32));
            a.connect(0x7fff_fff0u32.wrapping_add((seed >> 7) as u32), 0);
            let ab = pattern(300_000);
            let ba: Vec<u8> = (0..100_000).map(|i| (i % 241) as u8).collect();
            let (mut sa, mut sb) = (0, 0);
            let (mut got_b, mut got_a) = (Vec::new(), Vec::new());
            let mut wire: Vec<(u64, bool, Header, Vec<u8>)> = Vec::new();
            let loss = rng() % 15;
            let mut buf = vec![0u8; 5000];
            let mut seg = [0u8; MAX_SEGMENT];
            let mut now = 0u64;
            while got_b.len() < ab.len() || got_a.len() < ba.len() {
                assert!(now < 600_000, "seed {seed}: no completion");
                assert!(
                    !a.is_closed() && !b.is_closed(),
                    "seed {seed}, loss {loss} %: {:?} {:?} at {now} ms",
                    a.close_reason(),
                    b.close_reason()
                );
                sa += a.send(&ab[sa..]);
                sb += b.send(&ba[sb..]);
                if rng() % 4 != 0 {
                    let max = (rng() % 5000) as usize;
                    let n = b.recv(&mut buf[..max]);
                    got_b.extend_from_slice(&buf[..n]);
                }
                let n = a.recv(&mut buf);
                got_a.extend_from_slice(&buf[..n]);
                for (to_b, c, src, dst) in [(true, &mut a, A, B), (false, &mut b, B, A)] {
                    while let Some(n) = c.poll(now, &mut seg) {
                        let (h, p) = Header::parse(src, dst, &seg[..n]).unwrap();
                        let r = rng();
                        if r % 100 < loss {
                            continue;
                        }
                        let at = now + 5 + (r >> 8) % 30;
                        wire.push((at, to_b, h, p.to_vec()));
                        if (r >> 20) % 50 == 0 {
                            wire.push((at + 7, to_b, h, p.to_vec()));
                        }
                    }
                }
                wire.sort_by_key(|w| w.0);
                let due = wire.iter().take_while(|w| w.0 <= now).count();
                for (_, to_b, h, p) in wire.drain(..due) {
                    let c = if to_b { &mut b } else { &mut a };
                    c.on_segment(&h, &p, now);
                }
                assert_eq!(&ab[..got_b.len()], &got_b[..], "seed {seed}: A to B");
                assert_eq!(&ba[..got_a.len()], &got_a[..], "seed {seed}: B to A");
                now = wire
                    .first()
                    .map_or(now + 3, |w| w.0.clamp(now + 1, now + 3));
            }
        }
    }

    /// A close before our SYN is acknowledged keeps resending the
    /// SYN|ACK with its short budget, never a FIN at the ISN (audit of
    /// ADR 0070, L1); the FIN follows once the SYN is acknowledged.
    #[test]
    fn a_close_in_syn_received_waits_for_the_handshake() {
        let mut b = conn(B, 80, A, 40000);
        b.listen(7000);
        let syn = Header {
            src_port: 40000,
            dst_port: 80,
            seq: 1000,
            flags: FLAG_SYN,
            window: 4096,
            ..Header::default()
        };
        b.on_segment(&syn, &[], 0);
        let mut buf = [0u8; MAX_SEGMENT];
        b.poll(0, &mut buf).expect("SYN|ACK");
        b.close();
        assert_eq!(b.state(), State::SynReceived);
        let at = b.next_timer().unwrap();
        let n = b.poll(at, &mut buf).expect("a retransmission");
        let (h, _) = Header::parse(B, A, &buf[..n]).unwrap();
        assert_eq!(h.flags, FLAG_SYN | FLAG_ACK);
        let ack = Header {
            seq: 1001,
            ack: 7001,
            flags: FLAG_ACK,
            ..syn
        };
        b.on_segment(&ack, &[], at + 1);
        assert_eq!(b.state(), State::FinWait1);
        let n = b.poll(at + 1, &mut buf).expect("the FIN");
        let (h, _) = Header::parse(B, A, &buf[..n]).unwrap();
        assert_eq!((h.flags, h.seq), (FLAG_FIN | FLAG_ACK, 7001));
    }

    /// A FIN alone at the next sequence number is taken into a full
    /// buffer (audit of ADR 0070, L5).
    #[test]
    fn a_fin_enters_a_closed_window() {
        let mut c = Connection::new(B, 80, A, 40000, vec![0u8; 64], vec![0u8; 100]);
        c.listen(7000);
        let mut h = Header {
            src_port: 40000,
            dst_port: 80,
            seq: 1000,
            flags: FLAG_SYN,
            window: 4096,
            ..Header::default()
        };
        c.on_segment(&h, &[], 0);
        let mut buf = [0u8; MAX_SEGMENT];
        while c.poll(0, &mut buf).is_some() {}
        h.seq = 1001;
        h.ack = 7001;
        h.flags = FLAG_ACK;
        c.on_segment(&h, &[5u8; 100], 1);
        h.seq = 1101;
        h.flags = FLAG_ACK | FLAG_FIN;
        c.on_segment(&h, &[], 2);
        assert_eq!(c.state(), State::CloseWait, "the FIN needs no room");
    }

    /// Random segments in every state never panic (fuzz with
    /// `TCP_FUZZ_ITERATIONS`).
    #[test]
    fn random_segments_never_panic() {
        let iterations: usize = std::env::var("TCP_FUZZ_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20_000);
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut s = Script::new(Some(1460), true);
        let mut buf = [0u8; MAX_SEGMENT];
        let mut payload = [0u8; 2000];
        for i in 0..iterations {
            if i % 500 == 0 {
                s = Script::new(Some((next() % 1500) as u16), next() & 1 == 0);
                s.c.send(&pattern((next() % 40_000) as usize));
            }
            let r = next();
            let near = |base: u32, r: u64| match r % 4 {
                0 => base,
                1 => base.wrapping_add((r >> 8) as u32 % 70_000),
                2 => base.wrapping_sub((r >> 8) as u32 % 70_000),
                _ => (r >> 16) as u32,
            };
            let mut h = Header {
                src_port: 40000,
                dst_port: 80,
                seq: near(s.snd, r),
                ack: near(s.rcv, r >> 3),
                flags: (r >> 24) as u8 & 0x3f,
                window: (r >> 32) as u16,
                ..Header::default()
            };
            let nb = (r >> 48) as usize % 5;
            for k in 0..nb {
                let a = near(s.rcv, next());
                h.opts.sack[k.min(MAX_SACK - 1)] = (a, a.wrapping_add((next() % 5000) as u32));
            }
            h.opts.sack_len = nb.min(MAX_SACK) as u8;
            let len = (next() % 2000) as usize;
            for b in payload[..len].iter_mut() {
                *b = next() as u8;
            }
            s.c.on_segment(&h, &payload[..len], s.now);
            if r & 0x100 != 0 {
                s.now += next() % 3000;
            }
            if r & 0x200 != 0 {
                s.c.send(&payload[..len]);
            }
            if r & 0x400 != 0 {
                let _ = s.c.recv(&mut payload[..len]);
            }
            if r % 997 == 0 {
                s.c.close();
            }
            while s.c.poll(s.now, &mut buf).is_some() {}
            let _ = s.c.next_timer();
        }
    }
}
