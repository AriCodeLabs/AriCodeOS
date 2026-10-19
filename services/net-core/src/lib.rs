// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Network service core (Phase 22, ADR 0024): minimal stack (ARP, IPv4,
//! ICMP, UDP, DHCP, DNS, TCP) and `NetServiceProtocol v1` server,
//! independent of the NIC. Each driver supplies a [`Link`] (MAC, send
//! and receive frames) and calls [`run`]; virtio-net and rtl-net share it.
//! Single thread: alternates between reception, IPC requests and timeouts.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use aricode_net::tcp::{self, Connection};
use aricode_net::{arp, dhcp, dns, eth, icmp, ipv4, isn, udp, Ipv4Addr, MacAddr};
use aricode_proto::net::{Info, Op, Reply, Request, Status, TcpStats, MAX_DATAGRAM};
use aricode_rt::sys::virtio_pci::{IRQ, SERVICE};
use aricode_rt::sys::{self, abi, Handle};

pub const MAX_FRAME: usize = eth::HEADER_LEN + eth::MTU;

const MAX_PINGS: usize = 4;
const MAX_SOCKETS: usize = 8;
const MAX_DNS: usize = 4;
const MAX_TCP: usize = 8;
/// Connections one listener may hold before its owner accepts them, half-open
/// included (incident 0013, ADR 0035): a SYN beyond it, or with no free
/// slot, is answered with a reset and recorded as `net syn dropped`, so a
/// burst of SYNs to port 22 leaves at least `MAX_TCP / 2 - 1` slots to
/// active opens. The reasoning is with `tcp::listen_backlog`.
const TCP_BACKLOG: usize = tcp::listen_backlog(MAX_TCP);
const _: () = assert!(TCP_BACKLOG >= 1 && 1 + TCP_BACKLOG < MAX_TCP);
/// Fault event when the kernel gave fewer than `isn::SECRET_LEN` random
/// bytes for the TCP secret (ADR 0034); its code is the number obtained.
const TCP_SECRET_FAULT: &str = "net tcp secret degraded";
const _: () = assert!(TCP_SECRET_FAULT.len() <= abi::sysinfo::EVENT_NAME_LEN);
const PING_ID: u16 = 0xA51C;
const DHCP_RETRY_MS: u64 = 1000;
/// Wait for an answer to the ARP probe of an acknowledged address.
const DHCP_PROBE_MS: u64 = 600;
/// Pause before asking again after declining an address in use.
const DHCP_DECLINE_PAUSE_MS: u64 = 10_000;
const ARP_WAIT_MS: u64 = 1000;

/// Why netd refused a request or lost a frame (incident 0012). Each kind is
/// a fault event in the kernel ring, recorded at most once per second per
/// kind (`aricode_net::faults`): the name is `<kind> x<count>` (refusals of
/// that kind in that second) and the code is the `net::Status` number the
/// client got (`diag`: `fault reported by process N: net tcp table full x3
/// (code 5)`). `BadSock`, `InUse`, `NoRoute`, `NotFound`, `Timeout` and
/// `Reset` are not refusals: they describe the client's socket or the
/// network's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// `TcpConnect`/`TcpListen` with every TCP slot in use (`MAX_TCP`,
    /// TIME-WAIT and unreaped connections included).
    TcpTableFull = 0,
    /// A frame for an unresolved next hop found the parking table full.
    ArpDeferFull = 1,
    /// The NIC had no free transmit slot; the frame was lost.
    TxBusy = 2,
    /// A parked frame was dropped: no ARP reply within `ARP_WAIT_MS`.
    ArpNoReply = 3,
    PingTableFull = 4,
    DnsTableFull = 5,
    UdpTableFull = 6,
    /// A second `recv`/`accept` on a socket that already has one waiting.
    WaitPending = 7,
    /// Undecodable or out-of-range request (`Status::Invalid`).
    BadRequest = 8,
    /// Client buffer missing or not mappable (`Status::BadHandles`).
    BadHandles = 9,
    /// A SYN to a listening port answered with a reset: no free TCP slot or
    /// the listener's backlog at `TCP_BACKLOG` (incident 0013). The peer
    /// is remote, so the status is the one a local client gets for a full
    /// table (`Busy`).
    SynDropped = 10,
}

impl Refusal {
    const COUNT: usize = 11;
    const ALL: [Refusal; Self::COUNT] = [
        Refusal::TcpTableFull,
        Refusal::ArpDeferFull,
        Refusal::TxBusy,
        Refusal::ArpNoReply,
        Refusal::PingTableFull,
        Refusal::DnsTableFull,
        Refusal::UdpTableFull,
        Refusal::WaitPending,
        Refusal::BadRequest,
        Refusal::BadHandles,
        Refusal::SynDropped,
    ];

    fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    /// Kind name; with `" x999"` appended it must fit an event name.
    const fn name(self) -> &'static str {
        match self {
            Refusal::TcpTableFull => "net tcp table full",
            Refusal::ArpDeferFull => "net arp defer full",
            Refusal::TxBusy => "net tx busy",
            Refusal::ArpNoReply => "net arp no reply",
            Refusal::PingTableFull => "net ping table full",
            Refusal::DnsTableFull => "net dns table full",
            Refusal::UdpTableFull => "net udp table full",
            Refusal::WaitPending => "net wait pending",
            Refusal::BadRequest => "net bad request",
            Refusal::BadHandles => "net bad handles",
            Refusal::SynDropped => "net syn dropped",
        }
    }

    /// Status the client receives (its number is the event code); a lost
    /// frame carries the status its sender would have got.
    fn status(self) -> Status {
        match self {
            Refusal::BadRequest => Status::Invalid,
            Refusal::BadHandles => Status::BadHandles,
            _ => Status::Busy,
        }
    }
}

// Every kind name leaves room for " x999" in a kernel event name.
const _: () = {
    assert!(aricode_net::faults::LABEL_LEN == abi::sysinfo::EVENT_NAME_LEN);
    let mut i = 0;
    while i < Refusal::COUNT {
        assert!(Refusal::ALL[i].name().len() + 5 <= aricode_net::faults::LABEL_LEN);
        i += 1;
    }
};

/// Link to the NIC: complete Ethernet frames, no headers of its own.
pub trait Link {
    fn mac(&self) -> MacAddr;
    /// Transmits a frame; `false` if there is no free slot (the caller loses it).
    fn send_frame(&mut self, frame: &[u8]) -> bool;
    /// Next received frame copied into `out`, or `None` if there is none.
    fn recv_frame(&mut self, out: &mut [u8; MAX_FRAME]) -> Option<usize>;
    /// Physical link up, down or unknown (`None`).
    fn link_up(&self) -> Option<bool>;
}

