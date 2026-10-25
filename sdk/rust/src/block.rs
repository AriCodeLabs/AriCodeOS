// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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

    /// Points the client at another block service, keeping its DMA buffer
    /// (init probing several USB disks with one buffer).
    pub fn set_service(&mut self, service: Handle) {
        self.service = service;
    }

    /// Data buffer (`sectors * 512` bytes).
    pub fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: DMA buffer mapped RW in this process, exclusive to the client.
        unsafe { core::slice::from_raw_parts_mut(self.buf as *mut u8, self.buf_len as usize) }
    }

    /// One request: the send waits `rpc::GRACE_MS` for the server queue,
    /// the reply `REPLY_WAIT_MS`. The block servers release the buffer
    /// handle only once the device cannot reach the buffer (every transfer
    /// on it completed or was retired, its slot was released, or the device
    /// was reset) and keep it otherwise (incident 0017); with a reply wait above the
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

/// Mappings of client buffers a block server keeps for its devices (ADR
/// 0038): the server only needs the buffer in its IOMMU domain, and at most
/// this many dead clients' buffers stay allocated because of it.
pub type ClientBuffers = crate::client_buffer::ClientBuffers<8>;

/// Physical address and length of a client's `dma` buffer, mapped into
/// this server and its devices' IOMMU domain (the IOMMU entry stays until
/// the buffer is freed).
pub fn map_client_buffer(buffers: &mut ClientBuffers, dma: Handle) -> Option<(u64, u64)> {
    buffers.device_view(dma)
}

/// First and last LBA of the GPT partition `name` on the disk behind
/// `blk`. The table is read in chunks straight into the GPT scanner, so
/// its size (up to `aricode_gpt::MAX_ENTRIES` entries) costs no memory: a
/// crafted stick cannot exhaust a program's fixed heap (audit of incident
/// 0032, M2).
pub fn find_partition(blk: &mut BlockClient, name: &str) -> Option<(u64, u64)> {
    const SECTOR: usize = 512;
    const CHUNK: u64 = 16;
    blk.read(1, 1).ok()?;
    let header = aricode_gpt::Header::parse(&blk.buffer()[..SECTOR]).ok()?;
    let sectors = header.entries_bytes().div_ceil(SECTOR) as u64;
    let mut parts = [None; 16];
    let mut scan = aricode_gpt::TableScan::new(&header, &mut parts);
    let mut lba = 0;
    while lba < sectors {
        let n = (sectors - lba).min(CHUNK) as u16;
        blk.read(header.entries_lba.checked_add(lba)?, n).ok()?;
        scan.feed(&blk.buffer()[..n as usize * SECTOR]);
        lba += n as u64;
    }
    let count = scan.finish().ok()?;
    parts[..count]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| (p.first_lba, p.last_lba))
}
