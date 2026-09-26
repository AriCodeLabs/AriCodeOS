//! fsd (Fase 20): servicio de ficheros AriFS en ring 3 sobre el servicio de
//! bloques. Handles: 0x10004 = endpoint propio (READ|WRITE), 0x10005 =
//! endpoint del servicio de bloques (WRITE), 0x10006 = HARDWARE (DMA).
//! `arg` = LBA absoluto de la partición AriFS.
#![no_std]
#![no_main]

use aricode_arifs::{AriFs, BlockDevice, FsError, SECTOR};
use aricode_proto::fs::{Op, Reply, Request, Status, MAX_IO};
use user_testprogs::sys::{self, block::BlockClient, Handle, TIMEOUT_INFINITE};

const SERVICE: Handle = Handle(0x0001_0004);
const BLOCK: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);

fn say(msg: &[u8]) {
    sys::print(b"fsd: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// El servicio de bloques visto como dispositivo de la partición.
struct PartitionDevice {
    blk: BlockClient,
    base_lba: u64,
}

impl BlockDevice for PartitionDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
        self.blk
            .read(self.base_lba + lba, 1)
            .map_err(|_| FsError::Io)?;
        buf.copy_from_slice(&self.blk.buffer()[..SECTOR]);
        Ok(())
    }
    fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
        self.blk.buffer()[..SECTOR].copy_from_slice(buf);
        self.blk
            .write(self.base_lba + lba, 1)
            .map_err(|_| FsError::Io)
    }
}

/// Descriptores abiertos: índice de fichero por fd (global al servicio).
struct Fds([Option<usize>; 16]);

impl Fds {
    fn open(&mut self, index: usize) -> Option<u32> {
        let slot = self.0.iter().position(Option::is_none)?;
        self.0[slot] = Some(index);
        Some(slot as u32)
    }
    fn get(&self, fd: u32) -> Option<usize> {
        self.0.get(fd as usize).copied().flatten()
    }
    fn close(&mut self, fd: u32) -> bool {
        match self.0.get_mut(fd as usize) {
            Some(s) if s.is_some() => {
                *s = None;
                true
            }
            _ => false,
        }
    }
}

fn status_of(e: FsError) -> Status {
    match e {
        FsError::Io | FsError::BadMagic => Status::Io,
        FsError::NotFound => Status::NotFound,
        FsError::Exists => Status::Exists,
        FsError::NoSpace => Status::NoSpace,
        FsError::BadName => Status::Invalid,
        FsError::OutOfRange => Status::OutOfRange,
    }
}

/// Mapeos de buffers de clientes reutilizados por dirección física.
struct ClientBuffers {
    entries: [(u64, u64, u64); 8], // (phys, virt, len)
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

fn handle(
    fs: &mut AriFs<PartitionDevice>,
    fds: &mut Fds,
    buffers: &mut ClientBuffers,
    req: Request,
    data: Option<Handle>,
) -> Reply {
    match req.op {
        Op::List => {
            let mut it = fs.entries().skip(req.fd as usize);
            match it.next() {
                Some((_, e)) => Reply::ok(e.size as u64).with_name(e.name_str()),
                None => Reply::status(Status::NotFound),
            }
        }
        Op::Open => match fs.find(req.name_str()) {
            Some(i) => fds
                .open(i)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            None => Reply::status(Status::NotFound),
        },
        Op::Create => match fs.create(req.name_str()) {
            Ok(i) => fds
                .open(i)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            Err(e) => Reply::status(status_of(e)),
        },
        Op::Close => {
            if fds.close(req.fd) {
                Reply::ok(0)
            } else {
                Reply::status(Status::BadFd)
            }
        }
        Op::Read | Op::Write => {
            let Some(index) = fds.get(req.fd) else {
                return Reply::status(Status::BadFd);
            };
            let Some(dma) = data else {
                return Reply::status(Status::BadHandles);
            };
            let len = req.len.min(MAX_IO) as usize;
            let Some(buf) = buffers.map(dma) else {
                return Reply::status(Status::BadHandles);
            };
            if buf.len() < len {
                return Reply::status(Status::Invalid);
            }
            let result = match req.op {
                Op::Read => fs.read(index, req.offset, &mut buf[..len]),
                _ => fs.write(index, req.offset, &buf[..len]),
            };
            match result {
                Ok(n) => Reply::ok(n as u64),
                Err(e) => Reply::status(status_of(e)),
            }
        }
    }
}

fn main(partition_lba: u64) -> u64 {
    let Ok(blk) = BlockClient::new(BLOCK, HW, 1) else {
        return 1;
    };
    let dev = PartitionDevice {
        blk,
        base_lba: partition_lba,
    };
    let mut fs = match AriFs::open(dev) {
        Ok(fs) => fs,
        Err(_) => {
            say(b"la particion no contiene AriFS");
            return 2;
        }
    };
    let mut buf = [0u8; 20];
    sys::print(b"fsd: AriFS montado, ");
    sys::print(user_testprogs::fmt_u64(
        fs.entries().count() as u64,
        &mut buf,
    ));
    sys::print(b" ficheros\n");
    let mut fds = Fds([None; 16]);
    let mut buffers = ClientBuffers {
        entries: [(0, 0, 0); 8],
        count: 0,
    };
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 2];
    say(b"sirviendo peticiones");
    loop {
        let Ok((n, nh)) = sys::ipc_receive(SERVICE, &mut msg, &mut handles, TIMEOUT_INFINITE)
        else {
            continue;
        };
        if nh == 0 {
            continue;
        }
        let reply_to = handles[0];
        let data = (nh >= 2).then_some(handles[1]);
        let reply = match Request::decode(&msg[..n]) {
            Some(req) => handle(&mut fs, &mut fds, &mut buffers, req, data),
            None => Reply::status(Status::Invalid),
        };
        if let Some(d) = data {
            let _ = sys::handle_close(d);
        }
        let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
        let _ = sys::handle_close(reply_to);
    }
}

user_testprogs::entry!(main);