/// Trace line prefixed with the driver name.
pub fn say(name: &str, msg: &[u8]) {
    sys::print(name.as_bytes());
    sys::print(b": ");
    sys::print(msg);
    sys::print(b"\n");
}

fn print_mac(mac: MacAddr) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, b) in mac.0.iter().enumerate() {
        if i > 0 {
            sys::print(b":");
        }
        sys::print(&[HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
    }
}

fn print_ip(ip: Ipv4Addr) {
    let mut b = [0u8; 15];
    sys::print(ip.format(&mut b));
}

/// Clock in ms based on `system_info`.
pub struct Clock(pub Handle);

impl Clock {
    pub fn now(&self) -> u64 {
        let mut b = [0u8; 8];
        match sys::system_info(self.0, abi::sysinfo::UPTIME, &mut b) {
            Ok(8) => u64::from_le_bytes(b),
            _ => 0,
        }
    }
}

struct PendingPing {
    reply_to: Handle,
    dst: Ipv4Addr,
    seq: u16,
    sent_at: u64,
    deadline: u64,
}

struct PendingRecv {
    reply_to: Handle,
    buf: Handle,
    max_len: u16,
    deadline: u64,
}

struct Socket {
    port: u16,
    recv: Option<PendingRecv>,
    /// Last uncollected datagram (sender, port, data).
    queued: Option<(Ipv4Addr, u16, usize)>,
    data: [u8; MAX_DATAGRAM as usize],
}

/// TCP socket: one connection (or a listening port) and its waits.
struct TcpSock {
    conn: Connection,
    listening: bool,
    /// Incoming connection not yet handed out by `TcpAccept` (socket index).
    accepted_from: Option<u16>,
    connect: Option<(Handle, u64)>,
    accept: Option<(Handle, u64)>,
    recv: Option<PendingRecv>,
    /// The user already closed it: free the slot when the connection ends.
    user_closed: bool,
}

impl TcpSock {
    fn new(conn: Connection) -> Self {
        Self {
            conn,
            listening: false,
            accepted_from: None,
            connect: None,
            accept: None,
            recv: None,
            user_closed: false,
        }
    }
}

struct PendingDns {
    reply_to: Handle,
    id: u16,
    deadline: u64,
}

/// Frame waiting for ARP resolution.
struct Deferred {
    next_hop: Ipv4Addr,
    len: usize,
    frame: [u8; MAX_FRAME],
    deadline: u64,
}

/// Client buffer mappings by physical address.
struct ClientBuffers {
    entries: [(u64, u64, u64); 8],
    count: usize,
}

impl ClientBuffers {
    fn map(&mut self, dma: Handle) -> Option<&mut [u8]> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        let virt = match self.entries[..self.count].iter().find(|e| e.0 == phys) {
            Some(e) => e.1,
            None => {
                let v = sys::mmio_map(dma).ok()?;
                if self.count < self.entries.len() {
                    self.entries[self.count] = (phys, v, len);
                    self.count += 1;
                }
                v
            }
        };
        // SAFETY: client buffer mapped RW in this process.
        Some(unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, len as usize) })
    }
}

pub struct Stack<L: Link> {
    link: L,
    name: &'static str,
    pub clock: Clock,
    dhcp: dhcp::Client,
    dhcp_next: u64,
    /// End of the ARP probe of the acknowledged address (0: no probe).
    probe_deadline: u64,
    config: Option<dhcp::Config>,
    arp: arp::Cache<8>,
    ip_id: u16,
    dns_id: u16,
    pings: [Option<PendingPing>; MAX_PINGS],
    sockets: [Option<Socket>; MAX_SOCKETS],
    dns: [Option<PendingDns>; MAX_DNS],
    /// On the heap: each socket carries 8 KiB of buffers and the stack is 64 KiB.
    tcp: [Option<Box<TcpSock>>; MAX_TCP],
    /// Per-boot key of the initial sequence numbers and the ephemeral
    /// ports (`aricode_net::isn`, ADR 0034), from the kernel's entropy.
    tcp_secret: isn::Secret,
    /// Connections opened so far: the RFC 6056 port counter.
    tcp_port_count: u32,
    /// SYNs to a listener answered with a reset since boot (`TcpStats`).
    tcp_syn_dropped: u32,
    deferred: [Option<Deferred>; 4],
    buffers: ClientBuffers,
    /// Ephemeral UDP port for DNS queries.
    dns_port: u16,
    /// Client buffer kept by a pending request (receives):
    /// the main loop does not close it after dispatching.
    retained: Option<Handle>,
    /// Refusals recorded at most once per second per kind (incident 0012).
    faults: aricode_net::faults::Meter<{ Refusal::COUNT }>,
}

fn new_socket(port: u16) -> Socket {
    Socket {
        port,
        recv: None,
        queued: None,
        data: [0; MAX_DATAGRAM as usize],
    }
}

/// The per-boot TCP secret (ADR 0034): `isn::SECRET_LEN` bytes from the
/// kernel generator (`rdrand`, or the TSC and the interrupt pool on CPUs
/// without it; the same source sshd draws from), read with the `SYSTEM`
/// handle the clock already holds (`READ | TRANSFER` from init; the kernel
/// checks `READ`). The kernel answers with every byte asked whenever it
/// answers and exposes no entropy-quality state, so the only failure this
/// can see is a refused syscall, and that one is not hidden: it records the
/// fault event `TCP_SECRET_FAULT` (code = bytes obtained), says so on the
/// console and fills the rest from the clock, so that boot's numbers are as
/// predictable as before incident 0014 and `diag` shows it.
fn tcp_secret(clock: &Clock, name: &str) -> isn::Secret {
    let mut secret = [0u8; isn::SECRET_LEN];
    let got = sys::random(clock.0, &mut secret).unwrap_or(0);
    if got >= isn::SECRET_LEN {
        say(name, b"tcp secret from kernel entropy");
    } else {
        let _ = sys::event_record(got as u64, TCP_SECRET_FAULT);
        say(
            name,
            b"fault net tcp secret degraded: too few random bytes, ISNs predictable",
        );
        let clock_bytes = clock.now().to_le_bytes();
        for (i, b) in secret[got..].iter_mut().enumerate() {
            *b ^= clock_bytes[i % clock_bytes.len()];
        }
    }
    secret
}

