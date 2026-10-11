//! Block service client (`aricode-proto::block`) with its own DMA buffer:
//! the client provides the memory and the device writes into it.

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};

pub struct BlockClient {
    service: Handle,
    dma: Handle,
    buf: u64,
    buf_len: u64,
}

impl BlockClient {
    /// `service`: service endpoint (WRITE); `hardware`: capability to
    /// allocate the DMA buffer of `sectors` sectors (at most `MAX_SECTORS`).
    pub fn new(service: Handle, hardware: Handle, sectors: u16) -> Result<Self, Error> {
        let sectors = sectors.clamp(1, MAX_SECTORS) as u64;
        let dma = sys::dma_alloc(hardware, sectors * 512)?;
        let buf = sys::mmio_map(dma)?;
        Ok(Self {
            service,
            dma,
            buf,
            buf_len: sectors * 512,
        })
    }

    /// Data buffer (`sectors * 512` bytes).
    pub fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: DMA buffer mapped RW in this process, exclusive to the client.
        unsafe { core::slice::from_raw_parts_mut(self.buf as *mut u8, self.buf_len as usize) }
    }

    fn call(&self, req: Request, with_buffer: bool) -> Result<Reply, Error> {
        let mut extra = [Handle(0); 1];
        let mut n = 0;
        if with_buffer {
            extra[0] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)?;
            n = 1;
        }
        let mut msg = [0u8; Reply::LEN];
        let len = rpc::call(
            self.service,
            &req.encode(),
            &extra[..n],
            rpc::GRACE_MS,
            rpc::GRACE_MS,
            &mut msg,
        )?;
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

    /// Reads `count` sectors into the buffer.
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

    /// Writes `count` sectors from the buffer.
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

    /// Makes previous writes durable (ADR 0014).
    pub fn flush(&self) -> Result<(), Error> {
        Self::check(self.call(
            Request {
                op: Op::Flush,
                count: 0,
                lba: 0,
            },
            false,
        )?)
        .map(|_| ())
    }
}

/// Client DMA buffers mapped in a block server, reused by physical address
/// (each `mmio_map` creates a new mapping that is not released until the
/// process exits).
pub struct ClientBuffers {
    entries: [(u64, u64); 8], // (phys, len)
    count: usize,
}

impl Default for ClientBuffers {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuffers {
    pub const fn new() -> Self {
        Self {
            entries: [(0, 0); 8],
            count: 0,
        }
    }

    /// Physical address and length of the `dma` buffer, mapping it the first time.
    pub fn map(&mut self, dma: Handle) -> Option<(u64, u64)> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        if let Some(e) = self.entries[..self.count].iter().find(|e| e.0 == phys) {
            return Some(*e);
        }
        sys::mmio_map(dma).ok()?;
        if self.count < self.entries.len() {
            self.entries[self.count] = (phys, len);
            self.count += 1;
        }
        Some((phys, len))
    }
}
