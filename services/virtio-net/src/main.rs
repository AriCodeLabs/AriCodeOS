//! netd (Fase 22): driver virtio-net y pila de red mínima en ring 3.
//!
//! Recibe de init el contrato de driver (`aricode_sys::virtio_pci`) y, como
//! extra tras los BARs, el objeto `SYSTEM` (`READ`) para medir el tiempo.
//! Obtiene su dirección por DHCP y atiende `NetServiceProtocol v1`: info,
//! ping, UDP y DNS. Un solo hilo: alterna la cola de recepción, las
//! peticiones IPC y los plazos pendientes.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use aricode_net::tcp::{self, Connection};
use aricode_net::{arp, dhcp, dns, eth, icmp, ipv4, udp, Ipv4Addr, MacAddr};
use aricode_proto::net::{Info, Op, Reply, Request, Status, MAX_DATAGRAM};
use user_testprogs::sys::virtio_pci::{extra_handle, DmaRegion, Queue, Transport, IRQ, SERVICE};
use user_testprogs::sys::{self, abi, Handle};

/// VIRTIO_NET_F_MAC: el dispositivo tiene una MAC en su configuración.
const FEATURE_MAC: u64 = 1 << 5;
/// Cabecera virtio-net (sin MRG_RXBUF pero con VERSION_1: 12 bytes).
const NET_HDR: u64 = 12;
const SLOT: u64 = 2048;
const RX_SLOTS: u16 = 16;
const TX_SLOTS: u16 = 8;
const MAX_FRAME: usize = eth::HEADER_LEN + eth::MTU;

const MAX_PINGS: usize = 4;
const MAX_SOCKETS: usize = 8;
const MAX_DNS: usize = 4;
const MAX_TCP: usize = 8;
const TCP_EPHEMERAL_FIRST: u16 = 50000;
const PING_ID: u16 = 0xA51C;
const DHCP_RETRY_MS: u64 = 1000;
const ARP_WAIT_MS: u64 = 1000;

fn say(msg: &[u8]) {
    sys::print(b"virtio-net: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn print_ip(ip: Ipv4Addr) {
    let mut b = [0u8; 15];
    sys::print(ip.format(&mut b));
}

struct Nic {
    transport: Transport,
    rx: Queue,
    tx: Queue,
    rx_buf: DmaRegion,
    tx_buf: DmaRegion,
    tx_next: u16,
    tx_inflight: u16,
    mac: MacAddr,
}

impl Nic {
    fn setup(mask: u64) -> Result<Self, u64> {
        let transport = Transport::open(mask, FEATURE_MAC).map_err(|e| e.code())?;
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = transport.device_cfg.r8(i as u64);
        }
        let mut rx = transport.setup_queue(0, RX_SLOTS).map_err(|e| e.code())?;
        let tx = transport.setup_queue(1, TX_SLOTS).map_err(|e| e.code())?;
        let rx_buf = DmaRegion::alloc(SLOT * RX_SLOTS as u64).map_err(|_| 30u64)?;
        let tx_buf = DmaRegion::alloc(SLOT * TX_SLOTS as u64).map_err(|_| 31u64)?;
        for i in 0..rx.vq.size().min(RX_SLOTS) {
            rx.vq
                .add(&[(rx_buf.phys + SLOT * i as u64, SLOT as u32, true)])
                .ok_or(32u64)?;
        }
        transport.driver_ok();
        rx.notify(&transport);
        Ok(Self {
            transport,
            rx,
            tx,
            rx_buf,
            tx_buf,
            tx_next: 0,
            tx_inflight: 0,
            mac: MacAddr(mac),
        })
    }

    /// Recoge los envíos completados para reutilizar sus ranuras.
    fn reclaim_tx(&mut self) {
        while self.tx.vq.pop_used().is_some() {
            self.tx_inflight = self.tx_inflight.saturating_sub(1);
        }
    }

    /// Transmite una trama Ethernet completa.
    fn send_frame(&mut self, frame: &[u8]) -> bool {
        self.reclaim_tx();
        let slots = self.tx.vq.size().min(TX_SLOTS);
        if self.tx_inflight >= slots || frame.len() > MAX_FRAME {
            return false;
        }
        let slot = self.tx_next % slots;
        self.tx_next = self.tx_next.wrapping_add(1);
        let off = SLOT * slot as u64;
        let buf = self.tx_buf.slice_mut(off, NET_HDR + frame.len() as u64);
        buf[..NET_HDR as usize].fill(0);
        buf[NET_HDR as usize..].copy_from_slice(frame);
        let ok = self
            .tx
            .vq
            .add(&[(
                self.tx_buf.phys + off,
                (NET_HDR + frame.len() as u64) as u32,
                false,
            )])
            .is_some();
        if ok {
            self.tx_inflight += 1;
            self.tx.notify(&self.transport);
        }
        ok
    }

    /// Siguiente trama recibida (copiada a `out`); repone la ranura.
    fn recv_frame(&mut self, out: &mut [u8; MAX_FRAME]) -> Option<usize> {
        let (head, len) = self.rx.vq.pop_used()?;
        let off = SLOT * head as u64;
        let len = (len as usize)
            .saturating_sub(NET_HDR as usize)
            .min(MAX_FRAME);
        let src = self.rx_buf.slice_mut(off + NET_HDR, len as u64);
        out[..len].copy_from_slice(src);
        // Reponer la misma ranura.
        let _ = self
            .rx
            .vq
            .add(&[(self.rx_buf.phys + off, SLOT as u32, true)]);
        self.rx.notify(&self.transport);
        Some(len)
    }
}

