//! Cliente del servicio de ficheros (`aricode-proto::fs`).

use crate::{self as sys, rights, rpc, Error, Handle};
use aricode_proto::fs::{Op, Reply, Request, Status, MAX_IO};

pub struct FsClient {
    service: Handle,
    dma: Handle,
    buf: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Ipc(Error),
    Status(Status),
}

impl FsClient {
    pub fn new(service: Handle, hardware: Handle) -> Result<Self, Error> {
        let dma = sys::dma_alloc(hardware, MAX_IO as u64)?;
        let buf = sys::mmio_map(dma)?;
        Ok(Self { service, dma, buf })
    }

    fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: buffer DMA de MAX_IO bytes mapeado RW, exclusivo del cliente.
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
            rpc::GRACE_MS,
            &mut msg,
        )
        .map_err(FsError::Ipc)?;
        let reply = Reply::decode(&msg[..len]).ok_or(FsError::Ipc(Error::Invalid))?;
        match reply.status {
            Status::Ok => Ok(reply),
            s => Err(FsError::Status(s)),
        }
    }

    /// Entrada `index` del directorio: (nombre, tamaño). `None` al final.
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

    /// Solo para el broker: pide a fsd un endpoint ligado al directorio
    /// privado `prefix` y devuelve el handle recibido (`WRITE`, transferible).
    pub fn bind(&self, prefix: &str) -> Result<Handle, FsError> {
        let req = Request::new(Op::Bind)
            .with_name(prefix)
            .ok_or(FsError::Status(Status::Invalid))?;
        let mut msg = [0u8; Reply::LEN];
        let mut handles = [Handle(0); 1];
        let (len, nh) = rpc::call_handles(
            self.service,
            &req.encode(),
            &[],
            rpc::GRACE_MS,
            rpc::GRACE_MS,
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

    /// Lee hasta `out.len()` bytes (máximo `MAX_IO`) desde `offset`.
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

    /// Escribe `data` (máximo `MAX_IO`) en `offset`, en el sitio: tras un
    /// corte cada sector tiene lo viejo o lo nuevo. Duradero tras `flush`.
    pub fn write(&mut self, fd: u32, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        let len = data.len().min(MAX_IO as usize);
        self.buffer()[..len].copy_from_slice(&data[..len]);
        let mut req = Request::new(Op::Write);
        req.fd = fd;
        req.offset = offset;
        req.len = len as u32;
        self.call(req, true).map(|r| r.value as usize)
    }

    /// Sustituye el contenido entero del fichero por `data` (máximo `MAX_IO`)
    /// de forma atómica y duradera (ADR 0014): tras un corte el fichero tiene
    /// el contenido anterior o el nuevo, nunca una mezcla.
    pub fn replace(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        if data.len() > MAX_IO as usize {
            return Err(FsError::Status(Status::OutOfRange));
        }
        self.buffer()[..data.len()].copy_from_slice(data);
        let mut req = Request::new(Op::Replace);
        req.fd = fd;
        req.len = data.len() as u32;
        self.call(req, true).map(|r| r.value as usize)
    }

    /// Hace duraderas las escrituras anteriores (ADR 0014).
    pub fn flush(&self) -> Result<(), FsError> {
        self.call(Request::new(Op::Flush), false).map(|_| ())
    }
}
