//! Cliente del servicio de bloques (`aricode-proto::block`) con un buffer DMA
//! propio: la memoria la aporta el cliente y el dispositivo escribe en ella.

use crate::{self as sys, rights, Error, Handle, TIMEOUT_INFINITE};
use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};

pub struct BlockClient {
    service: Handle,
    reply: Handle,
    dma: Handle,
    buf: u64,
    buf_len: u64,
}

impl BlockClient {
    /// `service`: endpoint del servicio (WRITE); `hardware`: capacidad para
    /// reservar el buffer DMA de `sectors` sectores (máximo `MAX_SECTORS`).
    pub fn new(service: Handle, hardware: Handle, sectors: u16) -> Result<Self, Error> {
        let sectors = sectors.clamp(1, MAX_SECTORS) as u64;
        let reply = sys::endpoint_create()?;
        let dma = sys::dma_alloc(hardware, sectors * 512)?;
        let buf = sys::mmio_map(dma)?;
        Ok(Self {
            service,
            reply,
            dma,
            buf,
            buf_len: sectors * 512,
        })
    }

    /// Buffer de datos (`sectors * 512` bytes).
    pub fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: buffer DMA mapeado RW en este proceso, exclusivo del cliente.
        unsafe { core::slice::from_raw_parts_mut(self.buf as *mut u8, self.buf_len as usize) }
    }

    fn call(&self, req: Request, with_buffer: bool) -> Result<Reply, Error> {
        let reply_w = sys::handle_duplicate(self.reply, rights::WRITE | rights::TRANSFER)?;
        let mut handles = [reply_w, Handle(0)];
        let mut n = 1;
        if with_buffer {
            handles[1] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)?;
            n = 2;
        }
        sys::ipc_send(self.service, &req.encode(), &handles[..n], TIMEOUT_INFINITE)?;
        let mut msg = [0u8; Reply::LEN];
        let mut none: [Handle; 0] = [];
        let (len, _) = sys::ipc_receive(self.reply, &mut msg, &mut none, TIMEOUT_INFINITE)?;
        Reply::decode(&msg[..len]).ok_or(Error::Invalid)
    }

    fn check(reply: Reply) -> Result<u64, Error> {
        match reply.status {
            Status::Ok => Ok(reply.value),
            _ => Err(Error::Invalid),
        }
    }

    pub fn capacity(&self) -> Result<u64, Error> {
        Self::check(self.call(
            Request {
                op: Op::Info,
                count: 0,
                lba: 0,
            },
            false,
        )?)
    }

    /// Lee `count` sectores en el buffer.
    pub fn read(&self, lba: u64, count: u16) -> Result<(), Error> {
        Self::check(self.call(
            Request {
                op: Op::Read,
                count,
                lba,
            },
            true,
        )?)
        .map(|_| ())
    }

    /// Escribe `count` sectores desde el buffer.
    pub fn write(&self, lba: u64, count: u16) -> Result<(), Error> {
        Self::check(self.call(
            Request {
                op: Op::Write,
                count,
                lba,
            },
            true,
        )?)
        .map(|_| ())
    }
}
