// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Network service core (Phase 22, ADR 0024): minimal stack (ARP, IPv4,
//! ICMP, UDP, DHCP, DNS, TCP) and `NetServiceProtocol v1` server,
//! independent of the NIC. Each driver supplies a [`Link`] (MAC, send
//! and receive frames) and calls [`run`]; virtio-net and rtl-net share it.
//! Single thread: alternates between reception, IPC requests and timeouts.
//!
//! Besides its main endpoint (init) the service serves *sessions* (ADR
//! 0037): endpoints it creates on request of the main endpoint, one per
//! client process, of which it keeps only the receive side. Every socket
//! and every wait belongs to the endpoint whose request created it, and
//! when the kernel reports that nobody holds a session's send side any more
//! (its client died) the session is retired and its sockets are closed
//! (incident 0016).

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use aricode_net::owners::{Owned, Owner};
use aricode_net::rx;
use aricode_net::tcp::{self, Connection};
use aricode_net::{arp, dhcp, dns, eth, icmp, ipv4, isn, udp, Ipv4Addr, MacAddr};
use aricode_proto::net::{
    Info, Op, OwnerSockets, Reply, Request, Status, TcpStats, MAX_DATAGRAM, MAX_LABEL,
};
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
/// Client sessions served at once (ADR 0037): every one of them and the
/// main endpoint fit in a single `ipc_receive_any`. The clients that can
/// hold one at the same time are the console shell, `MAX_SESSIONS = 4` SSH
/// shells, sshd and the broker's `MAX_RUNNING = 4` programs: ten, so the
/// table is never full for a legitimate load.
const MAX_SESSIONS: usize = abi::IPC_MAX_ANY - 1;
/// How often the service asks the kernel whether a session's client is
/// still there, besides doing so whenever a table is full.
const SWEEP_MS: u64 = 1_000;
/// The main endpoint as an owner: init, which never exits.
const MAIN: Owner = 0;
/// IPC requests served per round, between two rounds of reception, so a
/// client in a loop cannot keep frames from being received. Its twin is
/// `rx::FRAMES_PER_ROUND` (`libs/net`, incident 0013): a round receives at
/// most 32 frames, then serves these requests and the timers, and follows
/// at once (no interrupt wait) when frames were left in the ring, which is
/// recorded as `net rx budget hit`. Together they bound what a client waits
/// under a flood to one round, instead of the flood's duration.
const IPC_REQUESTS_PER_ROUND: usize = 8;
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

/// What the service records in the kernel event ring (ADR 0028): why it
/// refused a request or lost a frame (incident 0012), and what it cleaned
/// up after a dead client (incident 0016). Each kind is recorded at most
/// once per second (`aricode_faults`): the name is `<kind> x<count>`
/// (faults of that kind in that second) and the code is the `net::Status`
/// number the client got, 0 when nobody was refused (`diag`: `fault
/// reported by process N: net tcp table full x3 (code 5)`). `BadSock`,
/// `InUse`, `NoRoute`, `NotFound`, `Timeout` and `Reset` are not refusals:
/// they describe the client's socket or the network's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
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
    /// `Attach` refused: every session slot taken, even after looking for
    /// dead clients (ADR 0037).
    SessionsFull = 11,
    /// Sockets and waits closed because the client that held them died
    /// without closing (incident 0016): the count is the number of sockets.
    /// Nobody was refused (code 0).
    Reclaimed = 12,
    /// A round of reception spent `rx::FRAMES_PER_ROUND` frames with more
    /// in the ring (incident 0013): the clients' requests took their turn
    /// and reception went on at once. Nobody was refused (code 0); the
    /// count is the number of such rounds in that second.
    RxBudget = 13,
    /// `Attach` refused because the kernel could not create the session's
    /// endpoint pair (out of handles or memory), not because the table is
    /// full.
    SessionCreate = 14,
}

impl Fault {
    const COUNT: usize = 15;
    const ALL: [Fault; Self::COUNT] = [
        Fault::TcpTableFull,
        Fault::ArpDeferFull,
        Fault::TxBusy,
        Fault::ArpNoReply,
        Fault::PingTableFull,
        Fault::DnsTableFull,
        Fault::UdpTableFull,
        Fault::WaitPending,
        Fault::BadRequest,
        Fault::BadHandles,
        Fault::SynDropped,
        Fault::SessionsFull,
        Fault::Reclaimed,
        Fault::RxBudget,
        Fault::SessionCreate,
    ];

    fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    /// Kind name; with `" x999"` appended it must fit an event name.
    const fn name(self) -> &'static str {
        match self {
            Fault::TcpTableFull => "net tcp table full",
            Fault::ArpDeferFull => "net arp defer full",
            Fault::TxBusy => "net tx busy",
            Fault::ArpNoReply => "net arp no reply",
            Fault::PingTableFull => "net ping table full",
            Fault::DnsTableFull => "net dns table full",
            Fault::UdpTableFull => "net udp table full",
            Fault::WaitPending => "net wait pending",
            Fault::BadRequest => "net bad request",
            Fault::BadHandles => "net bad handles",
            Fault::SynDropped => "net syn dropped",
            Fault::SessionsFull => "net sessions full",
            // "sockets" would not leave room for the count in the kernel's
            // 24-byte event name.
            Fault::Reclaimed => "net socks reclaimed",
            Fault::RxBudget => "net rx budget hit",
            Fault::SessionCreate => "net session create",
        }
    }

    /// Status the client receives (its number is the event code); a lost
    /// frame carries the status its sender would have got, a clean-up
    /// after a dead client `Ok` (nobody was refused).
    fn status(self) -> Status {
        match self {
            Fault::BadRequest => Status::Invalid,
            Fault::BadHandles => Status::BadHandles,
            Fault::Reclaimed | Fault::RxBudget => Status::Ok,
            _ => Status::Busy,
        }
    }
}