/// Reloj en ms a partir de `system_info`.
struct Clock(Handle);

impl Clock {
    fn now(&self) -> u64 {
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
    /// Último datagrama sin recoger (remitente, puerto, datos).
    queued: Option<(Ipv4Addr, u16, usize)>,
    data: [u8; MAX_DATAGRAM as usize],
}

/// Socket TCP: una conexión (o un puerto de escucha) y sus esperas.
struct TcpSock {
    conn: Connection,
    listening: bool,
    /// Conexión entrante aún no entregada por `TcpAccept` (índice del socket).
    accepted_from: Option<u16>,
    connect: Option<(Handle, u64)>,
    accept: Option<(Handle, u64)>,
    recv: Option<PendingRecv>,
    /// El usuario ya cerró: liberar el slot cuando la conexión termine.
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

/// Trama a la espera de resolver ARP.
struct Deferred {
    next_hop: Ipv4Addr,
    len: usize,
    frame: [u8; MAX_FRAME],
    deadline: u64,
}

/// Mapeos de buffers de clientes por dirección física.
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
        // SAFETY: buffer del cliente mapeado RW en este proceso.
        Some(unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, len as usize) })
    }
}

struct Stack {
    nic: Nic,
    clock: Clock,
    dhcp: dhcp::Client,
    dhcp_next: u64,
    config: Option<dhcp::Config>,
    arp: arp::Cache<8>,
    ip_id: u16,
    dns_id: u16,
    pings: [Option<PendingPing>; MAX_PINGS],
    sockets: [Option<Socket>; MAX_SOCKETS],
    dns: [Option<PendingDns>; MAX_DNS],
    /// En el heap: cada socket lleva 8 KiB de buffers y la pila es de 64 KiB.
    tcp: [Option<Box<TcpSock>>; MAX_TCP],
    tcp_next_port: u16,
    deferred: [Option<Deferred>; 4],
    buffers: ClientBuffers,
    /// Puerto UDP efímero para consultas DNS.
    dns_port: u16,
}

fn new_socket(port: u16) -> Socket {
    Socket {
        port,
        recv: None,
        queued: None,
        data: [0; MAX_DATAGRAM as usize],
    }
}

impl Stack {
    fn new(nic: Nic, clock: Clock) -> Self {
        let xid = 0x4152_4900 | (nic.mac.0[5] as u32);
        Self {
            dhcp: dhcp::Client::new(nic.mac, xid),
            nic,
            clock,
            dhcp_next: 0,
            config: None,
            arp: arp::Cache::new(),
            ip_id: 1,
            dns_id: 1,
            pings: [const { None }; MAX_PINGS],
            sockets: [const { None }; MAX_SOCKETS],
            dns: [const { None }; MAX_DNS],
            tcp: [const { None }; MAX_TCP],
            tcp_next_port: TCP_EPHEMERAL_FIRST,
            deferred: [const { None }; 4],
            buffers: ClientBuffers {
                entries: [(0, 0, 0); 8],
                count: 0,
            },
            dns_port: 49153,
        }
    }

