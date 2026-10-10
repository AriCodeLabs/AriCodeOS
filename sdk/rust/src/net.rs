//! Cliente del servicio de red (`aricode-proto::net`).

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_proto::net::{Info, Op, Reply, Request, Status, MAX_DATAGRAM};

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

/// Datagrama recibido: remitente y longitud.
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
        // SAFETY: buffer DMA de MAX_DATAGRAM bytes mapeado RW, exclusivo del cliente.
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
        // El plazo de la peticion (recv, connect, accept, ping) mas el margen.
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

    /// Estado del enlace físico (`LINK_UNKNOWN`, `LINK_DOWN` o `LINK_UP`),
    /// disponible aunque DHCP no haya terminado.
    pub fn link(&self) -> Result<u64, NetError> {
        self.call(Request::new(Op::Link), false).map(|r| r.value)
    }

    /// Eco ICMP; devuelve el RTT en ms.
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

    /// Acepta una conexión: devuelve el socket nuevo y el remitente.
    pub fn tcp_accept(&self, sock: u16, timeout_ms: u32) -> Result<(u16, [u8; 4], u16), NetError> {
        let mut req = Request::new(Op::TcpAccept);
        req.sock = sock;
        req.timeout_ms = timeout_ms;
        self.call(req, false)
            .map(|r| (r.value as u16, r.addr(), r.port))
    }

    /// Encola datos; devuelve los aceptados (puede ser menos que `data`).
    pub fn tcp_send(&mut self, sock: u16, data: &[u8]) -> Result<usize, NetError> {
        let len = data.len().min(MAX_DATAGRAM as usize);
        self.buffer()[..len].copy_from_slice(&data[..len]);
        let mut req = Request::new(Op::TcpSend);
        req.sock = sock;
        req.len = len as u16;
        self.call(req, true).map(|r| r.value as usize)
    }

    /// Envía todo `data`, esperando cuando el buffer está lleno.
    pub fn tcp_send_all(&mut self, sock: u16, data: &[u8]) -> Result<(), NetError> {
        let mut done = 0;
        let mut idle = 0;
        while done < data.len() {
            let n = self.tcp_send(sock, &data[done..])?;
            done += n;
            if n == 0 {
                idle += 1;
                if idle > 500 {
                    return Err(NetError::Status(Status::Timeout));
                }
                sys::thread_sleep(10);
            }
        }
        Ok(())
    }

    /// Recibe datos; `Ok(0)` significa que el otro extremo cerró.
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

    /// Resuelve un nombre (hasta 253 bytes, sin recortes); `server` =
    /// `Some((ip, puerto))` o `None` para el servidor DNS de la configuración.
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