// Every kind name leaves room for " x999" in a kernel event name.
const _: () = {
    assert!(aricode_faults::LABEL_LEN == abi::sysinfo::EVENT_NAME_LEN);
    let mut i = 0;
    while i < Fault::COUNT {
        assert!(Fault::ALL[i].name().len() + 5 <= aricode_faults::LABEL_LEN);
        assert!(Fault::ALL[i] as usize == i);
        i += 1;
    }
};

/// Link to the NIC: complete Ethernet frames, no headers of its own.
pub trait Link {
    fn mac(&self) -> MacAddr;
    /// Transmits a frame; `false` if there is no free slot (the caller loses it).
    fn send_frame(&mut self, frame: &[u8]) -> bool;
    /// Next received frame copied into `out`, or `None` if there is none.
    /// `Some(0)` is a receive slot consumed without a usable frame (an
    /// errored one): it counts toward the round's budget and is ignored.
    fn recv_frame(&mut self, out: &mut [u8; MAX_FRAME]) -> Option<usize>;
    /// Whether the ring holds a frame `recv_frame` would return, without
    /// taking it: what a round that spent its budget leaves behind.
    fn rx_pending(&self) -> bool;
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

impl PendingRecv {
    /// Its client is gone: nothing to answer, only handles to release.
    fn drop_handles(self) {
        let _ = sys::handle_close(self.buf);
        let _ = sys::handle_close(self.reply_to);
    }
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

/// A client's session (ADR 0037): the receive side of its endpoint and the
/// label init gave it (the process name).
#[derive(Clone, Copy)]
struct Session {
    ep: Handle,
    label: [u8; MAX_LABEL],
    len: u8,
}

impl Session {
    fn label_str(&self) -> &str {
        core::str::from_utf8(&self.label[..self.len as usize]).unwrap_or("")
    }
}

/// A request a sweep took from a session while asking the kernel whether
/// its client was alive; served before the next wait.
struct Pending {
    owner: Owner,
    n: usize,
    nh: usize,
    msg: [u8; Request::LEN],
    handles: [Handle; abi::IPC_MAX_HANDLES],
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
    /// Every table is per owner (ADR 0037): an index only answers the
    /// session whose request created the entry.
    pings: Owned<PendingPing, MAX_PINGS>,
    sockets: Owned<Socket, MAX_SOCKETS>,
    dns: Owned<PendingDns, MAX_DNS>,
    /// On the heap: each socket carries 8 KiB of buffers and the stack is 64 KiB.
    tcp: Owned<Box<TcpSock>, MAX_TCP>,
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
    /// Faults recorded at most once per second per kind (incident 0012).
    faults: aricode_faults::Meter<{ Fault::COUNT }>,
    /// Client sessions by slot; owner number = slot + 1 (`MAIN` is 0).
    sessions: [Option<Session>; MAX_SESSIONS],
    pending: [Option<Pending>; MAX_SESSIONS],
    /// Sockets closed on behalf of dead clients since the service started.
    reclaimed: u64,
    last_sweep: u64,
    /// An unexpected probe error was already said (once is enough to be
    /// seen; the session is kept and probed again).
    probe_error_said: bool,
    /// Bumped whenever a session is granted or retired, so the request
    /// round of `run` knows its endpoint list is stale.
    generation: u32,
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
            pings: Owned::new(),
            sockets: Owned::new(),
            dns: Owned::new(),
            tcp: Owned::new(),
            tcp_port_count: 0,
            tcp_syn_dropped: 0,
            deferred: [const { None }; 4],
            buffers: ClientBuffers {
                entries: [(0, 0, 0); 8],
                count: 0,
            },
            dns_port: 49153,
            retained: None,
            faults: aricode_faults::Meter::new(),
            sessions: [None; MAX_SESSIONS],
            pending: [const { None }; MAX_SESSIONS],
            reclaimed: 0,
            last_sweep: 0,
            probe_error_said: false,
            generation: 0,
        }
    }

    /// Accounts a refusal or a lost frame: recorded at once if the kind was
    /// quiet for a second, otherwise counted and recorded when the second
    /// ends (`expire`).
    fn note(&mut self, what: Fault, now: u64) {
        self.note_many(what, 1, now);
    }

    /// Accounts `count` faults of `kind` that happened together (the
    /// sockets of one dead client) with the same rule as `note`.
    fn note_many(&mut self, what: Fault, count: u32, now: u64) {
        let due = self.faults.report_many(what as usize, now, count);
        if due > 0 {
            self.record(what, due);
        }
    }

    /// A fault event in the kernel ring (`<kind> x<count>`, code = status
    /// number) and the same line on the console.
    fn record(&self, what: Fault, count: u32) {
        let mut label = [0u8; aricode_faults::LABEL_LEN];
        let name = aricode_faults::label(what.name(), count, &mut label);
        let _ = sys::event_record(what.status() as u64, name);
        sys::print(self.name.as_bytes());
        sys::print(b": fault ");
        sys::print(name.as_bytes());
        sys::print(b"\n");
    }