impl<L: Link> Stack<L> {
    pub fn new(link: L, clock: Clock, name: &'static str) -> Self {
        let xid = 0x4152_4900 | (link.mac().0[5] as u32);
        Self {
            dhcp: dhcp::Client::new(link.mac(), xid),
            link,
            name,
            tcp_secret: tcp_secret(&clock, name),
            clock,
            dhcp_next: 0,
            probe_deadline: 0,
            config: None,
            arp: arp::Cache::new(),
            ip_id: 1,
            dns_id: 1,
            pings: [const { None }; MAX_PINGS],
            sockets: [const { None }; MAX_SOCKETS],
            dns: [const { None }; MAX_DNS],
            tcp: [const { None }; MAX_TCP],
            tcp_port_count: 0,
            tcp_syn_dropped: 0,
            deferred: [const { None }; 4],
            buffers: ClientBuffers {
                entries: [(0, 0, 0); 8],
                count: 0,
            },
            dns_port: 49153,
            retained: None,
            faults: aricode_net::faults::Meter::new(),
        }
    }

    /// Accounts a refusal or a lost frame: recorded at once if the kind was
    /// quiet for a second, otherwise counted and recorded when the second
    /// ends (`expire`).
    fn note(&mut self, what: Refusal, now: u64) {
        let count = self.faults.report(what as usize, now);
        if count > 0 {
            self.record(what, count);
        }
    }

    /// A fault event in the kernel ring (`<kind> x<count>`, code = status
    /// number) and the same line on the console.
    fn record(&self, what: Refusal, count: u32) {
        let mut label = [0u8; aricode_net::faults::LABEL_LEN];
        let name = aricode_net::faults::label(what.name(), count, &mut label);
        let _ = sys::event_record(what.status() as u64, name);
        sys::print(self.name.as_bytes());
        sys::print(b": fault ");
        sys::print(name.as_bytes());
        sys::print(b"\n");
    }

    /// Refuses a request: records it and answers with the matching status.
    fn refuse(&mut self, reply_to: Handle, what: Refusal, now: u64) {
        self.note(what, now);
        answer(reply_to, Reply::status(what.status()));
    }

    fn my_ip(&self) -> Ipv4Addr {
        self.config
            .map(|c| c.address)
            .unwrap_or(Ipv4Addr::UNSPECIFIED)
    }

    /// RFC 6528 initial sequence number of a connection (ADR 0034).
    fn initial_seq(
        &self,
        local: Ipv4Addr,
        local_port: u16,
        remote: Ipv4Addr,
        remote_port: u16,
        now: u64,
    ) -> u32 {
        isn::initial_seq(
            &self.tcp_secret,
            local,
            local_port,
            remote,
            remote_port,
            now,
        )
    }

    // ---------------- transmit ----------------

    /// Sends an Ethernet frame whose `payload` is built by `fill`.
    fn send_eth(
        &mut self,
        dst: MacAddr,
        ethertype: u16,
        fill: impl FnOnce(&mut [u8]) -> Option<usize>,
    ) -> bool {
        let mut frame = [0u8; MAX_FRAME];
        let hdr = eth::Header {
            dst,
            src: self.link.mac(),
            ethertype,
        };
        let Some(h) = hdr.write(&mut frame) else {
            return false;
        };
        let Some(n) = fill(&mut frame[h..]) else {
            return false;
        };
        self.link.send_frame(&frame[..h + n])
    }

    /// Next hop for `dst`: the destination itself if it is on the subnet,
    /// otherwise the router.
    fn next_hop(&self, dst: Ipv4Addr) -> Option<Ipv4Addr> {
        let c = self.config?;
        if dst == Ipv4Addr::BROADCAST || dst.same_subnet(c.address, c.subnet_mask) {
            Some(dst)
        } else if !c.router.is_unspecified() {
            Some(c.router)
        } else {
            None
        }
    }

    /// Sends an IPv4 packet to `dst` with a `payload` of protocol `proto`.
    /// With no known MAC it parks the packet and sends an ARP request.
    /// Every failure other than `NoRoute` is recorded (`note`) here, where
    /// its cause is known; callers only pass the status on.
    fn send_ipv4(
        &mut self,
        dst: Ipv4Addr,
        proto: u8,
        payload: &[u8],
        now: u64,
    ) -> Result<(), Status> {
        let src = self.my_ip();
        let next_hop = self.next_hop(dst).ok_or(Status::NoRoute)?;
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        let mut frame = [0u8; MAX_FRAME];
        let ip_hdr = ipv4::Header::new(src, dst, proto, id, payload.len() as u16);
        let mut n = eth::HEADER_LEN;
        let header = ip_hdr.write(&mut frame[n..]);
        let fits = header.is_some_and(|h| frame.len() >= n + h + payload.len());
        let Some(h) = header.filter(|_| fits) else {
            self.note(Refusal::BadRequest, now);
            return Err(Status::Invalid);
        };
        n += h;
        frame[n..n + payload.len()].copy_from_slice(payload);
        n += payload.len();
        let dst_mac = if dst == Ipv4Addr::BROADCAST {
            Some(MacAddr::BROADCAST)
        } else {
            self.arp.lookup(next_hop)
        };
        match dst_mac {
            Some(mac) => {
                eth::Header {
                    dst: mac,
                    src: self.link.mac(),
                    ethertype: eth::ETHERTYPE_IPV4,
                }
                .write(&mut frame);
                if self.link.send_frame(&frame[..n]) {
                    Ok(())
                } else {
                    self.note(Refusal::TxBusy, now);
                    Err(Status::Busy)
                }
            }
            None => {
                let Some(slot) = self.deferred.iter_mut().find(|d| d.is_none()) else {
                    self.note(Refusal::ArpDeferFull, now);
                    return Err(Status::Busy);
                };
                *slot = Some(Deferred {
                    next_hop,
                    len: n,
                    frame,
                    deadline: now + ARP_WAIT_MS,
                });
                self.send_arp_request(next_hop);
                Ok(())
            }
        }
    }

    fn send_arp_request(&mut self, target: Ipv4Addr) {
        let req = arp::Packet::request(self.link.mac(), self.my_ip(), target);
        self.send_eth(MacAddr::BROADCAST, eth::ETHERTYPE_ARP, |b| req.write(b));
    }

    fn send_udp(
        &mut self,
        src_port: u16,
        dst: Ipv4Addr,
        dst_port: u16,
        payload: &[u8],
        now: u64,
    ) -> Result<(), Status> {
        let mut seg = [0u8; udp::HEADER_LEN + MAX_DATAGRAM as usize];
        let Some(n) =
            (udp::Header { src_port, dst_port }).write(self.my_ip(), dst, payload, &mut seg)
        else {
            self.note(Refusal::BadRequest, now);
            return Err(Status::Invalid);
        };
        self.send_ipv4(dst, ipv4::PROTO_UDP, &seg[..n], now)
    }

