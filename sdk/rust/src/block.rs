//! Block service client (`aricode-proto::block`) with its own DMA buffer:
//! the client provides the memory and the device writes into it.

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};

/// Reply wait of a block request (ms). Chain of waits (incident 0001): the
/// USB storage server spends at most `REQUEST_BUDGET_MS` = 34 s on one
/// request (a 30 s SCSI command window, as SCSI stacks use, plus the CBW
/// and the recovery reserve), and a request may first queue behind one
/// idle-loop maintenance step (`REENUMERATE_STEP_MS` = 6 s, the longest)
/// and one other request: 34 + 6 + 34 = 74 s < 90 s, assuming at most two
/// concurrent block clients (one queued request). The file client above
/// waits longer still (`fs::REPLY_WAIT_MS`, 120 s).
pub const REPLY_WAIT_MS: u64 = 90_000;

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

    /// One request: the send waits `rpc::GRACE_MS` for the server queue,
    /// the reply `REPLY_WAIT_MS`. The USB storage server releases the
    /// buffer handle only after every transfer on it completed, was
    /// retired, or its slot was disabled; with a reply wait above the
    /// server's worst case a slow disk shows up here as latency, never as
    /// a client that gave up while the device still owned its buffer.
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
            REPLY_WAIT_MS,
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

/// Physical address and length of a client's `dma` buffer, mapped into this
/// server (and its devices' IOMMU domain) for the request. Mapping again is
/// idempotent in the kernel, and the IOMMU entry is restored if the buffer
/// was freed and a new client got the same physical pages.
pub fn map_client_buffer(dma: Handle) -> Option<(u64, u64)> {
    let phys = sys::dma_phys(dma).ok()?;
    let len = sys::region_len(dma).ok()?;
    sys::mmio_map(dma).ok()?;
    Some((phys, len))
}
