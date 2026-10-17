// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! File service client (`aricode-proto::fs`).

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_proto::fs::{Op, Reply, Request, Status, MAX_IO, MAX_REPLACE};

/// Reply wait of a file request (ms): above the block client's
/// (`block::REPLY_WAIT_MS`, 90 s) plus the file service's own work, so a
/// slow disk is latency for the caller and never a reply the service
/// delivers to nobody.
pub const REPLY_WAIT_MS: u64 = 120_000;

pub struct FsClient {
    service: Handle,
    dma: Handle,
    buf: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// The request did not get an answer: `ServiceGone` when the file
    /// service endpoint has nobody to receive (ADR 0030, permanent),
    /// `ReplyLost` when the service dropped the request (it died; a
    /// relaunch may serve the next one), `Timeout` when it did not answer
    /// in time.
    Ipc(Error),
    /// The file service answered with an error.
    Status(Status),
}

impl FsError {
    /// The file service did not answer (gone, crashed or too slow), as
    /// opposed to answering with an error status: the caller learns
    /// nothing about the file.
    pub fn service_unavailable(&self) -> bool {
        matches!(
            self,
            FsError::Ipc(Error::ServiceGone | Error::ReplyLost | Error::Timeout)
        )
    }
}

impl FsClient {
    pub fn new(service: Handle, hardware: Handle) -> Result<Self, Error> {
        let dma = sys::dma_alloc(hardware, MAX_IO as u64)?;
        let buf = sys::mmio_map(dma)?;
        Ok(Self { service, dma, buf })
    }

    fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: DMA buffer of MAX_IO bytes mapped RW, exclusive to the client.
        unsafe { core::slice::from_raw_parts_mut(self.buf as *mut u8, MAX_IO as usize) }
    }

    fn call(&self, req: Request, with_buffer: bool) -> Result<Reply, FsError> {
        let mut extra = [Handle(0); 1];
        let mut n = 0;
        if with_buffer {
            extra[0] =
                sys::handle_duplicate(self.dma, rights::READ | rights::WRITE | rights::TRANSFER)
                    .map_err(FsError::Ipc)?;
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
        )
        .map_err(FsError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(FsError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(reply),
            s => Err(FsError::Status(s)),
        }
    }

    /// Directory entry `index`: (name, size). `None` at the end.
    pub fn list(
        &self,
        index: u32,
        name_out: &mut [u8; 32],
    ) -> Result<Option<(usize, u64)>, FsError> {
        let mut req = Request::new(Op::List);
        req.fd = index;
        match self.call(req, false) {
            Ok(r) => {
                name_out[..r.name_len as usize].copy_from_slice(&r.name[..r.name_len as usize]);
                Ok(Some((r.name_len as usize, r.value)))
            }
            Err(FsError::Status(Status::NotFound)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn open(&self, name: &str) -> Result<u32, FsError> {
        let req = Request::new(Op::Open)
            .with_name(name)
            .ok_or(FsError::Status(Status::Invalid))?;
        self.call(req, false).map(|r| r.value as u32)
    }

    pub fn create(&self, name: &str) -> Result<u32, FsError> {
        let req = Request::new(Op::Create)
            .with_name(name)
            .ok_or(FsError::Status(Status::Invalid))?;
        self.call(req, false).map(|r| r.value as u32)
    }

    pub fn close(&self, fd: u32) -> Result<(), FsError> {
        let mut req = Request::new(Op::Close);
        req.fd = fd;
        self.call(req, false).map(|_| ())
    }

    /// Broker only: asks fsd for an endpoint bound to the private directory
    /// `prefix` and returns the received handle (`WRITE`, transferable).
    pub fn bind(&self, prefix: &str) -> Result<Handle, FsError> {
        self.grant(Op::Bind, prefix)
    }

    /// Broker only: asks fsd for an endpoint bound to the file `name` in
    /// read-only mode (ADR 0016) and returns the received handle.
    pub fn share(&self, name: &str) -> Result<Handle, FsError> {
        self.grant(Op::Share, name)
    }

    fn grant(&self, op: Op, name: &str) -> Result<Handle, FsError> {
        let req = Request::new(op)
            .with_name(name)
            .ok_or(FsError::Status(Status::Invalid))?;
        let mut msg = [0u8; Reply::LEN];
        let mut handles = [Handle(0); 1];
        let (len, nh) = rpc::call_handles(
            self.service,
            &req.encode(),
            &[],
            rpc::GRACE_MS,
            REPLY_WAIT_MS,
            &mut msg,
            &mut handles,
        )
        .map_err(FsError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(FsError::Ipc(Error::Invalid))?;
        match (reply.status, nh) {
            (Status::Ok, 1) => Ok(handles[0]),
            (Status::Ok, _) => Err(FsError::Ipc(Error::Invalid)),
            (s, _) => Err(FsError::Status(s)),
        }
    }

    /// Reads up to `out.len()` bytes (at most `MAX_IO`) from `offset`.
    pub fn read(&mut self, fd: u32, offset: u32, out: &mut [u8]) -> Result<usize, FsError> {
        let len = out.len().min(MAX_IO as usize);
        let mut req = Request::new(Op::Read);
        req.fd = fd;
        req.offset = offset;
        req.len = len as u32;
        let n = self.call(req, true)?.value as usize;
        let n = n.min(len);
        out[..n].copy_from_slice(&self.buffer()[..n]);
        Ok(n)
    }

    /// Writes `data` (at most `MAX_IO`) at `offset`, in place: after a power
    /// cut each sector holds either the old or the new data. Durable after `flush`.
    pub fn write(&mut self, fd: u32, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        let len = data.len().min(MAX_IO as usize);
        self.buffer()[..len].copy_from_slice(&data[..len]);
        let mut req = Request::new(Op::Write);
        req.fd = fd;
        req.offset = offset;
        req.len = len as u32;
        self.call(req, true).map(|r| r.value as usize)
    }

    /// Replaces the whole file content with `data` (at most `MAX_REPLACE`)
    /// atomically and durably (ADR 0014): after a power cut the file holds the
    /// previous content or the new one, never a mix. Above `MAX_IO` it goes in
    /// chunks to the journal followed by a commit (ADR 0018).
    pub fn replace(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        if data.len() > MAX_REPLACE as usize {
            return Err(FsError::Status(Status::OutOfRange));
        }
        if data.len() <= MAX_IO as usize {
            self.buffer()[..data.len()].copy_from_slice(data);
            let mut req = Request::new(Op::Replace);
            req.fd = fd;
            req.len = data.len() as u32;
            return self.call(req, true).map(|r| r.value as usize);
        }
        for (i, chunk) in data.chunks(MAX_IO as usize).enumerate() {
            self.replace_chunk(fd, (i * MAX_IO as usize) as u32, chunk)?;
        }
        self.replace_commit(fd, data.len() as u32)
    }

    /// One chunk (at most `MAX_IO`) of a multi-part replace: the first at
    /// `offset` 0 and the rest contiguous, without gaps; nothing changes until
    /// `replace_commit`. `Busy` if another client has a replace in progress.
    pub fn replace_chunk(&mut self, fd: u32, offset: u32, chunk: &[u8]) -> Result<(), FsError> {
        if chunk.len() > MAX_IO as usize {
            return Err(FsError::Status(Status::OutOfRange));
        }
        self.buffer()[..chunk.len()].copy_from_slice(chunk);
        let mut req = Request::new(Op::ReplaceChunk);
        req.fd = fd;
        req.offset = offset;
        req.len = chunk.len() as u32;
        self.call(req, true).map(|_| ())
    }

    /// Commits the `len` bytes sent with `replace_chunk` (exactly those of
    /// this transaction; otherwise `Invalid`) and applies them at once.
    pub fn replace_commit(&mut self, fd: u32, len: u32) -> Result<usize, FsError> {
        let mut req = Request::new(Op::ReplaceCommit);
        req.fd = fd;
        req.len = len;
        self.call(req, false).map(|r| r.value as usize)
    }

    /// Makes previous writes durable (ADR 0014).
    pub fn flush(&self) -> Result<(), FsError> {
        self.call(Request::new(Op::Flush), false).map(|_| ())
    }
}