    fn my_ip(&self) -> Ipv4Addr {
        self.config
            .map(|c| c.address)
            .unwrap_or(Ipv4Addr::UNSPECIFIED)
    }

    // ---------------- emisión ----------------

    /// Envía una trama Ethernet con `payload` construido por `fill`.
    fn send_eth(
        &mut self,
        dst: MacAddr,
        ethertype: u16,
        fill: impl FnOnce(&mut [u8]) -> Option<usize>,
    ) -> bool {
        let mut frame = [0u8; MAX_FRAME];
        let hdr = eth::Header {
            dst,
            src: self.nic.mac,
            ethertype,
        };
        let Some(h) = hdr.write(&mut frame) else {
            return false;
        };
        let Some(n) = fill(&mut frame[h..]) else {
            return false;
        };
        self.nic.send_frame(&frame[..h + n])
    }

    /// Siguiente salto para `dst`: el propio destino si está en la subred,
    /// si no el router.
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

    /// Envía un paquete IPv4 a `dst` con `payload` de protocolo `proto`.
    /// Sin MAC conocida lo aparca y lanza ARP.
    fn send_ipv4(&mut self, dst: Ipv4Addr, proto: u8, payload: &[u8]) -> Result<(), Status> {
        let src = self.my_ip();
        let next_hop = self.next_hop(dst).ok_or(Status::NoRoute)?;
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        let mut frame = [0u8; MAX_FRAME];
        let ip_hdr = ipv4::Header::new(src, dst, proto, id, payload.len() as u16);
        let mut n = eth::HEADER_LEN;
        n += ip_hdr.write(&mut frame[n..]).ok_or(Status::Invalid)?;
        if frame.len() < n + payload.len() {
            return Err(Status::Invalid);
        }
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
                    src: self.nic.mac,
                    ethertype: eth::ETHERTYPE_IPV4,
                }
                .write(&mut frame);
                if self.nic.send_frame(&frame[..n]) {
                    Ok(())
                } else {
                    Err(Status::Busy)
                }
            }
            None => {
                let deadline = self.clock.now() + ARP_WAIT_MS;
                let slot = self
                    .deferred
                    .iter_mut()
                    .find(|d| d.is_none())
                    .ok_or(Status::Busy)?;
                *slot = Some(Deferred {
                    next_hop,
                    len: n,
                    frame,
                    deadline,
                });
                self.send_arp_request(next_hop);
                Ok(())
            }
        }
    }

    fn send_arp_request(&mut self, target: Ipv4Addr) {
        let req = arp::Packet::request(self.nic.mac, self.my_ip(), target);
        self.send_eth(MacAddr::BROADCAST, eth::ETHERTYPE_ARP, |b| req.write(b));
    }

    fn send_udp(
        &mut self,
        src_port: u16,
        dst: Ipv4Addr,
        dst_port: u16,
        payload: &[u8],
    ) -> Result<(), Status> {
        let mut seg = [0u8; udp::HEADER_LEN + MAX_DATAGRAM as usize];
        let n = udp::Header { src_port, dst_port }
            .write(self.my_ip(), dst, payload, &mut seg)
            .ok_or(Status::Invalid)?;
        self.send_ipv4(dst, ipv4::PROTO_UDP, &seg[..n])
    }

    /// Envía el DHCP que corresponda (difusión desde 0.0.0.0).
    fn dhcp_step(&mut self, now: u64) {
        if self.dhcp.state() == dhcp::State::Bound || now < self.dhcp_next {
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

    // ---------------- recepción ----------------

    fn on_frame(&mut self, frame: &[u8], now: u64) {
        let Some((eh, payload)) = eth::Header::parse(frame) else {
            return;
        };
        if eh.dst != self.nic.mac && !eh.dst.is_broadcast() {
            return;
        }
        match eh.ethertype {
            eth::ETHERTYPE_ARP => self.on_arp(payload),
            eth::ETHERTYPE_IPV4 => self.on_ipv4(payload, now),
            _ => {}
        }
    }

    fn on_arp(&mut self, b: &[u8]) {
        let Some(p) = arp::Packet::parse(b) else {
            return;
        };
        if !p.sender_ip.is_unspecified() {
            self.arp.insert(p.sender_ip, p.sender_mac);
            self.flush_deferred(p.sender_ip, p.sender_mac);
        }
        if p.op == arp::OP_REQUEST && p.target_ip == self.my_ip() && !self.my_ip().is_unspecified()
        {
            let reply = arp::Packet::reply_to(&p, self.nic.mac, self.my_ip());
            self.send_eth(p.sender_mac, eth::ETHERTYPE_ARP, |out| reply.write(out));
        }
    }

    fn flush_deferred(&mut self, ip: Ipv4Addr, mac: MacAddr) {
        for slot in self.deferred.iter_mut() {
            if slot.as_ref().is_some_and(|d| d.next_hop == ip) {
                let mut d = slot.take().unwrap();
                eth::Header {
                    dst: mac,
                    src: self.nic.mac,
                    ethertype: eth::ETHERTYPE_IPV4,
                }
                .write(&mut d.frame);
                self.nic.send_frame(&d.frame[..d.len]);
            }
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
            // Responder al eco: el anfitrión puede hacernos ping.
            let reply = icmp::Echo {
                reply: true,
                ..echo
            };
            let mut out = [0u8; icmp::HEADER_LEN + eth::MTU];
            if let Some(n) = reply.write(data, &mut out) {
                let _ = self.send_ipv4(ih.src, ipv4::PROTO_ICMP, &out[..n]);
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
            if self.dhcp.state() == dhcp::State::Bound {
                self.config = self.dhcp.config();
                if let Some(c) = self.config {
                    sys::print(b"virtio-net: DHCP ");
                    print_ip(c.address);
                    sys::print(b" mascara ");
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
        // Conexión existente con la misma cuádrupla.
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
        // SYN a un puerto en escucha: nueva conexión pasiva.
        if h.flags & tcp::FLAG_SYN == 0 || h.flags & tcp::FLAG_ACK != 0 {
            return;
        }
        let Some(listener) = self.tcp.iter().position(|s| {
            s.as_ref()
                .is_some_and(|s| s.listening && s.conn.local_port() == h.dst_port)
        }) else {
            return;
        };
        let Some(slot) = self.tcp.iter().position(|s| s.is_none()) else {
            return;
        };
        let mut conn = Connection::new(ih.dst, h.dst_port, ih.src, h.src_port);
        conn.listen();
        conn.on_segment(&h, payload, now);
        let mut sock = Box::new(TcpSock::new(conn));
        sock.accepted_from = Some(listener as u16);
        self.tcp[slot] = Some(sock);
    }

    /// Emite segmentos pendientes y resuelve esperas de todas las conexiones.
    fn tcp_poll(&mut self, now: u64) {
        let mut seg = [0u8; tcp::MAX_SEGMENT];
        for i in 0..MAX_TCP {
            if self.tcp[i].as_ref().is_none_or(|s| s.listening) {
                continue;
            }
            // Sacar el socket del slot mientras se emite: `send_ipv4` necesita
            // el resto de la pila.
            let mut sock = self.tcp[i].take().unwrap();
            let (remote, _) = sock.conn.remote();
            while let Some(n) = sock.conn.poll(now, &mut seg) {
                let _ = self.send_ipv4(remote, ipv4::PROTO_TCP, &seg[..n]);
                if sock.conn.is_closed() {
                    break;
                }
            }
            self.tcp[i] = Some(sock);
            let Some(sock) = self.tcp[i].as_mut() else {
                continue;
            };
            // Conexión activa resuelta.
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
            // Recepción pendiente.
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
            // Liberar conexiones terminadas que el usuario ya cerró.
            if sock.user_closed && sock.conn.is_closed() {
                self.tcp[i] = None;
            }
        }
        // Entregar conexiones aceptadas a los `TcpAccept` pendientes.
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
                    return answer(reply_to, Reply::status(Status::Busy));
                };
                let port = self.tcp_next_port;
                self.tcp_next_port = if port == u16::MAX {
                    TCP_EPHEMERAL_FIRST
                } else {
                    port + 1
                };
                let mut conn = Connection::new(self.my_ip(), port, addr, req.port);
                conn.connect((now as u32).wrapping_mul(64).wrapping_add(slot as u32), now);
                let mut sock = Box::new(TcpSock::new(conn));
                sock.connect = Some((reply_to, now + req.timeout_ms.max(1) as u64));
                self.tcp[slot] = Some(sock);
            }
            Op::TcpListen => {
                if req.port == 0 {
                    return answer(reply_to, Reply::status(Status::Invalid));
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
                    return answer(reply_to, Reply::status(Status::Busy));
                };
                let mut conn = Connection::new(self.my_ip(), req.port, Ipv4Addr::UNSPECIFIED, 0);
                conn.listen();
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
                    return answer(reply_to, Reply::status(Status::Busy));
                }
                listener.accept = Some((reply_to, now + req.timeout_ms.max(1) as u64));
            }
            Op::TcpSend => {
                let Some(buf) = buf else {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                };
                let len = req.len.min(MAX_DATAGRAM) as usize;
                let mut data = [0u8; MAX_DATAGRAM as usize];
                match self.buffers.map(buf) {
                    Some(b) if b.len() >= len => data[..len].copy_from_slice(&b[..len]),
                    _ => return answer(reply_to, Reply::status(Status::BadHandles)),
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
                    return answer(reply_to, Reply::status(Status::BadHandles));
                };
                let Some(sock) = self.tcp.get_mut(req.sock as usize).and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.listening || sock.user_closed || sock.recv.is_some() {
                    return answer(
                        reply_to,
                        Reply::status(if sock.recv.is_some() {
                            Status::Busy
                        } else {
                            Status::BadSock
                        }),
                    );
                }
                sock.recv = Some(PendingRecv {
                    reply_to,
                    buf,
                    max_len: req.len.min(MAX_DATAGRAM),
                    deadline: now + req.timeout_ms.max(1) as u64,
                });
                return; // el buffer se cierra al entregar o al vencer
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
                } else {
                    sock.conn.close();
                }
                answer(reply_to, Reply::ok(0));
            }
            _ => answer(reply_to, Reply::status(Status::Invalid)),
        }
        if let Some(b) = buf {
            let _ = sys::handle_close(b);
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

    // ---------------- plazos ----------------

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
        for slot in self.deferred.iter_mut() {
            if slot.as_ref().is_some_and(|d| now >= d.deadline) {
                *slot = None;
            }
        }
    }

    // ---------------- peticiones IPC ----------------

    fn handle(&mut self, req: Request, reply_to: Handle, buf: Option<Handle>, now: u64) {
        let addr = Ipv4Addr(req.addr);
        match req.op {
            Op::TcpConnect
            | Op::TcpListen
            | Op::TcpAccept
            | Op::TcpSend
            | Op::TcpRecv
            | Op::TcpClose => {
                return self.tcp_handle(req, reply_to, buf, now);
            }
            Op::Info => match self.config {
                Some(c) => {
                    let info = Info {
                        address: c.address.0,
                        mask: c.subnet_mask.0,
                        router: c.router.0,
                        dns: c.dns.0,
                        mac: self.nic.mac.0,
                    };
                    let mut r = Reply::ok(0);
                    r.data = info.encode();
                    answer(reply_to, r);
                }
                None => answer(reply_to, Reply::status(Status::NoRoute)),
            },
            Op::Ping => {
                let Some(slot) = self.pings.iter().position(|p| p.is_none()) else {
                    return answer(reply_to, Reply::status(Status::Busy));
                };
                let echo = icmp::Echo {
                    reply: false,
                    identifier: PING_ID,
                    sequence: req.id,
                };
                let mut out = [0u8; 64];
                let Some(n) = echo.write(b"AriCodeOS ping", &mut out) else {
                    return answer(reply_to, Reply::status(Status::Invalid));
                };
                match self.send_ipv4(addr, ipv4::PROTO_ICMP, &out[..n]) {
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
                if req.port == 0 || self.sockets.iter().flatten().any(|s| s.port == req.port) {
                    return answer(
                        reply_to,
                        Reply::status(if req.port == 0 {
                            Status::Invalid
                        } else {
                            Status::InUse
                        }),
                    );
                }
                match self.sockets.iter().position(|s| s.is_none()) {
                    Some(i) => {
                        self.sockets[i] = Some(new_socket(req.port));
                        answer(reply_to, Reply::ok(i as u64));
                    }
                    None => answer(reply_to, Reply::status(Status::Busy)),
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
                    return answer(reply_to, Reply::status(Status::BadHandles));
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
                    _ => return answer(reply_to, Reply::status(Status::BadHandles)),
                }
                match self.send_udp(port, addr, req.port, &data[..len]) {
                    Ok(()) => answer(reply_to, Reply::ok(len as u64)),
                    Err(s) => answer(reply_to, Reply::status(s)),
                }
            }
            Op::UdpRecv => {
                let Some(buf) = buf else {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                };
                let Some(sock) = self
                    .sockets
                    .get_mut(req.sock as usize)
                    .and_then(|s| s.as_mut())
                else {
                    return answer(reply_to, Reply::status(Status::BadSock));
                };
                if sock.recv.is_some() {
                    return answer(reply_to, Reply::status(Status::Busy));
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
                    return; // el buffer se cierra al entregar o al vencer
                }
            }
            Op::Dns => {
                let Some(slot) = self.dns.iter().position(|d| d.is_none()) else {
                    return answer(reply_to, Reply::status(Status::Busy));
                };
                let (server, port) = if addr.is_unspecified() {
                    match self.config {
                        Some(c) if !c.dns.is_unspecified() => (c.dns, dns::PORT),
                        _ => return answer(reply_to, Reply::status(Status::NoRoute)),
                    }
                } else {
                    (addr, if req.port == 0 { dns::PORT } else { req.port })
                };
                let id = self.dns_id;
                self.dns_id = self.dns_id.wrapping_add(1);
                let mut q = [0u8; 320];
                let Some(n) = dns::write_query(id, req.name_str(), &mut q) else {
                    return answer(reply_to, Reply::status(Status::Invalid));
                };
                let src_port = self.dns_port;
                match self.send_udp(src_port, server, port, &q[..n]) {
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
        if let Some(b) = buf {
            let _ = sys::handle_close(b);
        }
    }
}

/// Entrega un datagrama a una recepción pendiente y libera sus handles.
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

/// Responde por el endpoint transferido y lo cierra.
fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

fn main(mask: u64) -> u64 {
    let nic = match Nic::setup(mask) {
        Ok(n) => n,
        Err(step) => {
            say(b"fallo en la inicializacion");
            return step;
        }
    };
    let clock = Clock(extra_handle(mask, 0));
    let mut hex = [0u8; 16];
    sys::print(b"virtio-net: dispositivo listo, MAC ");
    for (i, b) in nic.mac.0.iter().enumerate() {
        if i > 0 {
            sys::print(b":");
        }
        sys::print(user_testprogs::fmt_hex(*b as u64, 2, &mut hex));
    }
    sys::print(b"\n");
    let mut stack = Stack::new(nic, clock);
    let mut frame = [0u8; MAX_FRAME];
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 2];
    say(b"sirviendo peticiones");
    loop {
        let now = stack.clock.now();
        stack.dhcp_step(now);
        while let Some(n) = stack.nic.recv_frame(&mut frame) {
            stack.on_frame(&frame[..n], now);
        }
        for _ in 0..8 {
            let Ok((n, nh)) = sys::ipc_receive(SERVICE, &mut msg, &mut handles, 0) else {
                break;
            };
            if nh == 0 {
                continue;
            }
            let reply_to = handles[0];
            let buf = (nh >= 2).then_some(handles[1]);
            match Request::decode(&msg[..n]) {
                Some(req) => stack.handle(req, reply_to, buf, now),
                None => {
                    if let Some(b) = buf {
                        let _ = sys::handle_close(b);
                    }
                    answer(reply_to, Reply::status(Status::Invalid));
                }
            }
        }
        stack.expire(now);
        stack.tcp_expire(now);
        stack.tcp_poll(now);
        let _ = sys::irq_wait(IRQ, 10);
    }
}

user_testprogs::entry!(main);