    /// Refuses a request: records it and answers with the matching status.
    fn refuse(&mut self, reply_to: Handle, what: Fault, now: u64) {
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

    // ---------------- sessions (ADR 0037) ----------------

    /// The endpoints served by one `ipc_receive_any` (the main one first,
    /// then every session) and the owner each one stands for; returns how
    /// many were written.
    fn endpoints(
        &self,
        eps: &mut [Handle; abi::IPC_MAX_ANY],
        owners: &mut [Owner; abi::IPC_MAX_ANY],
    ) -> usize {
        eps[0] = SERVICE;
        owners[0] = MAIN;
        let mut count = 1;
        for (i, s) in self.sessions.iter().enumerate() {
            if let Some(s) = s {
                eps[count] = s.ep;
                owners[count] = i + 1;
                count += 1;
            }
        }
        count
    }

    fn session(&self, owner: Owner) -> Option<Session> {
        owner
            .checked_sub(1)
            .and_then(|i| self.sessions.get(i).copied().flatten())
    }

    /// Creates a session for the client `label`: a new endpoint pair of
    /// which the service keeps the receive side; the send side goes to the
    /// client, and when its last holder dies the session is retired. One
    /// per launch, so the sockets of one process never mix with another's.
    fn grant(&mut self, label: &str, now: u64) -> Result<Handle, Fault> {
        let free = |sessions: &[Option<Session>]| sessions.iter().position(Option::is_none);
        let slot = match free(&self.sessions) {
            Some(slot) => slot,
            None => {
                // Full: the sessions of dead clients go first.
                self.sweep(now);
                free(&self.sessions).ok_or(Fault::SessionsFull)?
            }
        };
        let (receive, send) = sys::endpoint_create_pair().map_err(|_| Fault::SessionCreate)?;
        let mut l = [0u8; MAX_LABEL];
        l[..label.len()].copy_from_slice(label.as_bytes());
        self.sessions[slot] = Some(Session {
            ep: receive,
            label: l,
            len: label.len() as u8,
        });
        self.generation = self.generation.wrapping_add(1);
        Ok(send)
    }

    /// Asks the kernel, session by session, whether the client is still
    /// there: a receive that would not wait reports `PeerClosed` once no
    /// send handle is left and the queue is empty (ADR 0030). A request
    /// found waiting is kept for the main loop; a session with one kept is
    /// alive and not asked.
    fn sweep(&mut self, now: u64) {
        self.last_sweep = now;
        for i in 0..MAX_SESSIONS {
            self.probe(i, now);
        }
    }

    /// Asks about the session behind `owner`, if it is one (the main
    /// endpoint is never retired). `true` while it lives.
    fn probe_owner(&mut self, owner: Owner, now: u64) -> bool {
        match owner.checked_sub(1) {
            Some(i) if i < MAX_SESSIONS => self.probe(i, now),
            _ => true,
        }
    }

    /// Whether `owner` still exists after a sweep or a probe made while
    /// serving its request: a client that died right after sending is
    /// retired by that very sweep, and its request must not create a
    /// socket that the next client given its owner number would inherit.
    /// `false` closes the reply endpoint (nobody reads it).
    fn still_there(&self, owner: Owner, reply_to: Handle) -> bool {
        if owner == MAIN || self.session(owner).is_some() {
            return true;
        }
        let _ = sys::handle_close(reply_to);
        false
    }

    /// Asks the kernel whether the client of session `i` is still there
    /// and retires the session if not. `true` while the session lives.
    fn probe(&mut self, i: usize, now: u64) -> bool {
        let Some(s) = self.sessions[i] else {
            return false;
        };
        if self.pending[i].is_some() {
            return true;
        }
        let mut p = Pending {
            owner: i + 1,
            n: 0,
            nh: 0,
            msg: [0; Request::LEN],
            handles: [Handle(0); abi::IPC_MAX_HANDLES],
        };
        match sys::ipc_receive(s.ep, &mut p.msg, &mut p.handles, 0) {
            Ok((n, nh)) => {
                p.n = n;
                p.nh = nh;
                self.pending[i] = Some(p);
                true
            }
            Err(e) if e.endpoint_gone() => {
                self.retire(i, now);
                false
            }
            // `Timeout` is the live answer. Anything else (a bad handle, an
            // invalid buffer) is a defect, not a dead client: said once,
            // never silent (ADR 0028), and the session stays.
            Err(sys::Error::Timeout) => true,
            Err(e) => {
                if !self.probe_error_said {
                    self.probe_error_said = true;
                    sys::print(self.name.as_bytes());
                    aricode_rt::print_error(b": session probe", e);
                }
                true
            }
        }
    }

    /// The client of session `i` is gone: closes its endpoint and every
    /// socket and wait it held (a connection gets a reset, a listener goes
    /// with its backlog, a wait only releases its handles), and accounts
    /// what it left open as a fault of that client (incident 0016). A
    /// connection the client had already closed is nobody's now and
    /// finishes by itself (TIME-WAIT, the last ACK).
    fn retire(&mut self, i: usize, now: u64) {
        let owner = i + 1;
        let Some(s) = self.sessions[i].take() else {
            return;
        };
        let _ = sys::handle_close(s.ep);
        self.generation = self.generation.wrapping_add(1);
        let mut n = 0;
        n += self.pings.reclaim(owner, |_, p| {
            let _ = sys::handle_close(p.reply_to);
        });
        n += self.dns.reclaim(owner, |_, p| {
            let _ = sys::handle_close(p.reply_to);
        });
        n += self.sockets.reclaim(owner, |_, s| {
            if let Some(r) = s.recv {
                r.drop_handles();
            }
        });
        for slot in 0..MAX_TCP {
            if self.tcp.owner_of(slot) != Some(owner) {
                continue;
            }
            if self.tcp.at(slot).is_some_and(|s| s.user_closed) {
                self.tcp.disown(slot);
                continue;
            }
            if let Some((_, sock)) = self.tcp.take(slot) {
                self.reset_and_drop(sock, now);
                n += 1;
            }
        }
        if n == 0 {
            return;
        }
        self.reclaimed += n as u64;
        let mut buf = [0u8; 20];
        sys::print(self.name.as_bytes());
        sys::print(b": client gone ('");
        sys::print(s.label_str().as_bytes());
        sys::print(b"'), ");
        sys::print(aricode_rt::fmt_u64(n as u64, &mut buf));
        sys::print(b" socket(s) left open: reclaimed\n");
        self.note_many(Fault::Reclaimed, n as u32, now);
    }

    /// Ends a TCP socket whose client is gone: its waits release their
    /// handles (nobody will read the replies), a live connection is aborted
    /// and its reset sent at once so the slot is free for the request that
    /// found the table full. A listener holds no connection of its own;
    /// its backlog is owned by the same client and goes the same way.
    fn reset_and_drop(&mut self, mut sock: Box<TcpSock>, now: u64) {
        if let Some(r) = sock.recv.take() {
            r.drop_handles();
        }
        for h in [sock.accept.take(), sock.connect.take()]
            .into_iter()
            .flatten()
        {
            let _ = sys::handle_close(h.0);
        }
        if sock.listening || sock.conn.is_closed() {
            return;
        }
        sock.conn.abort();
        let mut seg = [0u8; tcp::MAX_SEGMENT];
        if let Some(n) = sock.conn.poll(now, &mut seg) {
            let (remote, _) = sock.conn.remote();
            let _ = self.send_ipv4(remote, ipv4::PROTO_TCP, &seg[..n], now);
        }
    }

    /// `Op::Sockets`: usage of owner slot `slot` and its label.
    fn sockets_usage(&self, slot: usize) -> Reply {
        if slot > MAX_SESSIONS {
            return Reply::status(Status::NotFound);
        }
        let usage = OwnerSockets {
            owned: (self.pings.owned_by(slot)
                + self.dns.owned_by(slot)
                + self.sockets.owned_by(slot)
                + self.tcp.owned_by(slot)) as u16,
            used: (self.pings.used() + self.dns.used() + self.sockets.used() + self.tcp.used())
                as u16,
            reclaimed: self.reclaimed.min(u16::MAX as u64) as u16,
            sessions: self.sessions.iter().flatten().count() as u16,
        };
        let label = if slot == MAIN {
            "main"
        } else {
            self.sessions[slot - 1]
                .as_ref()
                .map_or("", Session::label_str)
        };
        Reply::ok(usage.encode()).with_label(label)
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

    /// Builds in `frame` the IPv4 packet to `dst` with a `payload` of
    /// protocol `proto`, leaving the Ethernet header for the caller:
    /// returns the frame length and the next hop. An oversized payload is
    /// recorded here as `BadRequest`.
    fn ipv4_frame(
        &mut self,
        dst: Ipv4Addr,
        proto: u8,
        payload: &[u8],
        frame: &mut [u8; MAX_FRAME],
        now: u64,
    ) -> Result<(usize, Ipv4Addr), Status> {
        let src = self.my_ip();
        let next_hop = self.next_hop(dst).ok_or(Status::NoRoute)?;
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        let ip_hdr = ipv4::Header::new(src, dst, proto, id, payload.len() as u16);
        let mut n = eth::HEADER_LEN;
        let header = ip_hdr.write(&mut frame[n..]);
        let fits = header.is_some_and(|h| frame.len() >= n + h + payload.len());
        let Some(h) = header.filter(|_| fits) else {
            self.note(Fault::BadRequest, now);
            return Err(Status::Invalid);
        };
        n += h;
        frame[n..n + payload.len()].copy_from_slice(payload);
        n += payload.len();
        Ok((n, next_hop))
    }

    /// Writes the Ethernet header for `mac` and transmits `frame[..n]`; a
    /// full transmit ring loses the frame and is recorded as `TxBusy`.
    fn transmit(
        &mut self,
        mac: MacAddr,
        frame: &mut [u8; MAX_FRAME],
        n: usize,
        now: u64,
    ) -> Result<(), Status> {
        eth::Header {
            dst: mac,
            src: self.link.mac(),
            ethertype: eth::ETHERTYPE_IPV4,
        }
        .write(frame);
        if self.link.send_frame(&frame[..n]) {
            Ok(())
        } else {
            self.note(Fault::TxBusy, now);
            Err(Status::Busy)
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
        let mut frame = [0u8; MAX_FRAME];
        let (n, next_hop) = self.ipv4_frame(dst, proto, payload, &mut frame, now)?;
        let dst_mac = if dst == Ipv4Addr::BROADCAST {
            Some(MacAddr::BROADCAST)
        } else {
            self.arp.lookup(next_hop)
        };
        match dst_mac {
            Some(mac) => self.transmit(mac, &mut frame, n, now),
            None => {
                let Some(slot) = self.deferred.iter_mut().find(|d| d.is_none()) else {
                    self.note(Fault::ArpDeferFull, now);
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
            self.note(Fault::BadRequest, now);
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
            self.note(Fault::TxBusy, now);
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
        let waiting = self
            .pings
            .iter()
            .find(|(_, _, p)| p.seq == echo.sequence && p.dst == ih.src)
            .map(|(i, _, _)| i);
        if let Some((_, p)) = waiting.and_then(|i| self.pings.take(i)) {
            answer(
                p.reply_to,
                Reply::ok(now.saturating_sub(p.sent_at)).with_addr(ih.src.0, 0),
            );
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
        let Some((_, _, sock)) = self
            .sockets
            .iter_mut()
            .find(|(_, _, s)| s.port == uh.dst_port)
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
        let existing = self
            .tcp
            .iter()
            .find(|(_, _, s)| {
                !s.listening
                    && s.conn.local_port() == h.dst_port
                    && s.conn.remote() == (ih.src, h.src_port)
            })
            .map(|(i, _, _)| i);
        if let Some(sock) = existing.and_then(|i| self.tcp.at_mut(i)) {
            sock.conn.on_segment(&h, payload, now);
            return;
        }
        // A segment with RST for a connection that does not exist is never
        // answered (RFC 793 section 3.4, "except another reset") and never
        // opens one: a SYN|RST would leave a child in LISTEN that nothing
        // completes or reaps (incident 0013 audit).
        if h.flags & tcp::FLAG_RST != 0 {
            return;
        }
        // SYN to a listening port: new passive connection.
        if h.flags & tcp::FLAG_SYN == 0 || h.flags & tcp::FLAG_ACK != 0 {
            return;
        }
        // RFC 1122 section 3.2.1.3: a SYN from a source that cannot be a
        // host (unspecified, multicast, broadcast) is dropped, not reset.
        let (local, mask) = self.config.map_or((ih.dst, Ipv4Addr::BROADCAST), |c| {
            (c.address, c.subnet_mask)
        });
        if ih.src.is_invalid_source(local, mask) {
            return;
        }
        let Some((listener, owner, _)) = self
            .tcp
            .iter()
            .find(|(_, _, s)| s.listening && s.conn.local_port() == h.dst_port)
        else {
            return;
        };
        // No free slot, or the listener already holds `TCP_BACKLOG`
        // connections nobody accepted (half-open included): the SYN is
        // refused with a reset so the peer stops retransmitting, and the
        // refusal is recorded like every other (incident 0013, ADR 0035).
        let backlog = self
            .tcp
            .iter()
            .filter(|(_, _, s)| s.accepted_from == Some(listener as u16))
            .count();
        if self.tcp.is_full() || backlog >= TCP_BACKLOG {
            self.tcp_syn_dropped = self.tcp_syn_dropped.saturating_add(1);
            self.note(Fault::SynDropped, now);
            self.send_tcp_reset(ih, &h, payload.len(), now);
            return;
        }
        let mut conn = Connection::new(ih.dst, h.dst_port, ih.src, h.src_port);
        conn.listen(self.initial_seq(ih.dst, h.dst_port, ih.src, h.src_port, now));
        conn.on_segment(&h, payload, now);
        let mut sock = Box::new(TcpSock::new(conn));
        sock.accepted_from = Some(listener as u16);
        // The child belongs to the listener's client, who will accept it.
        self.tcp.insert(owner, sock);
    }

    /// Reset answering a segment that belongs to no connection (RFC 793
    /// section 3.4, `tcp::reset_for`). Stateless and best effort: it goes
    /// out only when the next hop's MAC is already known. A SYN from a
    /// spoofed on-subnet source would otherwise cost an ARP request and one
    /// of the four parking slots per SYN (`net arp defer full` for everyone
    /// else); a real peer whose MAC is unknown retransmits its SYN and the
    /// ARP exchange of the first answer resolves it meanwhile. The refusal
    /// itself is recorded (`net syn dropped`) whether the reset went out or
    /// not; a full transmit ring is recorded by `transmit`.
    fn send_tcp_reset(&mut self, ih: ipv4::Header, h: &tcp::Header, payload_len: usize, now: u64) {
        let mut seg = [0u8; tcp::HEADER_LEN];
        let Some(len) = tcp::reset_for(h, payload_len).write(ih.dst, ih.src, None, &[], &mut seg)
        else {
            return;
        };
        let mut frame = [0u8; MAX_FRAME];
        let Ok((n, next_hop)) =
            self.ipv4_frame(ih.src, ipv4::PROTO_TCP, &seg[..len], &mut frame, now)
        else {
            return;
        };
        if let Some(mac) = self.arp.lookup(next_hop) {
            let _ = self.transmit(mac, &mut frame, n, now);
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
        for i in 0..MAX_TCP {
            let Some(s) = self.tcp.at(i) else {
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
                    // FIN sent or received, finished and not yet reaped, or a
                    // child of a listener in a state none should be in (the
                    // hand-out loop reaps it on this same poll).
                    tcp::State::FinWait1
                    | tcp::State::FinWait2
                    | tcp::State::Closing
                    | tcp::State::LastAck
                    | tcp::State::Closed
                    | tcp::State::Listen => &mut t.closing,
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
            if self.tcp.at(i).is_none_or(|s| s.listening) {
                continue;
            }
            // Take the socket out of its slot while emitting: `send_ipv4` needs
            // the rest of the stack.
            let Some((owner, mut sock)) = self.tcp.take(i) else {
                continue;
            };
            let (remote, _) = sock.conn.remote();
            while let Some(n) = sock.conn.poll(now, &mut seg) {
                let _ = self.send_ipv4(remote, ipv4::PROTO_TCP, &seg[..n], now);
                if sock.conn.is_closed() {
                    break;
                }
            }
            self.tcp.set(i, owner, sock);
            let Some(sock) = self.tcp.at_mut(i) else {
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
                self.tcp.take(i);
            }
        }
        // Hand accepted connections to pending `TcpAccept` calls.
        for i in 0..MAX_TCP {
            let Some(from) = self.tcp.at(i).and_then(|s| s.accepted_from) else {
                continue;
            };
            if !self.tcp.at(i).unwrap().conn.is_established() {
                // Half-open children wait for their ACK or their budget.
                // Anything else nobody accepted (finished, or a state no
                // child of a listener can legitimately be in) is reaped,
                // so the backlog cap counts only live connections (incident
                // 0013 audit: defence in depth behind the RST check).
                if self.tcp.at(i).unwrap().conn.state() != tcp::State::SynReceived {
                    self.tcp.take(i);
                }
                continue;
            }
            let Some(listener) = self.tcp.at_mut(from as usize) else {
                self.tcp.take(i);
                continue;
            };
            if let Some((reply_to, _)) = listener.accept.take() {
                let sock = self.tcp.at_mut(i).unwrap();
                sock.accepted_from = None;
                let (addr, port) = sock.conn.remote();
                answer(reply_to, Reply::ok(i as u64).with_addr(addr.0, port));
            }
        }
    }

    fn tcp_expire(&mut self, now: u64) {
        for (_, _, sock) in self.tcp.iter_mut() {
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

    /// Makes room in the TCP table by looking for dead clients when it is
    /// full (ADR 0037: a full table is swept before it refuses). `false`
    /// when the request is over: `owner` itself was found dead (its reply
    /// endpoint is closed) or the table is still full (refused, recorded).
    fn tcp_room(&mut self, owner: Owner, reply_to: Handle, now: u64) -> bool {
        if !self.tcp.is_full() {
            return true;
        }
        self.sweep(now);
        if !self.still_there(owner, reply_to) {
            return false;
        }
        if self.tcp.is_full() {
            self.refuse(reply_to, Fault::TcpTableFull, now);
            return false;
        }
        true
    }

    fn tcp_handle(
        &mut self,
        req: Request,
        owner: Owner,
        reply_to: Handle,
        buf: Option<Handle>,
        now: u64,
    ) {
        let addr = Ipv4Addr(req.addr);
        let sock = req.sock as usize;
        match req.op {
            Op::TcpConnect => {
                if self.config.is_none() {
                    return answer(reply_to, Reply::status(Status::NoRoute));
                }
                if !self.tcp_room(owner, reply_to, now) {
                    return;
                }
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
                    |p| tcp.iter().any(|(_, _, s)| s.conn.local_port() == p),
                ) else {
                    return self.refuse(reply_to, Fault::TcpTableFull, now);
                };
                let mut conn = Connection::new(local, port, addr, req.port);
                conn.connect(self.initial_seq(local, port, addr, req.port, now), now);
                let mut sock = Box::new(TcpSock::new(conn));
                sock.connect = Some((reply_to, now + req.timeout_ms.max(1) as u64));
                self.tcp.insert(owner, sock);
            }
            Op::TcpListen => {
                if req.port == 0 {
                    return self.refuse(reply_to, Fault::BadRequest, now);
                }
                // A port another client listens on may be a dead client's
                // (sshd relaunched before the sweep noticed its previous
                // generation): ask about that client before refusing.
                let held_by = |tcp: &Owned<Box<TcpSock>, MAX_TCP>| {
                    tcp.iter()
                        .find(|(_, _, s)| s.listening && s.conn.local_port() == req.port)
                        .map(|(_, o, _)| o)
                };
                if let Some(o) = held_by(&self.tcp) {
                    self.probe_owner(o, now);
                    if !self.still_there(owner, reply_to) {
                        return;
                    }
                    if held_by(&self.tcp).is_some() {
                        return answer(reply_to, Reply::status(Status::InUse));
                    }
                }
                if !self.tcp_room(owner, reply_to, now) {
                    return;
                }
                let mut conn = Connection::new(self.my_ip(), req.port, Ipv4Addr::UNSPECIFIED, 0);
                // A listener sends nothing itself: every SYN to it gets a
                // connection of its own in `on_tcp`, with its own ISN.
                conn.listen(0);
                let mut sock = Box::new(TcpSock::new(conn));
                sock.listening = true;
                let slot = self.tcp.insert(owner, sock).unwrap_or(MAX_TCP);
                answer(reply_to, Reply::ok(slot as u64));
            }
            Op::TcpAccept => {
                let Some(listener) = self.tcp.get_mut(sock, owner) else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if !listener.listening {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if listener.accept.is_some() {
                    return self.refuse(reply_to, Fault::WaitPending, now);
                }
                listener.accept = Some((reply_to, now + req.timeout_ms.max(1) as u64));
            }
            Op::TcpSend => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Fault::BadHandles, now);
                };
                let len = req.len.min(MAX_DATAGRAM) as usize;
                let mut data = [0u8; MAX_DATAGRAM as usize];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => data[..len].copy_from_slice(&b[..len]),
                    _ => return self.refuse(reply_to, Fault::BadHandles, now),
                }
                let Some(sock) = self.tcp.get_mut(sock, owner) else {
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
                    return self.refuse(reply_to, Fault::BadHandles, now);
                };
                let Some(sock) = self.tcp.get_mut(sock, owner) else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.listening || sock.user_closed {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if sock.recv.is_some() {
                    return self.refuse(reply_to, Fault::WaitPending, now);
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
                let Some(s) = self.tcp.get_mut(sock, owner) else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if s.user_closed {
                    return answer(reply_to, Reply::status(Status::BadSock));
                }
                if let Some(r) = s.recv.take() {
                    let _ = sys::handle_close(r.buf);
                    answer(r.reply_to, Reply::status(Status::BadSock));
                }
                if let Some((h, _)) = s.accept.take() {
                    answer(h, Reply::status(Status::BadSock));
                }
                s.user_closed = true;
                if s.listening {
                    self.tcp.take(sock);
                    // Its backlog has no owner left: each connection nobody
                    // accepted is reset on the next poll and its slot freed,
                    // instead of lingering silently (incident 0013).
                    for (_, _, child) in self.tcp.iter_mut() {
                        if child.accepted_from == Some(req.sock) {
                            child.accepted_from = None;
                            child.user_closed = true;
                            child.conn.abort();
                        }
                    }
                } else {
                    s.conn.close();
                }
                answer(reply_to, Reply::ok(0));
            }
            _ => self.refuse(reply_to, Fault::BadRequest, now),
        }
    }

    fn on_dns(&mut self, data: &[u8]) {
        for i in 0..MAX_DNS {
            let Some(p) = self.dns.at(i) else { continue };
            let reply = match dns::parse_response(p.id, data) {
                Some(dns::Answer::Address(ip, _)) => Reply::ok(0).with_addr(ip.0, 0),
                Some(dns::Answer::NoAddress(_)) => Reply::status(Status::NotFound),
                None => continue,
            };
            if let Some((_, p)) = self.dns.take(i) {
                answer(p.reply_to, reply);
            }
            return;
        }
    }

    // ---------------- timeouts ----------------

    fn expire(&mut self, now: u64) {
        for i in 0..MAX_PINGS {
            if self.pings.at(i).is_some_and(|p| now >= p.deadline) {
                if let Some((_, p)) = self.pings.take(i) {
                    answer(p.reply_to, Reply::status(Status::Timeout));
                }
            }
        }
        for i in 0..MAX_DNS {
            if self.dns.at(i).is_some_and(|p| now >= p.deadline) {
                if let Some((_, p)) = self.dns.take(i) {
                    answer(p.reply_to, Reply::status(Status::Timeout));
                }
            }
        }
        for (_, _, sock) in self.sockets.iter_mut() {
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
            self.note(Fault::ArpNoReply, now);
        }
        // Faults held back during a window are recorded when it ends, so
        // a burst that stopped still shows its size (incident 0012 audit).
        while let Some((kind, count)) = self.faults.drain_due(now) {
            if let Some(what) = Fault::from_index(kind) {
                self.record(what, count);
            }
        }
    }

    // ---------------- IPC requests ----------------

    /// Serves one message that arrived on `owner`'s endpoint: the reply
    /// endpoint comes first, the client buffer (if any) second; anything
    /// beyond is closed. The buffer handle is closed afterwards unless the
    /// request left it in `retained` (pending receive), so no error path
    /// leaves orphan handles.
    fn serve(&mut self, owner: Owner, msg: &[u8], handles: &[Handle], now: u64) {
        let Some(&reply_to) = handles.first() else {
            return;
        };
        let buf = handles.get(1).copied();
        for h in handles.iter().skip(2) {
            let _ = sys::handle_close(*h);
        }
        match Request::decode(msg) {
            Some(req) => self.handle(req, owner, reply_to, buf, now),
            None => self.refuse(reply_to, Fault::BadRequest, now),
        }
        let retained = self.retained.take();
        if let Some(b) = buf {
            if retained != Some(b) {
                let _ = sys::handle_close(b);
            }
        }
    }

    /// Serves one request from `owner` (`MAIN` or a session).
    fn handle(
        &mut self,
        req: Request,
        owner: Owner,
        reply_to: Handle,
        buf: Option<Handle>,
        now: u64,
    ) {
        let addr = Ipv4Addr(req.addr);
        let sock = req.sock as usize;
        match req.op {
            Op::TcpConnect
            | Op::TcpListen
            | Op::TcpAccept
            | Op::TcpSend
            | Op::TcpRecv
            | Op::TcpClose => self.tcp_handle(req, owner, reply_to, buf, now),
            Op::Attach => {
                // Only the main endpoint (init, the broker) creates sessions.
                if owner != MAIN {
                    return self.refuse(reply_to, Fault::BadRequest, now);
                }
                match self.grant(req.label_str(), now) {
                    Ok(send) => answer_with(reply_to, Reply::ok(0), send),
                    Err(fault) => self.refuse(reply_to, fault, now),
                }
            }
            Op::Sockets => answer(reply_to, self.sockets_usage(sock)),
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
                if self.pings.is_full() {
                    self.sweep(now);
                    if !self.still_there(owner, reply_to) {
                        return;
                    }
                }
                if self.pings.is_full() {
                    return self.refuse(reply_to, Fault::PingTableFull, now);
                }
                let echo = icmp::Echo {
                    reply: false,
                    identifier: PING_ID,
                    sequence: req.id,
                };
                let mut out = [0u8; 64];
                let Some(n) = echo.write(b"AriCodeOS ping", &mut out) else {
                    return self.refuse(reply_to, Fault::BadRequest, now);
                };
                match self.send_ipv4(addr, ipv4::PROTO_ICMP, &out[..n], now) {
                    Ok(()) => {
                        self.pings.insert(
                            owner,
                            PendingPing {
                                reply_to,
                                dst: addr,
                                seq: req.id,
                                sent_at: now,
                                deadline: now + req.timeout_ms.max(1) as u64,
                            },
                        );
                    }
                    Err(s) => answer(reply_to, Reply::status(s)),
                }
            }
            Op::UdpBind => {
                if req.port == 0 {
                    return self.refuse(reply_to, Fault::BadRequest, now);
                }
                // A port in use may be a dead client's: ask about it first.
                let held_by = |sockets: &Owned<Socket, MAX_SOCKETS>| {
                    sockets
                        .iter()
                        .find(|(_, _, s)| s.port == req.port)
                        .map(|(_, o, _)| o)
                };
                if let Some(o) = held_by(&self.sockets) {
                    self.probe_owner(o, now);
                    if !self.still_there(owner, reply_to) {
                        return;
                    }
                    if held_by(&self.sockets).is_some() {
                        return answer(reply_to, Reply::status(Status::InUse));
                    }
                }
                if self.sockets.is_full() {
                    self.sweep(now);
                    if !self.still_there(owner, reply_to) {
                        return;
                    }
                }
                match self.sockets.insert(owner, new_socket(req.port)) {
                    Some(i) => answer(reply_to, Reply::ok(i as u64)),
                    None => self.refuse(reply_to, Fault::UdpTableFull, now),
                }
            }
            Op::UdpClose => match self.sockets.remove(sock, owner) {
                Some(s) => {
                    if let Some(r) = s.recv {
                        let _ = sys::handle_close(r.buf);
                        answer(r.reply_to, Reply::status(Status::BadSock));
                    }
                    answer(reply_to, Reply::ok(0));
                }
                None => answer(reply_to, Reply::status(Status::BadSock)),
            },
            Op::UdpSend => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Fault::BadHandles, now);
                };
                let Some(port) = self.sockets.get(sock, owner).map(|s| s.port) else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                let len = req.len.min(MAX_DATAGRAM) as usize;
                let mut data = [0u8; MAX_DATAGRAM as usize];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => data[..len].copy_from_slice(&b[..len]),
                    _ => return self.refuse(reply_to, Fault::BadHandles, now),
                }
                match self.send_udp(port, addr, req.port, &data[..len], now) {
                    Ok(()) => answer(reply_to, Reply::ok(len as u64)),
                    Err(s) => answer(reply_to, Reply::status(s)),
                }
            }
            Op::UdpRecv => {
                let Some(buf) = buf else {
                    return self.refuse(reply_to, Fault::BadHandles, now);
                };
                let Some(sock) = self.sockets.get_mut(sock, owner) else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.recv.is_some() {
                    return self.refuse(reply_to, Fault::WaitPending, now);
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
                    return self.refuse(reply_to, Fault::BadHandles, now);
                };
                if self.dns.is_full() {
                    self.sweep(now);
                    if !self.still_there(owner, reply_to) {
                        return;
                    }
                }
                if self.dns.is_full() {
                    return self.refuse(reply_to, Fault::DnsTableFull, now);
                }
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
                    return self.refuse(reply_to, Fault::BadRequest, now);
                }
                let mut name = [0u8; dns::MAX_NAME];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => name[..len].copy_from_slice(&b[..len]),
                    _ => return self.refuse(reply_to, Fault::BadHandles, now),
                }
                let Ok(name_str) = core::str::from_utf8(&name[..len]) else {
                    return self.refuse(reply_to, Fault::BadRequest, now);
                };
                let id = self.dns_id;
                self.dns_id = self.dns_id.wrapping_add(1);
                let mut q = [0u8; 320];
                let Some(n) = dns::write_query(id, name_str, &mut q) else {
                    return self.refuse(reply_to, Fault::BadRequest, now);
                };
                let src_port = self.dns_port;
                match self.send_udp(src_port, server, port, &q[..n], now) {
                    Ok(()) => {
                        self.dns.insert(
                            owner,
                            PendingDns {
                                reply_to,
                                id,
                                deadline: now + req.timeout_ms.max(1) as u64,
                            },
                        );
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

/// Replies with a handle (a session's send side), which is closed if the
/// reply cannot be delivered, and closes the reply endpoint.
fn answer_with(reply_to: Handle, reply: Reply, handle: Handle) {
    if sys::ipc_send(reply_to, &reply.encode(), &[handle], 0).is_err() {
        let _ = sys::handle_close(handle);
    }
    let _ = sys::handle_close(reply_to);
}

/// Announces the MAC and serves requests forever: frame reception (up to
/// `rx::FRAMES_PER_ROUND` frames), up to `IPC_REQUESTS_PER_ROUND` IPC
/// requests (from the main endpoint and every session together), timeouts
/// and waiting for the IRQ unless frames were left in the ring.
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
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    say(name, b"serving requests");
    loop {
        let now = stack.clock.now();
        stack.dhcp_step(now);
        // Reception is bounded per round (incident 0013): a flood cannot
        // keep the requests and the timers below from their turn.
        let round = rx::receive_round(
            rx::FRAMES_PER_ROUND,
            &mut stack,
            &mut frame,
            |s, f| s.link.recv_frame(f),
            |s, f, n| s.on_frame(&f[..n], now),
            |s| s.link.rx_pending(),
        );
        if round.left_over {
            stack.note(Fault::RxBudget, now);
        }
        // Once a second the kernel is asked about every client (ADR 0037).
        if now.saturating_sub(stack.last_sweep) >= SWEEP_MS {
            stack.sweep(now);
        }
        // Requests a sweep took while checking on the clients.
        while let Some(i) = stack.pending.iter().position(Option::is_some) {
            if let Some(p) = stack.pending[i].take() {
                stack.serve(p.owner, &p.msg[..p.n], &p.handles[..p.nh], now);
            }
        }
        // The main endpoint and every session at once (ADR 0020): the index
        // tells which one the request arrived on, and so who owns what it
        // creates. A request served inside the round can change the
        // session table: an `Attach` grants one, a sweep or probe retires a
        // dead client and closes its receive handle. The list is rebuilt
        // before the next receive whenever that happened, so a closed
        // handle is never passed back to the kernel (it would answer
        // `BadHandle` and end the round early) and a session granted
        // mid-round is polled in the same round.
        let mut eps = [SERVICE; abi::IPC_MAX_ANY];
        let mut owners = [MAIN; abi::IPC_MAX_ANY];
        let mut count = 0;
        let mut built = None;
        for _ in 0..IPC_REQUESTS_PER_ROUND {
            if built != Some(stack.generation) {
                count = stack.endpoints(&mut eps, &mut owners);
                built = Some(stack.generation);
            }
            let (which, n, nh) =
                match sys::ipc_receive_any(&eps[..count], &mut msg, &mut handles, 0) {
                    Ok(r) => r,
                    // Every endpoint was closed or revoked (ADR 0030): nobody
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
            stack.serve(owners[which], &msg[..n], &handles[..nh], now);
        }
        stack.expire(now);
        stack.tcp_expire(now);
        stack.tcp_poll(now);
        // Frames left behind are received by the next round at once; a
        // quiet ring waits for the interrupt (or 10 ms, for the timers).
        if !round.left_over {
            let _ = sys::irq_wait(IRQ, 10);
        }
    }
}
