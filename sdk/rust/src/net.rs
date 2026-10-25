// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Network service client (`aricode-proto::net`).

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_proto::net::{
    Info, Op, OwnerSockets, Reply, Request, Status, TcpStats, MAX_DATAGRAM, MAX_LABEL,
};

/// How long `tcp_send_all` waits in all for room in a full send buffer.
pub const SEND_IDLE_TIMEOUT_MS: u32 = 5_000;
/// Pause between two attempts to send into a full buffer.
const SEND_POLL_MS: u32 = 10;

pub struct NetClient {
    service: Handle,
    dma: Handle,
    buf: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    Ipc(Error),
    Status(Status),
}

impl NetError {
    /// One line for the user, naming the status: a refusal by the network
    /// service says which one ("network service refused: busy ...") instead
    /// of a generic "rejected" (incident 0012). A protocol outcome
    /// (timeout, no configuration, unknown name, reset) keeps its own
    /// wording, since the service did not refuse anything.
    pub fn describe(self) -> &'static str {
        match self {
            NetError::Ipc(_) => "network service not responding",
            NetError::Status(s) => match s {
                Status::Ok => "ok",
                Status::Timeout => "no reply (timed out)",
                Status::NoRoute => "no network configuration",
                Status::NotFound => "unknown name",
                Status::Reset => "connection refused or reset by the other end",
                Status::Denied => "port reserved to another service",
                Status::Busy => "network service refused: busy (no free connection or send slot)",
                Status::Invalid => "network service refused: invalid (malformed request)",
                Status::BadHandles => {
                    "network service refused: bad handles (client buffer missing or not accepted)"
                }
                Status::BadSock => {
                    "network service refused: bad socket (unknown, closed or listening)"
                }
                Status::InUse => "network service refused: port in use",
            },
        }
    }
}

/// Init and the broker only: asks the network service, through its main
/// endpoint, for a session for the client called `label` (ADR 0037) and
/// returns the received handle (`WRITE`, transferable). The sockets the
/// process opens through it are released when it dies. A label longer than
/// `MAX_LABEL` is cut at a character boundary, as the kernel cuts process
/// names. No buffer is needed, so no `NetClient` is.
/// `port`: the one privileged port (below `PRIVILEGED_PORTS`) the session
/// may listen or bind on, 0 for none.
pub fn attach(service: Handle, label: &str, port: u16) -> Result<Handle, NetError> {
    let mut cut = label.len().min(MAX_LABEL);
    while !label.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut req = Request::new(Op::Attach)
        .with_label(&label[..cut])
        .ok_or(NetError::Status(Status::Invalid))?;
    req.port = port;
    let mut msg = [0u8; Reply::LEN];
    let mut handles = [Handle(0); 1];
    let (len, nh) = rpc::call_handles(
        service,
        &req.encode(),
        &[],
        rpc::GRACE_MS,
        rpc::GRACE_MS,
        &mut msg,
        &mut handles,
    )
    .map_err(NetError::Ipc)?;
    let reply = Reply::decode(&msg[..len]).ok_or(NetError::Ipc(Error::Invalid))?;
    match (reply.status, nh) {
        (Status::Ok, 1) => Ok(handles[0]),
        (Status::Ok, _) => Err(NetError::Ipc(Error::Invalid)),
        (s, _) => {
            for h in &handles[..nh] {
                let _ = sys::handle_close(*h);
            }
            Err(NetError::Status(s))
        }
    }
}

/// Received datagram: sender and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram {
    pub from: [u8; 4],
    pub port: u16,
    pub len: usize,
}

impl NetClient {
    pub fn new(service: Handle, hardware: Handle) -> Result<Self, Error> {
        let dma = sys::dma_alloc(hardware, MAX_DATAGRAM as u64)?;
        let buf = sys::mmio_map(dma)?;
        Ok(Self { service, dma, buf })
    }

    fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: DMA buffer of MAX_DATAGRAM bytes mapped RW, exclusive to the client.
        unsafe { core::slice::from_raw_parts_mut(self.buf as *mut u8, MAX_DATAGRAM as usize) }
    }

    fn call(&self, req: Request, with_buffer: bool) -> Result<Reply, NetError> {
        let mut extra = [Handle(0); 1];
        let mut n = 0;
        if with_buffer {
            extra[0] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)
                    .map_err(NetError::Ipc)?;
            n = 1;
        }
        let mut msg = [0u8; Reply::LEN];
        // The request deadline (recv, connect, accept, ping) plus the slack.
        let wait = req.timeout_ms as u64 + rpc::GRACE_MS;
        let len = rpc::call(
            self.service,
            &req.encode(),
            &extra[..n],
            rpc::GRACE_MS,
            wait,
            &mut msg,
        )
        .map_err(NetError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(NetError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(reply),
            s => Err(NetError::Status(s)),
        }
    }

    pub fn info(&self) -> Result<Info, NetError> {
        self.call(Request::new(Op::Info), false)
            .map(|r| Info::decode(&r.data))
    }

    /// Physical link state (`LINK_UNKNOWN`, `LINK_DOWN` or `LINK_UP`),
    /// available even if DHCP has not finished.
    pub fn link(&self) -> Result<u64, NetError> {
        self.call(Request::new(Op::Link), false).map(|r| r.value)
    }

    /// The service's TCP table by state (`diag`, incident 0013), available
    /// even if DHCP has not finished.
    pub fn tcp_stats(&self) -> Result<TcpStats, NetError> {
        self.call(Request::new(Op::TcpStats), false)
            .map(|r| TcpStats::decode(&r.data))
    }

    /// Socket table usage of owner slot `index` (ADR 0037): the counts and
    /// the owner's label in `label_out`. `None` past the last slot.
    pub fn sockets(
        &self,
        index: u16,
        label_out: &mut [u8; MAX_LABEL],
    ) -> Result<Option<(OwnerSockets, usize)>, NetError> {
        let mut req = Request::new(Op::Sockets);
        req.sock = index;
        match self.call(req, false) {
            Ok(r) => {
                let label = r.label_str();
                label_out[..label.len()].copy_from_slice(label.as_bytes());
                Ok(Some((OwnerSockets::decode(r.value), label.len())))
            }
            Err(NetError::Status(Status::NotFound)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// ICMP echo; returns the RTT in ms.
    pub fn ping(&self, addr: [u8; 4], seq: u16, timeout_ms: u32) -> Result<u64, NetError> {
        let mut req = Request::new(Op::Ping);
        req.addr = addr;
        req.id = seq;
        req.timeout_ms = timeout_ms;
        self.call(req, false).map(|r| r.value)
    }

    pub fn udp_bind(&self, port: u16) -> Result<u16, NetError> {
        let mut req = Request::new(Op::UdpBind);
        req.port = port;
        self.call(req, false).map(|r| r.value as u16)
    }

    pub fn udp_close(&self, sock: u16) -> Result<(), NetError> {
        let mut req = Request::new(Op::UdpClose);
        req.sock = sock;
        self.call(req, false).map(|_| ())
    }

    pub fn udp_send(
        &mut self,
        sock: u16,
        addr: [u8; 4],
        port: u16,
        data: &[u8],
    ) -> Result<usize, NetError> {
        let len = data.len().min(MAX_DATAGRAM as usize);
        self.buffer()[..len].copy_from_slice(&data[..len]);
        let mut req = Request::new(Op::UdpSend);
        req.sock = sock;
        req.addr = addr;
        req.port = port;
        req.len = len as u16;
        self.call(req, true).map(|r| r.value as usize)
    }

    pub fn udp_recv(
        &mut self,
        sock: u16,
        timeout_ms: u32,
        out: &mut [u8],
    ) -> Result<Datagram, NetError> {
        let mut req = Request::new(Op::UdpRecv);
        req.sock = sock;
        req.timeout_ms = timeout_ms;
        req.len = out.len().min(MAX_DATAGRAM as usize) as u16;
        let r = self.call(req, true)?;
        let len = (r.value as usize).min(out.len());
        out[..len].copy_from_slice(&self.buffer()[..len]);
        Ok(Datagram {
            from: r.addr(),
            port: r.port,
            len,
        })
    }

    pub fn tcp_connect(&self, addr: [u8; 4], port: u16, timeout_ms: u32) -> Result<u16, NetError> {
        let mut req = Request::new(Op::TcpConnect);
        req.addr = addr;
        req.port = port;
        req.timeout_ms = timeout_ms;
        self.call(req, false).map(|r| r.value as u16)
    }

    pub fn tcp_listen(&self, port: u16) -> Result<u16, NetError> {
        let mut req = Request::new(Op::TcpListen);
        req.port = port;
        self.call(req, false).map(|r| r.value as u16)
    }

    /// Accepts a connection: returns the new socket and the peer.
    pub fn tcp_accept(&self, sock: u16, timeout_ms: u32) -> Result<(u16, [u8; 4], u16), NetError> {
        let mut req = Request::new(Op::TcpAccept);
        req.sock = sock;
        req.timeout_ms = timeout_ms;
        self.call(req, false)
            .map(|r| (r.value as u16, r.addr(), r.port))
    }

    /// Queues data; returns how much was accepted (may be less than `data`).
    pub fn tcp_send(&mut self, sock: u16, data: &[u8]) -> Result<usize, NetError> {
        let len = data.len().min(MAX_DATAGRAM as usize);
        self.buffer()[..len].copy_from_slice(&data[..len]);
        let mut req = Request::new(Op::TcpSend);
        req.sock = sock;
        req.len = len as u16;
        self.call(req, true).map(|r| r.value as usize)
    }

    /// Sends all of `data`, waiting up to [`SEND_IDLE_TIMEOUT_MS`] for room
    /// when the buffer is full.
    pub fn tcp_send_all(&mut self, sock: u16, data: &[u8]) -> Result<(), NetError> {
        self.tcp_send_all_within(sock, data, SEND_IDLE_TIMEOUT_MS)
    }

    /// Sends all of `data`, waiting at most `timeout_ms` in all for room
    /// when the buffer is full (`Status::Timeout` otherwise). A timeout of
    /// `0` sends what fits now and gives up on the first full buffer.
    pub fn tcp_send_all_within(
        &mut self,
        sock: u16,
        data: &[u8],
        timeout_ms: u32,
    ) -> Result<(), NetError> {
        let mut done = 0;
        let mut waited_ms = 0u32;
        while done < data.len() {
            let n = self.tcp_send(sock, &data[done..])?;
            done += n;
            if n == 0 {
                if waited_ms >= timeout_ms {
                    return Err(NetError::Status(Status::Timeout));
                }
                sys::thread_sleep(SEND_POLL_MS as u64);
                waited_ms = waited_ms.saturating_add(SEND_POLL_MS);
            }
        }
        Ok(())
    }

    /// Receives data; `Ok(0)` means the other end closed.
    pub fn tcp_recv(
        &mut self,
        sock: u16,
        timeout_ms: u32,
        out: &mut [u8],
    ) -> Result<usize, NetError> {
        let mut req = Request::new(Op::TcpRecv);
        req.sock = sock;
        req.timeout_ms = timeout_ms;
        req.len = out.len().min(MAX_DATAGRAM as usize) as u16;
        let r = self.call(req, true)?;
        let len = (r.value as usize).min(out.len());
        out[..len].copy_from_slice(&self.buffer()[..len]);
        Ok(len)
    }

    pub fn tcp_close(&self, sock: u16) -> Result<(), NetError> {
        let mut req = Request::new(Op::TcpClose);
        req.sock = sock;
        self.call(req, false).map(|_| ())
    }

    /// Resolves a name (up to 253 bytes, not truncated); `server` =
    /// `Some((ip, port))` or `None` for the configured DNS server.
    pub fn resolve(
        &mut self,
        name: &str,
        server: Option<([u8; 4], u16)>,
        timeout_ms: u32,
    ) -> Result<[u8; 4], NetError> {
        if name.is_empty() || name.len() > aricode_net::dns::MAX_NAME {
            return Err(NetError::Status(Status::Invalid));
        }
        self.buffer()[..name.len()].copy_from_slice(name.as_bytes());
        let mut req = Request::new(Op::Dns);
        req.len = name.len() as u16;
        if let Some((addr, port)) = server {
            req.addr = addr;
            req.port = port;
        }
        req.timeout_ms = timeout_ms;
        self.call(req, true).map(|r| r.addr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Status; 10] = [
        Status::Ok,
        Status::Invalid,
        Status::Timeout,
        Status::NoRoute,
        Status::BadHandles,
        Status::Busy,
        Status::BadSock,
        Status::InUse,
        Status::NotFound,
        Status::Reset,
    ];

    /// Every status has its own text and none hides behind a generic
    /// "rejected the request" (incident 0012).
    #[test]
    fn every_status_is_named() {
        for (i, a) in ALL.iter().enumerate() {
            let text = NetError::Status(*a).describe();
            assert!(
                !text.is_empty() && !text.contains("rejected"),
                "{a:?}: {text}"
            );
            for b in &ALL[i + 1..] {
                assert_ne!(text, NetError::Status(*b).describe(), "{a:?} vs {b:?}");
            }
        }
        assert_eq!(
            NetError::Ipc(Error::Timeout).describe(),
            "network service not responding"
        );
    }

    /// Refusals by the service say so and name the status; protocol
    /// outcomes do not claim a refusal.
    #[test]
    fn refusals_say_refused_and_name_the_status() {
        for s in [
            Status::Busy,
            Status::Invalid,
            Status::BadHandles,
            Status::BadSock,
            Status::InUse,
        ] {
            let text = NetError::Status(s).describe();
            assert!(
                text.starts_with("network service refused: "),
                "{s:?}: {text}"
            );
        }
        assert_eq!(
            NetError::Status(Status::Busy).describe(),
            "network service refused: busy (no free connection or send slot)"
        );
        for s in [
            Status::Timeout,
            Status::NoRoute,
            Status::NotFound,
            Status::Reset,
        ] {
            let text = NetError::Status(s).describe();
            assert!(!text.starts_with("network service"), "{s:?}: {text}");
        }
    }
}
