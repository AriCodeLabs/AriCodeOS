// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Minimal TCP (RFC 793) without `alloc`: header, and a connection with fixed
//! buffers, retransmission with an exponential timer and orderly close. The
//! connection sends nothing on its own: `poll` produces at most one
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

/// Maximum segment size we announce and use.
pub const MSS: usize = 1400;
pub const SEND_BUF: usize = 4096;
pub const RECV_BUF: usize = 4096;
/// Largest segment `poll` produces (header + MSS option + data).
pub const MAX_SEGMENT: usize = HEADER_LEN + 4 + MSS;
const INITIAL_RTO_MS: u64 = 500;
const MAX_RTO_MS: u64 = 8000;
/// Retransmissions of a SYN, data or FIN before the connection gives up
/// (`CloseReason::Timeout`): with the RTO doubling from 500 ms to 8 s the
/// schedule is 0.5, 1, 2, 4, 8, 8, 8 s, about 31 s in all.
pub const MAX_RETRIES: u8 = 6;
/// Retransmissions of the SYN|ACK of a passive connection before it gives
/// up (incident 0013, ADR 0035): 0.5, 1, 2 s and a last wait of 4 s, about
/// 7.5 s in all. A SYN whose sender never answers the SYN|ACK (unreachable,
/// spoofed, or a scanner) must not hold one of the service's few slots for
/// the full `MAX_RETRIES` schedule (31 s); three retransmissions still
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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
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
            },
            &b[off..],
        ))
    }

    /// Writes the complete segment (with MSS option if `mss`, and checksum).
    pub fn write(
        &self,
        src: Ipv4Addr,
        dst: Ipv4Addr,
        mss: Option<u16>,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let opt = if mss.is_some() { 4 } else { 0 };
        let off = HEADER_LEN + opt;
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
        if let Some(m) = mss {
            out[20] = 2;
            out[21] = 4;
            out[22..24].copy_from_slice(&m.to_be_bytes());
        }
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

/// Fixed-size byte ring.
struct Ring<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> Ring<N> {
    const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            len: 0,
        }
    }
    fn free(&self) -> usize {
        N - self.len
    }
    fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free());
        for (i, b) in data[..n].iter().enumerate() {
            self.buf[(self.head + self.len + i) % N] = *b;
        }
        self.len += n;
        n
    }
    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for (i, slot) in out[..n].iter_mut().enumerate() {
            *slot = self.buf[(self.head + i) % N];
        }
        self.drop_front(n);
        n
    }
    fn drop_front(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % N;
        self.len -= n;
    }
    /// Copies `[offset, offset + out.len())` without consuming.
    fn peek(&self, offset: usize, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len.saturating_sub(offset));
        for (i, slot) in out[..n].iter_mut().enumerate() {
            *slot = self.buf[(self.head + offset + i) % N];
        }
        n
    }
}

pub struct Connection {
    local_ip: Ipv4Addr,
    remote_ip: Ipv4Addr,
    local_port: u16,
    remote_port: u16,
    state: State,
    reason: Option<CloseReason>,
    /// First unacknowledged byte and next byte to send.
    snd_una: u32,
    snd_nxt: u32,
    /// Window announced by the peer.
    snd_wnd: u16,
    rcv_nxt: u32,
    /// Data accepted and not yet acknowledged/sent (from `snd_una`).
    send: Ring<SEND_BUF>,
    recv: Ring<RECV_BUF>,
    /// SYN must be sent (active, or passive SYN|ACK).
    syn_pending: bool,
    /// The user asked to close: FIN after the data.
    fin_queued: bool,
    /// FIN already sent (its sequence number is `snd_nxt - 1`).
    fin_sent: bool,
    /// The peer closed its side.
    peer_fin: bool,
    need_ack: bool,
    rto_ms: u64,
    retransmit_at: Option<u64>,
    retries: u8,
    time_wait_until: u64,
    /// When FIN-WAIT-2 gives up on the peer's FIN (`FIN_WAIT2_MS`).
    fin_wait2_until: u64,
    /// When a connection closed on our side gives up delivering its data
    /// and FIN (`CLOSING_MS`); set at the first `poll` after `close`.
    closing_until: Option<u64>,
}