    /// Sends the appropriate DHCP message (broadcast from 0.0.0.0).
    fn dhcp_step(&mut self, now: u64) {
        match self.dhcp.state() {
            dhcp::State::Bound => return,
            dhcp::State::Probing => {
                // Nobody claimed the address during the probe: use it.
                if now >= self.probe_deadline {
                    self.probe_deadline = 0;
                    self.dhcp.address_free();
                    self.apply_lease();
                }
                return;
            }
            _ => {}
        }
        if now < self.dhcp_next {
            return;
        }
        let mut msg = [0u8; dhcp::MAX_LEN];
        if let Some(n) = self.dhcp.poll(&mut msg) {
            self.broadcast_dhcp(&msg[..n]);
        }
        self.dhcp_next = now + DHCP_RETRY_MS;
    }

    fn broadcast_dhcp(&mut self, msg: &[u8]) {
        let mut seg = [0u8; udp::HEADER_LEN + dhcp::MAX_LEN];
        let Some(n) = (udp::Header {
            src_port: dhcp::CLIENT_PORT,
            dst_port: dhcp::SERVER_PORT,
        })
        .write(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, msg, &mut seg) else {
            return;
        };
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        let hdr = ipv4::Header::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            ipv4::PROTO_UDP,
            id,
            n as u16,
        );
        self.send_eth(MacAddr::BROADCAST, eth::ETHERTYPE_IPV4, |b| {
            let h = hdr.write(b)?;
            b.get_mut(h..h + n)?.copy_from_slice(&seg[..n]);
            Some(h + n)
        });
    }

    // ---------------- receive ----------------

    fn on_frame(&mut self, frame: &[u8], now: u64) {
        let Some((eh, payload)) = eth::Header::parse(frame) else {
            return;
        };
        if eh.dst != self.link.mac() && !eh.dst.is_broadcast() {
            return;
        }
        match eh.ethertype {
            eth::ETHERTYPE_ARP => self.on_arp(payload, now),
            eth::ETHERTYPE_IPV4 => self.on_ipv4(payload, now),
            _ => {}
        }
    }

    /// The acknowledged and probed address becomes the configuration.
    fn apply_lease(&mut self) {
        self.config = self.dhcp.config();
        if let Some(c) = self.config {
            sys::print(self.name.as_bytes());
            sys::print(b": DHCP ");
            print_ip(c.address);
            sys::print(b" mask ");
            print_ip(c.subnet_mask);
            sys::print(b" router ");
            print_ip(c.router);
            sys::print(b" dns ");
            print_ip(c.dns);
            sys::print(b"\n");
            let router = c.router;
            self.send_arp_request(router);
        }
    }

    fn on_arp(&mut self, b: &[u8], now: u64) {
        let Some(p) = arp::Packet::parse(b) else {
            return;
        };
        if self.dhcp.probing().is_some_and(|a| a == p.sender_ip) && p.sender_mac != self.link.mac()
        {
            // Someone answered the probe: decline and start over later.
            sys::print(self.name.as_bytes());
            sys::print(b": DHCP address ");
            print_ip(p.sender_ip);
            sys::print(b" is already used by ");
            print_mac(p.sender_mac);
            sys::print(b"; declined, asking again\n");
            let mut msg = [0u8; dhcp::MAX_LEN];
            if let Some(n) = self.dhcp.address_in_use(&mut msg) {
                self.broadcast_dhcp(&msg[..n]);
            }
            self.probe_deadline = 0;
            self.dhcp_next = now + DHCP_DECLINE_PAUSE_MS;
            return;
        }
        if !p.sender_ip.is_unspecified() {
            self.arp.insert(p.sender_ip, p.sender_mac);
            self.flush_deferred(p.sender_ip, p.sender_mac, now);
        }
        if p.op == arp::OP_REQUEST && p.target_ip == self.my_ip() && !self.my_ip().is_unspecified()
        {
            let reply = arp::Packet::reply_to(&p, self.link.mac(), self.my_ip());
            self.send_eth(p.sender_mac, eth::ETHERTYPE_ARP, |out| reply.write(out));
        }
    }

    fn flush_deferred(&mut self, ip: Ipv4Addr, mac: MacAddr, now: u64) {
        let mut lost = false;
        for slot in self.deferred.iter_mut() {
            if slot.as_ref().is_some_and(|d| d.next_hop == ip) {
                let mut d = slot.take().unwrap();
                eth::Header {
                    dst: mac,
                    src: self.link.mac(),
                    ethertype: eth::ETHERTYPE_IPV4,
                }
                .write(&mut d.frame);
                lost |= !self.link.send_frame(&d.frame[..d.len]);
            }
        }
        if lost {
            self.note(Refusal::TxBusy, now);
        }
    }

    fn on_ipv4(&mut self, b: &[u8], now: u64) {
        let Some((ih, payload)) = ipv4::Header::parse(b) else {
            return;
        };
        let mine = self.my_ip();
        let for_me = ih.dst == mine || ih.dst == Ipv4Addr::BROADCAST || mine.is_unspecified();
        if !for_me {
            return;
        }
        match ih.protocol {
            ipv4::PROTO_ICMP => self.on_icmp(ih, payload, now),
            ipv4::PROTO_UDP => self.on_udp(ih, payload, now),
            ipv4::PROTO_TCP => self.on_tcp(ih, payload, now),
            _ => {}
        }
    }

    fn on_icmp(&mut self, ih: ipv4::Header, b: &[u8], now: u64) {
        let Some((echo, data)) = icmp::Echo::parse(b) else {
            return;
        };
        if !echo.reply {
            // Answer the echo: the host may ping us.
            let reply = icmp::Echo {
                reply: true,
                ..echo
            };
            let mut out = [0u8; icmp::HEADER_LEN + eth::MTU];
            if let Some(n) = reply.write(data, &mut out) {
                let _ = self.send_ipv4(ih.src, ipv4::PROTO_ICMP, &out[..n], now);
            }
            return;
        }
        if echo.identifier != PING_ID {
            return;
        }
        for slot in self.pings.iter_mut() {
            if slot
                .as_ref()
                .is_some_and(|p| p.seq == echo.sequence && p.dst == ih.src)
            {
                let p = slot.take().unwrap();
                answer(
                    p.reply_to,
                    Reply::ok(now.saturating_sub(p.sent_at)).with_addr(ih.src.0, 0),
                );
                return;
            }
        }
    }

    fn on_udp(&mut self, ih: ipv4::Header, b: &[u8], now: u64) {
        let Some((uh, data)) = udp::Header::parse(ih.src, ih.dst, b) else {
            return;
        };
        if uh.dst_port == dhcp::CLIENT_PORT && self.dhcp.state() != dhcp::State::Bound {
            let mut out = [0u8; dhcp::MAX_LEN];
            let response = self.dhcp.on_message(data, &mut out);
            if let Some(n) = response {
                self.broadcast_dhcp(&out[..n]);
            }
            if let Some(address) = self.dhcp.probing() {
                // Acknowledged: ARP probe (sender 0.0.0.0) before using it,
                // in case another host already has the address (RFC 2131
                // 4.4.1); `dhcp_step` applies the lease when nobody answers.
                let probe = arp::Packet::request(self.link.mac(), Ipv4Addr::UNSPECIFIED, address);
                self.send_eth(MacAddr::BROADCAST, eth::ETHERTYPE_ARP, |out| {
                    probe.write(out)
                });
                self.probe_deadline = now + DHCP_PROBE_MS;
            }
            return;
        }
        if uh.dst_port == self.dns_port {
            self.on_dns(data);
            return;
        }
        let Some(sock) = self
            .sockets
            .iter_mut()
            .flatten()
            .find(|s| s.port == uh.dst_port)
        else {
            return;
        };
        let len = data.len().min(sock.data.len());
        if let Some(pending) = sock.recv.take() {
            let _ = now;
            deliver_recv(
                &mut self.buffers,
                pending,
                ih.src,
                uh.src_port,
                &data[..len],
            );
        } else {
            sock.data[..len].copy_from_slice(&data[..len]);
            sock.queued = Some((ih.src, uh.src_port, len));
        }
    }

    fn on_tcp(&mut self, ih: ipv4::Header, b: &[u8], now: u64) {
        let Some((h, payload)) = tcp::Header::parse(ih.src, ih.dst, b) else {
            return;
        };
        // Existing connection with the same 4-tuple.
        let existing = self.tcp.iter().position(|s| {
            s.as_ref().is_some_and(|s| {
                !s.listening
                    && s.conn.local_port() == h.dst_port
                    && s.conn.remote() == (ih.src, h.src_port)
            })
        });
        if let Some(i) = existing {
            self.tcp[i]
                .as_mut()
                .unwrap()
                .conn
                .on_segment(&h, payload, now);
            return;
        }
        // SYN to a listening port: new passive connection.
        if h.flags & tcp::FLAG_SYN == 0 || h.flags & tcp::FLAG_ACK != 0 {
            return;
        }
        let Some(listener) = self.tcp.iter().position(|s| {
            s.as_ref()
                .is_some_and(|s| s.listening && s.conn.local_port() == h.dst_port)
        }) else {
            return;
        };
        // No free slot, or the listener already holds `TCP_BACKLOG`
        // connections nobody accepted (half-open included): the SYN is
        // refused with a reset so the peer stops retransmitting, and the
        // refusal is recorded like every other (incident 0013, ADR 0035).
        let backlog = self
            .tcp
            .iter()
            .flatten()
            .filter(|s| s.accepted_from == Some(listener as u16))
            .count();
        let free = self.tcp.iter().position(|s| s.is_none());
        let Some(slot) = free.filter(|_| backlog < TCP_BACKLOG) else {
            self.tcp_syn_dropped = self.tcp_syn_dropped.saturating_add(1);
            self.note(Refusal::SynDropped, now);
            self.send_tcp_reset(ih, &h, payload.len(), now);
            return;
        };
        let mut conn = Connection::new(ih.dst, h.dst_port, ih.src, h.src_port);
        conn.listen(self.initial_seq(ih.dst, h.dst_port, ih.src, h.src_port, now));
        conn.on_segment(&h, payload, now);
        let mut sock = Box::new(TcpSock::new(conn));
        sock.accepted_from = Some(listener as u16);
        self.tcp[slot] = Some(sock);
    }

    /// Reset answering a segment that belongs to no connection (RFC 793
    /// section 3.4, `tcp::reset_for`). A failure to send it is recorded by
    /// `send_ipv4`; the peer then retransmits and meets the same answer.
    fn send_tcp_reset(&mut self, ih: ipv4::Header, h: &tcp::Header, payload_len: usize, now: u64) {
        let mut seg = [0u8; tcp::HEADER_LEN];
        if let Some(n) = tcp::reset_for(h, payload_len).write(ih.dst, ih.src, None, &[], &mut seg) {
            let _ = self.send_ipv4(ih.src, ipv4::PROTO_TCP, &seg[..n], now);
        }
    }

    /// The TCP table by state (`TcpStats`, incident 0013): every slot in
    /// exactly one count.
    fn tcp_stats(&self) -> TcpStats {
        let mut t = TcpStats {
            slots: MAX_TCP as u16,
            syn_dropped: self.tcp_syn_dropped,
            ..TcpStats::default()
        };
        for slot in self.tcp.iter() {
            let Some(s) = slot else {
                t.free += 1;
                continue;
            };
            let count = if s.listening {
                &mut t.listen
            } else {
                match s.conn.state() {
                    tcp::State::SynReceived => &mut t.syn_received,
                    tcp::State::SynSent => &mut t.syn_sent,
                    tcp::State::Established | tcp::State::CloseWait
                        if s.accepted_from.is_some() =>
                    {
                        &mut t.backlog
                    }
                    tcp::State::Established | tcp::State::CloseWait => &mut t.established,
                    tcp::State::TimeWait => &mut t.time_wait,
                    _ => &mut t.closing,
                }
            };
            *count += 1;
        }
        t
    }

    /// Emits pending segments and resolves waits on every connection.
    fn tcp_poll(&mut self, now: u64) {
        let mut seg = [0u8; tcp::MAX_SEGMENT];
        for i in 0..MAX_TCP {
            if self.tcp[i].as_ref().is_none_or(|s| s.listening) {
                continue;
            }
            // Take the socket out of its slot while emitting: `send_ipv4` needs
            // the rest of the stack.
            let mut sock = self.tcp[i].take().unwrap();
            let (remote, _) = sock.conn.remote();
            while let Some(n) = sock.conn.poll(now, &mut seg) {
                let _ = self.send_ipv4(remote, ipv4::PROTO_TCP, &seg[..n], now);
                if sock.conn.is_closed() {
                    break;
                }
            }
            self.tcp[i] = Some(sock);
            let Some(sock) = self.tcp[i].as_mut() else {
                continue;
            };
            // Active connection resolved.
            if let Some((reply_to, deadline)) = sock.connect {
                if sock.conn.is_established() {
                    sock.connect = None;
                    answer(reply_to, Reply::ok(i as u64));
                } else if sock.conn.is_closed() || now >= deadline {
                    sock.connect = None;
                    let status = match sock.conn.close_reason() {
                        Some(tcp::CloseReason::Reset) => Status::Reset,
                        _ => Status::Timeout,
                    };
                    sock.conn.abort();
                    sock.user_closed = true;
                    answer(reply_to, Reply::status(status));
                }
            }
            // Pending receive.
            if sock.recv.is_some()
                && (sock.conn.recv_available() > 0
                    || sock.conn.recv_finished()
                    || sock.conn.is_closed())
            {
                let pending = sock.recv.take().unwrap();
                let mut data = [0u8; MAX_DATAGRAM as usize];
                let n = sock.conn.recv(&mut data[..pending.max_len as usize]);
                if n == 0
                    && !sock.conn.recv_finished()
                    && sock.conn.close_reason() == Some(tcp::CloseReason::Reset)
                {
                    let _ = sys::handle_close(pending.buf);
                    answer(pending.reply_to, Reply::status(Status::Reset));
                } else {
                    let (from, port) = sock.conn.remote();
                    deliver_recv(&mut self.buffers, pending, from, port, &data[..n]);
                }
            }
            // Free finished connections the user already closed.
            if sock.user_closed && sock.conn.is_closed() {
                self.tcp[i] = None;
            }
        }
        // Hand accepted connections to pending `TcpAccept` calls.
        for i in 0..MAX_TCP {
            let Some(from) = self.tcp[i].as_ref().and_then(|s| s.accepted_from) else {
                continue;
            };
            if !self.tcp[i].as_ref().unwrap().conn.is_established() {
                if self.tcp[i].as_ref().unwrap().conn.is_closed() {
                    self.tcp[i] = None;
                }
                continue;
            }
            let Some(listener) = self.tcp.get_mut(from as usize).and_then(|s| s.as_mut()) else {
                self.tcp[i] = None;
                continue;
            };
            if let Some((reply_to, _)) = listener.accept.take() {
                let sock = self.tcp[i].as_mut().unwrap();
                sock.accepted_from = None;
                let (addr, port) = sock.conn.remote();
                answer(reply_to, Reply::ok(i as u64).with_addr(addr.0, port));
            }
        }
    }

    fn tcp_expire(&mut self, now: u64) {
        for sock in self.tcp.iter_mut().flatten() {
            if sock.accept.as_ref().is_some_and(|(_, d)| now >= *d) {
                let (reply_to, _) = sock.accept.take().unwrap();
                answer(reply_to, Reply::status(Status::Timeout));
            }
            if sock.recv.as_ref().is_some_and(|r| now >= r.deadline) {
                let r = sock.recv.take().unwrap();
                let _ = sys::handle_close(r.buf);
                answer(r.reply_to, Reply::status(Status::Timeout));
            }
        }
    }

    fn tcp_handle(&mut self, req: Request, reply_to: Handle, buf: Option<Handle>, now: u64) {
        let addr = Ipv4Addr(req.addr);
        match req.op {
            Op::TcpConnect => {
                if self.config.is_none() {
                    return answer(reply_to, Reply::status(Status::NoRoute));
                }
                let Some(slot) = self.tcp.iter().position(|s| s.is_none()) else {
                    return self.refuse(reply_to, Refusal::TcpTableFull, now);
                };
                let local = self.my_ip();
                // RFC 6056 walk (ADR 0034) skipping ports held by any slot,
                // listeners and TIME-WAIT included (a slot stays until
                // `tcp_poll` frees it). With `MAX_TCP` slots the answer comes
                // within `MAX_TCP + 1` candidates; `None` (range exhausted)
                // is only reachable with more slots than ports.
                let tcp = &self.tcp;
                let Some(port) = isn::pick_ephemeral_port(
                    &self.tcp_secret,
                    local,
                    addr,
                    req.port,
                    &mut self.tcp_port_count,
                    |p| tcp.iter().flatten().any(|s| s.conn.local_port() == p),
                ) else {
                    return self.refuse(reply_to, Refusal::TcpTableFull, now);
                };
                let mut conn = Connection::new(local, port, addr, req.port);
                conn.connect(self.initial_seq(local, port, addr, req.port, now), now);
                let mut sock = Box::new(TcpSock::new(conn));
                sock.connect = Some((reply_to, now + req.timeout_ms.max(1) as u64));
                self.tcp[slot] = Some(sock);
            }
            Op::TcpListen => {
                if req.port == 0 {
                    return self.refuse(reply_to, Refusal::BadRequest, now);
                }
                if self
                    .tcp
                    .iter()
                    .flatten()
                    .any(|s| s.listening && s.conn.local_port() == req.port)
                {
                    return answer(reply_to, Reply::status(Status::InUse));
                }
                let Some(slot) = self.tcp.iter().position(|s| s.is_none()) else {
                    return self.refuse(reply_to, Refusal::TcpTableFull, now);
                };
                let mut conn = Connection::new(self.my_ip(), req.port, Ipv4Addr::UNSPECIFIED, 0);
                // A listener sends nothing itself: every SYN to it gets a
                // connection of its own in `on_tcp`, with its own ISN.
                conn.listen(0);
                let mut sock = Box::new(TcpSock::new(conn));
                sock.listening = true;
                self.tcp[slot] = Some(sock);
                answer(reply_to, Reply::ok(slot as u64));
            }
            Op::TcpAccept => {
                let Some(listener) = self.tcp.get_mut(req.sock as usize).and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if !listener.listening {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if listener.accept.is_some() {
                    return self.refuse(reply_to, Refusal::WaitPending, now);
                }
                listener.accept = Some((reply_to, now + req.timeout_ms.max(1) as u64));
            }
            Op::TcpSend => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Refusal::BadHandles, now);
                };
                let len = req.len.min(MAX_DATAGRAM) as usize;
                let mut data = [0u8; MAX_DATAGRAM as usize];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => data[..len].copy_from_slice(&b[..len]),
                    _ => return self.refuse(reply_to, Refusal::BadHandles, now),
                }
                let Some(sock) = self.tcp.get_mut(req.sock as usize).and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.listening || sock.user_closed {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if sock.conn.is_closed() {
                    return answer(reply_to, Reply::status(Status::Reset));
                }
                let n = sock.conn.send(&data[..len]);
                answer(reply_to, Reply::ok(n as u64));
            }
            Op::TcpRecv => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Refusal::BadHandles, now);
                };
                let Some(sock) = self.tcp.get_mut(req.sock as usize).and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.listening || sock.user_closed {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if sock.recv.is_some() {
                    return self.refuse(reply_to, Refusal::WaitPending, now);
                }
                sock.recv = Some(PendingRecv {
                    reply_to,
                    buf,
                    max_len: req.len.min(MAX_DATAGRAM),
                    deadline: now + req.timeout_ms.max(1) as u64,
                });
                self.retained = Some(buf); // closed on delivery or on timeout
            }
            Op::TcpClose => {
                let Some(sock) = self.tcp.get_mut(req.sock as usize).and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.user_closed {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if let Some(r) = sock.recv.take() {
                    let _ = sys::handle_close(r.buf);
                    answer(r.reply_to, Reply::status(Status::BadSock));
                }
                if let Some((h, _)) = sock.accept.take() {
                    answer(h, Reply::status(Status::BadSock));
                }
                sock.user_closed = true;
                if sock.listening {
                    self.tcp[req.sock as usize] = None;
                    // Its backlog has no owner left: each connection nobody
                    // accepted is reset on the next poll and its slot freed,
                    // instead of lingering silently (incident 0013).
                    for child in self.tcp.iter_mut().flatten() {
                        if child.accepted_from == Some(req.sock) {
                            child.accepted_from = None;
                            child.user_closed = true;
                            child.conn.abort();
                        }
                    }
                } else {
                    sock.conn.close();
                }
                answer(reply_to, Reply::ok(0));
            }
            _ => self.refuse(reply_to, Refusal::BadRequest, now),
        }
    }

    fn on_dns(&mut self, data: &[u8]) {
        for slot in self.dns.iter_mut() {
            let Some(p) = slot.as_ref() else { continue };
            match dns::parse_response(p.id, data) {
                Some(dns::Answer::Address(ip, _)) => {
                    let p = slot.take().unwrap();
                    answer(p.reply_to, Reply::ok(0).with_addr(ip.0, 0));
                    return;
                }
                Some(dns::Answer::NoAddress(_)) => {
                    let p = slot.take().unwrap();
                    answer(p.reply_to, Reply::status(Status::NotFound));
                    return;
                }
                None => {}
            }
        }
    }

    // ---------------- timeouts ----------------

    fn expire(&mut self, now: u64) {
        for slot in self.pings.iter_mut() {
            if slot.as_ref().is_some_and(|p| now >= p.deadline) {
                let p = slot.take().unwrap();
                answer(p.reply_to, Reply::status(Status::Timeout));
            }
        }
        for slot in self.dns.iter_mut() {
            if slot.as_ref().is_some_and(|p| now >= p.deadline) {
                let p = slot.take().unwrap();
                answer(p.reply_to, Reply::status(Status::Timeout));
            }
        }
        for sock in self.sockets.iter_mut().flatten() {
            if sock.recv.as_ref().is_some_and(|r| now >= r.deadline) {
                let r = sock.recv.take().unwrap();
                let _ = sys::handle_close(r.buf);
                answer(r.reply_to, Reply::status(Status::Timeout));
            }
        }
        // A parked frame nobody answered ARP for is lost: its sender (TCP)
        // retransmits or times out, and the loss stays visible in `diag`.
        let mut dropped = false;
        for slot in self.deferred.iter_mut() {
            if slot.as_ref().is_some_and(|d| now >= d.deadline) {
                *slot = None;
                dropped = true;
            }
        }
        if dropped {
            self.note(Refusal::ArpNoReply, now);
        }
        // Refusals held back during a window are recorded when it ends, so
        // a burst that stopped still shows its size (incident 0012 audit).
        while let Some((kind, count)) = self.faults.drain_due(now) {
            if let Some(what) = Refusal::from_index(kind) {
                self.record(what, count);
            }
        }
    }

    // ---------------- IPC requests ----------------

    /// Serves one request. The client buffer handle is closed by the
    /// main loop unless it is left in `retained` (pending receive), so no
    /// error path leaves orphan handles.
    fn handle(&mut self, req: Request, reply_to: Handle, buf: Option<Handle>, now: u64) {
        let addr = Ipv4Addr(req.addr);
        match req.op {
            Op::TcpConnect
            | Op::TcpListen
            | Op::TcpAccept
            | Op::TcpSend
            | Op::TcpRecv
            | Op::TcpClose => self.tcp_handle(req, reply_to, buf, now),
            Op::Link => {
                let state = match self.link.link_up() {
                    None => aricode_proto::net::LINK_UNKNOWN,
                    Some(false) => aricode_proto::net::LINK_DOWN,
                    Some(true) => aricode_proto::net::LINK_UP,
                };
                answer(reply_to, Reply::ok(state));
            }
            Op::TcpStats => {
                let mut r = Reply::ok(0);
                r.data = self.tcp_stats().encode();
                answer(reply_to, r);
            }
            Op::Info => match self.config {
                Some(c) => {
                    let info = Info {
                        address: c.address.0,
                        mask: c.subnet_mask.0,
                        router: c.router.0,
                        dns: c.dns.0,
                        mac: self.link.mac().0,
                    };
                    let mut r = Reply::ok(0);
                    r.data = info.encode();
                    answer(reply_to, r);
                }
                None => answer(reply_to, Reply::status(Status::NoRoute)),
            },
            Op::Ping => {
                let Some(slot) = self.pings.iter().position(|p| p.is_none()) else {
                    return self.refuse(reply_to, Refusal::PingTableFull, now);
                };
                let echo = icmp::Echo {
                    reply: false,
                    identifier: PING_ID,
                    sequence: req.id,
                };
                let mut out = [0u8; 64];
                let Some(n) = echo.write(b"AriCodeOS ping", &mut out) else {
                    return self.refuse(reply_to, Refusal::BadRequest, now);
                };
                match self.send_ipv4(addr, ipv4::PROTO_ICMP, &out[..n], now) {
                    Ok(()) => {
                        self.pings[slot] = Some(PendingPing {
                            reply_to,
                            dst: addr,
                            seq: req.id,
                            sent_at: now,
                            deadline: now + req.timeout_ms.max(1) as u64,
                        });
                    }
                    Err(s) => answer(reply_to, Reply::status(s)),
                }
            }
            Op::UdpBind => {
                if req.port == 0 {
                    return self.refuse(reply_to, Refusal::BadRequest, now);
                }
                if self.sockets.iter().flatten().any(|s| s.port == req.port) {
                    return answer(reply_to, Reply::status(Status::InUse));
                }
                match self.sockets.iter().position(|s| s.is_none()) {
                    Some(i) => {
                        self.sockets[i] = Some(new_socket(req.port));
                        answer(reply_to, Reply::ok(i as u64));
                    }
                    None => self.refuse(reply_to, Refusal::UdpTableFull, now),
                }
            }
            Op::UdpClose => match self.sockets.get_mut(req.sock as usize) {
                Some(s) if s.is_some() => {
                    if let Some(r) = s.take().and_then(|s| s.recv) {
                        let _ = sys::handle_close(r.buf);
                        answer(r.reply_to, Reply::status(Status::BadSock));
                    }
                    answer(reply_to, Reply::ok(0));
                }
                _ => answer(reply_to, Reply::status(Status::BadSock)),
            },
            Op::UdpSend => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Refusal::BadHandles, now);
                };
                let Some(port) = self
                    .sockets
                    .get(req.sock as usize)
                    .and_then(|s| s.as_ref())
                    .map(|s| s.port)
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                let len = req.len.min(MAX_DATAGRAM) as usize;
                let mut data = [0u8; MAX_DATAGRAM as usize];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => data[..len].copy_from_slice(&b[..len]),
                    _ => return self.refuse(reply_to, Refusal::BadHandles, now),
                }
                match self.send_udp(port, addr, req.port, &data[..len], now) {
                    Ok(()) => answer(reply_to, Reply::ok(len as u64)),
                    Err(s) => answer(reply_to, Reply::status(s)),
                }
            }
            Op::UdpRecv => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Refusal::BadHandles, now);
                };
                let Some(sock) = self
                    .sockets
                    .get_mut(req.sock as usize)
                    .and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.recv.is_some() {
                    return self.refuse(reply_to, Refusal::WaitPending, now);
                }
                let pending = PendingRecv {
                    reply_to,
                    buf,
                    max_len: req.len.min(MAX_DATAGRAM),
                    deadline: now + req.timeout_ms.max(1) as u64,
                };
                if let Some((from, port, len)) = sock.queued.take() {
                    let data = sock.data;
                    deliver_recv(&mut self.buffers, pending, from, port, &data[..len]);
                } else {
                    sock.recv = Some(pending);
                    self.retained = Some(buf); // closed on delivery or on timeout
                }
            }
            Op::Dns => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Refusal::BadHandles, now);
                };
                let Some(slot) = self.dns.iter().position(|d| d.is_none()) else {
                    return self.refuse(reply_to, Refusal::DnsTableFull, now);
                };
                let (server, port) = if addr.is_unspecified() {
                    match self.config {
                        Some(c) if !c.dns.is_unspecified() => (c.dns, dns::PORT),
                        _ => return answer(reply_to, Reply::status(Status::NoRoute)),
                    }
                } else {
                    (addr, if req.port == 0 { dns::PORT } else { req.port })
                };
                // The whole name travels in the client buffer (up to 253 bytes).
                let len = req.len as usize;
                if len == 0 || len > dns::MAX_NAME {
                    return self.refuse(reply_to, Refusal::BadRequest, now);
                }
                let mut name = [0u8; dns::MAX_NAME];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => name[..len].copy_from_slice(&b[..len]),
                    _ => return self.refuse(reply_to, Refusal::BadHandles, now),
                }
                let Ok(name_str) = core::str::from_utf8(&name[..len]) else {
                    return self.refuse(reply_to, Refusal::BadRequest, now);
                };
                let id = self.dns_id;
                self.dns_id = self.dns_id.wrapping_add(1);
                let mut q = [0u8; 320];
                let Some(n) = dns::write_query(id, name_str, &mut q) else {
                    return self.refuse(reply_to, Refusal::BadRequest, now);
                };
                let src_port = self.dns_port;
                match self.send_udp(src_port, server, port, &q[..n], now) {
                    Ok(()) => {
                        self.dns[slot] = Some(PendingDns {
                            reply_to,
                            id,
                            deadline: now + req.timeout_ms.max(1) as u64,
                        });
                    }
                    Err(s) => answer(reply_to, Reply::status(s)),
                }
            }
        }
    }
}

