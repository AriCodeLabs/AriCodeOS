// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Minimal TCP (RFC 793) without `alloc`: header, and a connection with fixed
//! buffers, retransmission with an exponential timer and orderly close. The
//! connection sends nothing on its own: `poll` produces at most one
//! segment per call and the caller transmits it.

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
const MAX_RETRIES: u8 = 6;
const TIME_WAIT_MS: u64 = 2000;

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

    /// Passive open: waits for a SYN.
    pub fn listen(&mut self) {
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
        // RST after abort.
        if self.state == State::Closed {
            if self.need_ack && self.reason == Some(CloseReason::Reset) {
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
        // Timer-driven retransmission.
        if let Some(at) = self.retransmit_at {
            if now >= at {
                self.retries += 1;
                if self.retries > MAX_RETRIES {
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
        if matches!(
            self.state,
            State::Established | State::CloseWait | State::FinWait1 | State::LastAck
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
                    let iss = h.seq.wrapping_mul(2654435761).wrapping_add(0x4152_4900);
                    self.snd_una = iss;
                    self.snd_nxt = iss;
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
                    self.state = State::FinWait2
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
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

    /// Two endpoints and a wire with controlled losses.
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
            b.listen();
            a.connect(1000, 0);
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

    #[test]
    fn gives_up_after_too_many_retries() {
        let mut p = Pair::new(|_| true);
        p.run(60_000);
        assert_eq!(p.a.state(), State::Closed);
        assert_eq!(p.a.close_reason(), Some(CloseReason::Timeout));
        assert!(p.a.retries() > MAX_RETRIES);
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
}