impl Connection {
    pub fn new(local_ip: Ipv4Addr, local_port: u16, remote_ip: Ipv4Addr, remote_port: u16) -> Self {
        Self {
            local_ip,
            remote_ip,
            local_port,
            remote_port,
            state: State::Closed,
            reason: None,
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: 0,
            rcv_nxt: 0,
            send: Ring::new(),
            recv: Ring::new(),
            syn_pending: false,
            fin_queued: false,
            fin_sent: false,
            peer_fin: false,
            need_ack: false,
            rto_ms: INITIAL_RTO_MS,
            retransmit_at: None,
            retries: 0,
            time_wait_until: 0,
            fin_wait2_until: 0,
            closing_until: None,
        }
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

    /// Active open: sends SYN on the next `poll`.
    pub fn connect(&mut self, iss: u32, now: u64) {
        self.snd_una = iss;
        self.snd_nxt = iss;
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
        self.snd_una = iss;
        self.snd_nxt = iss;
        self.state = State::Listen;
    }

    /// Accepts user data; returns how much fits.
    pub fn send(&mut self, data: &[u8]) -> usize {
        if !self.is_established() || self.fin_queued {
            return 0;
        }
        self.send.push(data)
    }

    /// Delivers received data in order.
    pub fn recv(&mut self, out: &mut [u8]) -> usize {
        let n = self.recv.pop(out);
        if n > 0 {
            self.need_ack = true; // announce the freed window
        }
        n
    }

    /// Orderly close: FIN after draining the send buffer.
    pub fn close(&mut self) {
        match self.state {
            State::Closed | State::Listen => self.finish(CloseReason::Graceful),
            State::SynSent => self.finish(CloseReason::Graceful),
            State::Established | State::SynReceived => {
                self.fin_queued = true;
                self.state = State::FinWait1;
            }
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
            self.need_ack = true; // reused as "send RST"
        }
    }

    fn finish(&mut self, reason: CloseReason) {
        self.state = State::Closed;
        self.reason = Some(reason);
        self.retransmit_at = None;
        self.need_ack = false;
    }

    fn arm_timer(&mut self, now: u64) {
        if self.retransmit_at.is_none() {
            self.retransmit_at = Some(now + self.rto_ms);
        }
    }

    fn window(&self) -> u16 {
        self.recv.free().min(u16::MAX as usize) as u16
    }

    fn header(&self, flags: u8, seq: u32) -> Header {
        Header {
            src_port: self.local_port,
            dst_port: self.remote_port,
            seq,
            ack: self.rcv_nxt,
            flags,
            window: self.window(),
        }
    }

    fn emit(&self, h: Header, mss: bool, payload: &[u8], out: &mut [u8]) -> Option<usize> {
        h.write(
            self.local_ip,
            self.remote_ip,
            mss.then_some(MSS as u16),
            payload,
            out,
        )
    }

    /// Bytes sent and not yet acknowledged.
    fn inflight(&self) -> usize {
        self.snd_nxt.wrapping_sub(self.snd_una) as usize
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
                self.state = State::Closed;
                self.reason = Some(CloseReason::Timeout);
                self.need_ack = true; // reused as "send RST"
            }
        }
        if self.state == State::FinWait2 && now >= self.fin_wait2_until {
            self.state = State::Closed;
            self.reason = Some(CloseReason::Timeout);
            self.need_ack = true; // reused as "send RST"
        }
        // RST after abort or a FIN-WAIT-2 that ran out.
        if self.state == State::Closed {
            if self.need_ack
                && matches!(self.reason, Some(CloseReason::Reset | CloseReason::Timeout))
            {
                self.need_ack = false;
                return self.emit(
                    self.header(FLAG_RST | FLAG_ACK, self.snd_nxt),
                    false,
                    &[],
                    out,
                );
            }
            return None;
        }
        if self.state == State::TimeWait && now >= self.time_wait_until {
            self.finish(CloseReason::Graceful);
            return None;
        }
        // Timer-driven retransmission. A SYN|ACK nobody answers has the
        // short budget: the slot must not wait 31 s for a peer that never
        // was (incident 0013).
        if let Some(at) = self.retransmit_at {
            if now >= at {
                self.retries += 1;
                let budget = if self.state == State::SynReceived {
                    SYN_ACK_MAX_RETRIES
                } else {
                    MAX_RETRIES
                };
                if self.retries > budget {
                    self.finish(CloseReason::Timeout);
                    return None;
                }
                self.rto_ms = (self.rto_ms * 2).min(MAX_RTO_MS);
                self.retransmit_at = Some(now + self.rto_ms);
                return self.retransmit(out);
            }
        }
        // Initial SYN.
        if self.syn_pending {
            self.syn_pending = false;
            let flags = if self.state == State::SynReceived {
                FLAG_SYN | FLAG_ACK
            } else {
                FLAG_SYN
            };
            let seg = self.emit(self.header(flags, self.snd_una), true, &[], out);
            self.snd_nxt = self.snd_una.wrapping_add(1);
            self.arm_timer(now);
            self.need_ack = false;
            return seg;
        }
        // New data.
        // CLOSING included: a FIN from the peer can arrive while our data
        // and FIN are still pending (audit 2026-10-25 B, L3).
        if matches!(
            self.state,
            State::Established
                | State::CloseWait
                | State::FinWait1
                | State::Closing
                | State::LastAck
        ) {
            let sent = self.inflight() - usize::from(self.fin_sent);
            let unsent = self.send.len.saturating_sub(sent);
            let room = (self.snd_wnd as usize).saturating_sub(self.inflight());
            let n = unsent.min(room).min(MSS);
            if n > 0 && !self.fin_sent {
                let mut payload = [0u8; MSS];
                self.send.peek(sent, &mut payload[..n]);
                let seg = self.emit(
                    self.header(FLAG_ACK | FLAG_PSH, self.snd_nxt),
                    false,
                    &payload[..n],
                    out,
                );
                self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
                self.arm_timer(now);
                self.need_ack = false;
                return seg;
            }
            // FIN once everything has been sent.
            if self.fin_queued && !self.fin_sent && unsent == 0 {
                self.fin_sent = true;
                let seg = self.emit(
                    self.header(FLAG_FIN | FLAG_ACK, self.snd_nxt),
                    false,
                    &[],
                    out,
                );
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                self.arm_timer(now);
                self.need_ack = false;
                return seg;
            }
        }
        if self.need_ack && self.state != State::Listen && self.state != State::SynSent {
            self.need_ack = false;
            return self.emit(self.header(FLAG_ACK, self.snd_nxt), false, &[], out);
        }
        None
    }

    /// Resends from `snd_una`: SYN, first data chunk or FIN.
    fn retransmit(&mut self, out: &mut [u8]) -> Option<usize> {
        match self.state {
            State::SynSent => self.emit(self.header(FLAG_SYN, self.snd_una), true, &[], out),
            State::SynReceived => self.emit(
                self.header(FLAG_SYN | FLAG_ACK, self.snd_una),
                true,
                &[],
                out,
            ),
            _ => {
                let data_inflight = self.inflight() - usize::from(self.fin_sent);
                if data_inflight > 0 {
                    let n = data_inflight.min(MSS);
                    let mut payload = [0u8; MSS];
                    self.send.peek(0, &mut payload[..n]);
                    self.emit(
                        self.header(FLAG_ACK | FLAG_PSH, self.snd_una),
                        false,
                        &payload[..n],
                        out,
                    )
                } else if self.fin_sent {
                    self.emit(
                        self.header(FLAG_FIN | FLAG_ACK, self.snd_una),
                        false,
                        &[],
                        out,
                    )
                } else {
                    self.retransmit_at = None;
                    self.retries = 0;
                    None
                }
            }
        }
    }

    /// Processes a segment addressed to this connection.
    pub fn on_segment(&mut self, h: &Header, payload: &[u8], now: u64) {
        if h.flags & FLAG_RST != 0 {
            if self.state != State::Listen {
                self.finish(CloseReason::Reset);
            }
            return;
        }
        match self.state {
            State::Closed => {}
            State::Listen => {
                if h.flags & FLAG_SYN != 0 && h.flags & FLAG_ACK == 0 {
                    self.rcv_nxt = h.seq.wrapping_add(1);
                    self.snd_wnd = h.window;
                    // `snd_una`/`snd_nxt` hold the ISN given to `listen`.
                    self.state = State::SynReceived;
                    self.syn_pending = true;
                    self.retransmit_at = None;
                    self.arm_timer(now);
                }
            }
            State::SynSent => {
                if h.flags & (FLAG_SYN | FLAG_ACK) == (FLAG_SYN | FLAG_ACK) && h.ack == self.snd_nxt
                {
                    self.rcv_nxt = h.seq.wrapping_add(1);
                    self.snd_una = h.ack;
                    self.snd_wnd = h.window;
                    self.state = State::Established;
                    self.retransmit_at = None;
                    self.retries = 0;
                    self.rto_ms = INITIAL_RTO_MS;
                    self.need_ack = true;
                }
            }
            _ => self.on_synchronized(h, payload, now),
        }
    }

    fn on_synchronized(&mut self, h: &Header, payload: &[u8], now: u64) {
        // ACK: advance snd_una and release acknowledged data.
        if h.flags & FLAG_ACK != 0 && seq_lt(self.snd_una, h.ack) && seq_le(h.ack, self.snd_nxt) {
            let acked = h.ack.wrapping_sub(self.snd_una) as usize;
            let syn_or_fin = usize::from(self.state == State::SynReceived)
                + usize::from(self.fin_sent && h.ack == self.snd_nxt);
            self.send.drop_front(acked - syn_or_fin.min(acked));
            self.snd_una = h.ack;
            self.snd_wnd = h.window;
            self.retries = 0;
            self.rto_ms = INITIAL_RTO_MS;
            self.retransmit_at = (self.inflight() > 0).then_some(now + self.rto_ms);
            match self.state {
                State::SynReceived => self.state = State::Established,
                State::FinWait1 if self.fin_sent && h.ack == self.snd_nxt => {
                    self.state = State::FinWait2;
                    self.fin_wait2_until = now + FIN_WAIT2_MS;
                }
                State::Closing if self.fin_sent && h.ack == self.snd_nxt => {
                    self.enter_time_wait(now)
                }
                State::LastAck if self.fin_sent && h.ack == self.snd_nxt => {
                    self.finish(CloseReason::Graceful)
                }
                _ => {}
            }
        } else if h.flags & FLAG_ACK != 0 {
            self.snd_wnd = h.window;
        }
        // In-order data.
        if !payload.is_empty() {
            if h.seq == self.rcv_nxt && !self.peer_fin {
                let n = self.recv.push(payload);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
            }
            // Out of order or duplicate: ACK only (the peer retransmits).
            self.need_ack = true;
        }
        // FIN from the peer (after its data).
        if h.flags & FLAG_FIN != 0
            && !self.peer_fin
            && h.seq.wrapping_add(payload.len() as u32) == self.rcv_nxt
        {
            self.peer_fin = true;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            self.need_ack = true;
            match self.state {
                State::Established | State::SynReceived => self.state = State::CloseWait,
                State::FinWait1 => {
                    if self.fin_sent && self.snd_una == self.snd_nxt {
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
    }

    /// The next time `poll` has timed work to do (ADR 0044): a
    /// retransmission, the end of TIME-WAIT, or giving up FIN-WAIT-2.
    /// `None` when only a segment
    /// or a call from the user can change anything, so a service may sleep
    /// until then; work that is due at once (a first SYN, new data, an ACK)
    /// is what `poll` emits in the same round and needs no timer.
    pub fn next_timer(&self) -> Option<u64> {
        match self.state {
            State::Closed => None,
            State::TimeWait => Some(self.time_wait_until),
            State::FinWait2 => Some(self.fin_wait2_until),
            State::FinWait1 | State::Closing | State::LastAck => {
                let closing = self.closing_until;
                match (self.retransmit_at, closing) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            }
            _ => self.retransmit_at,
        }
    }
}

/// Of `(slot, closed by its client, connection)`, the closed connection in
/// TIME-WAIT that ends soonest: the slot a full table may give to a new
/// connection (incident 0027). TIME-WAIT guards that connection's own
/// address and port pair, which the new one does not share. A connection
/// its client still holds is never chosen.
pub fn recyclable<'a>(socks: impl Iterator<Item = (usize, bool, &'a Connection)>) -> Option<usize> {
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
    use std::vec::Vec;

    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
    const SECRET: isn::Secret = [0x5a; isn::SECRET_LEN];

    /// Two endpoints and a wire with controlled losses. Each side starts at
    /// the RFC 6528 ISN of its own 4-tuple, as the service does.
    struct Pair {
        a: Connection,
        b: Connection,
        now: u64,
        /// Returns `true` to drop the n-th segment.
        drop: fn(usize) -> bool,
        count: usize,
        delivered: usize,
    }

    impl Pair {
        fn new(drop: fn(usize) -> bool) -> Self {
            let mut a = Connection::new(A, 40000, B, 80);
            let mut b = Connection::new(B, 80, A, 40000);
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
    fn time_wait_from(at: u64) -> Connection {
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
    /// are retransmitted, TIME-WAIT ends, and an unanswered connection
    /// gives up exactly at the end of its schedule.
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
        assert_eq!(p.b.next_timer(), None);
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
        assert_eq!(
            p.now, FULL_SCHEDULE_MS,
            "gave up at the end of its schedule"
        );
    }

    #[test]
    fn gives_up_after_too_many_retries() {
        let mut p = Pair::new(|_| true);
        p.run(60_000);
        assert_eq!(p.a.state(), State::Closed);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
        assert!(p.a.retries() > MAX_RETRIES);
    }

    /// Time at which a connection whose segments are all lost gives up:
    /// the RTO schedule 0.5, 1, 2, 4, 8, 8, 8 s puts it at 31.5 s for an
    /// active open or data (ADR 0035 leaves it unchanged).
    const FULL_SCHEDULE_MS: u64 = 500 + 1000 + 2000 + 4000 + 8000 + 8000 + 8000;

    /// Incident 0013: a passive connection whose SYN|ACK is never answered
    /// frees itself after `SYN_ACK_MAX_RETRIES` retransmissions (0.5, 1, 2 s
    /// and a last 4 s wait: 7.5 s), not after the 31 s data schedule.
    #[test]
    fn unanswered_syn_ack_gives_up_within_the_short_budget() {
        let mut b = Connection::new(B, 80, A, 40000);
        b.listen(isn::initial_seq(&SECRET, B, 80, A, 40000, 0));
        let syn = Header {
            src_port: 40000,
            dst_port: 80,
            seq: 1000,
            ack: 0,
            flags: FLAG_SYN,
            window: 4096,
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
            assert!(
                now <= FULL_SCHEDULE_MS,
                "still holding its slot at {now} ms"
            );
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
    /// data on an established connection.
    #[test]
    fn active_open_and_data_keep_the_full_schedule() {
        let mut p = Pair::new(|_| true);
        p.run(FULL_SCHEDULE_MS - 100);
        assert_eq!(p.a.state(), State::SynSent, "still trying before 31.5 s");
        p.run(200);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));

        let mut p = Pair::new(|_| false);
        p.run(10);
        assert!(p.a.is_established() && p.b.is_established());
        p.drop = |_| true;
        assert_eq!(p.a.send(b"lost"), 4);
        p.run(FULL_SCHEDULE_MS - 100);
        assert!(p.a.is_established(), "still retransmitting before 31.5 s");
        p.run(200);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
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
        let mut b = Connection::new(B, 22, A, 40000);
        b.listen(isn::initial_seq(&SECRET, B, 22, A, 40000, 0));
        let syn_rst = Header {
            src_port: 40000,
            dst_port: 22,
            seq: 1000,
            ack: 0,
            flags: FLAG_SYN | FLAG_RST,
            window: 4096,
        };
        b.on_segment(&syn_rst, &[], 0);
        assert_eq!(b.state(), State::Listen);
        assert!(!b.is_closed() && !b.is_established());
        let mut buf = [0u8; MAX_SEGMENT];
        for now in (0..FULL_SCHEDULE_MS).step_by(500) {
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
            ack: 0,
            flags: FLAG_SYN,
            window: 65535,
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
        let n = r.write(B, A, None, &[], &mut buf).unwrap();
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
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let mut sent = 0;
        let mut got = Vec::new();
        let mut out = [0u8; 512];
        for _ in 0..4000 {
            if p.a.is_established() && sent < data.len() {
                sent += p.a.send(&data[sent..]);
            }
            p.run(10);
            loop {
                let n = p.b.recv(&mut out);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&out[..n]);
            }
            if got.len() == data.len() {
                break;
            }
        }
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
            let mut b = Connection::new(B, 80, A, 40000);
            b.listen(iss);
            let syn = Header {
                src_port: 40000,
                dst_port: 80,
                seq: peer_seq,
                ack: 0,
                flags: FLAG_SYN,
                window: 4096,
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
    fn header_round_trip_and_bad_checksum() {
        let h = Header {
            src_port: 40000,
            dst_port: 80,
            seq: 1,
            ack: 2,
            flags: FLAG_SYN,
            window: 4096,
        };
        let mut buf = [0u8; 64];
        let n = h.write(A, B, Some(1400), b"", &mut buf).unwrap();
        assert_eq!(n, 24);
        let (parsed, payload) = Header::parse(A, B, &buf[..n]).unwrap();
        assert_eq!(parsed, h);
        assert!(payload.is_empty());
        assert!(
            Header::parse(A, Ipv4Addr::new(10, 0, 2, 3), &buf[..n]).is_none(),
            "another address"
        );
        assert!(seq_lt(u32::MAX - 1, 1) && !seq_lt(1, u32::MAX - 1));
    }

    /// A connection on port 22 that sent 10 of 100 bytes into a 10-byte
    /// window and was closed by its owner.
    fn closed_with_data_pending(seg: &mut [u8]) -> (Connection, u32) {
        let me = Ipv4Addr::new(10, 0, 2, 15);
        let peer = Ipv4Addr::new(10, 0, 2, 2);
        let mut c = Connection::new(me, 22, peer, 4000);
        c.listen(1000);
        let h = |seq, ack, flags, window| Header {
            src_port: 4000,
            dst_port: 22,
            seq,
            ack,
            flags,
            window,
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
    /// closed connection in FIN-WAIT-1 with no timer. It now ends in RST
    /// after `CLOSING_MS`.
    #[test]
    fn a_closed_connection_gives_up_after_closing_ms() {
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
        };
        c.on_segment(&ack, &[], 3);
        assert_eq!(c.state(), State::FinWait1);
        let at = c.next_timer().expect("a deadline");
        assert!(at <= 2 + CLOSING_MS);
        assert!(c.poll(at - 1, &mut seg).is_none() || c.state() == State::FinWait1);
        let n = c.poll(at, &mut seg).expect("an RST");
        let (h, _) = Header::parse(
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 2),
            &seg[..n],
        )
        .unwrap();
        assert!(h.flags & FLAG_RST != 0);
        assert!(c.is_closed());
        assert_eq!(c.close_reason(), Some(CloseReason::Timeout));
    }
}