/// Delivers a datagram to a pending receive and releases its handles.
fn deliver_recv(
    buffers: &mut ClientBuffers,
    pending: PendingRecv,
    from: Ipv4Addr,
    port: u16,
    data: &[u8],
) {
    let len = data.len().min(pending.max_len as usize);
    let reply = match buffers.map(pending.buf) {
        Some(b) if b.len() >= len => {
            b[..len].copy_from_slice(&data[..len]);
            Reply::ok(len as u64).with_addr(from.0, port)
        }
        _ => Reply::status(Status::BadHandles),
    };
    let _ = sys::handle_close(pending.buf);
    answer(pending.reply_to, reply);
}

/// Replies through the transferred endpoint and closes it.
fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

/// Announces the MAC and serves requests forever: frame reception,
/// up to eight IPC requests per round, timeouts and waiting for the IRQ.
pub fn run<L: Link>(link: L, clock: Clock, name: &'static str) -> ! {
    let mut hex = [0u8; 16];
    sys::print(name.as_bytes());
    sys::print(b": device ready, MAC ");
    for (i, b) in link.mac().0.iter().enumerate() {
        if i > 0 {
            sys::print(b":");
        }
        sys::print(aricode_rt::fmt_hex(*b as u64, 2, &mut hex));
    }
    sys::print(b"\n");
    let mut stack = Stack::new(link, clock, name);
    let mut frame = [0u8; MAX_FRAME];
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 2];
    say(name, b"serving requests");
    loop {
        let now = stack.clock.now();
        stack.dhcp_step(now);
        while let Some(n) = stack.link.recv_frame(&mut frame) {
            stack.on_frame(&frame[..n], now);
        }
        for _ in 0..8 {
            let (n, nh) = match sys::ipc_receive(SERVICE, &mut msg, &mut handles, 0) {
                Ok(r) => r,
                // The service endpoint was closed or revoked (ADR 0030): nobody
                // can send here again, so polling it would spin.
                Err(e) if e.endpoint_gone() => {
                    say(name, b"service endpoint closed; exiting");
                    sys::process_exit(9);
                }
                Err(_) => break,
            };
            if nh == 0 {
                continue;
            }
            let reply_to = handles[0];
            let buf = (nh >= 2).then_some(handles[1]);
            match Request::decode(&msg[..n]) {
                Some(req) => stack.handle(req, reply_to, buf, now),
                None => stack.refuse(reply_to, Refusal::BadRequest, now),
            }
            let retained = stack.retained.take();
            if let Some(b) = buf {
                if retained != Some(b) {
                    let _ = sys::handle_close(b);
                }
            }
        }
        stack.expire(now);
        stack.tcp_expire(now);
        stack.tcp_poll(now);
        let _ = sys::irq_wait(IRQ, 10);
    }
}
