// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
use aricode_arifs::{AriFs, BlockDevice, FsError, SECTOR};
use aricode_proto::fs::{Op, Reply, Request, Status, MAX_IO};
#[derive(Clone, Copy)]
struct Handle(u32);
mod sys {
    use super::Handle;
    pub fn dma_phys(_: Handle) -> Result<u64, ()> {
        Ok(4096)
    }
    pub fn region_len(_: Handle) -> Result<u64, ()> {
        Ok(4096)
    }
    pub fn mmio_map(_: Handle) -> Result<u64, ()> {
        Ok(super::DMA.with(|p| p.get()))
    }
}
thread_local! { static DMA:std::cell::Cell<u64>=const { std::cell::Cell::new(0) }; }
struct PartitionDevice(Vec<[u8; 512]>);
impl BlockDevice for PartitionDevice {
    fn read_sector(&mut self, lba: u64, b: &mut [u8; SECTOR]) -> Result<(), FsError> {
        *b = *self.0.get(lba as usize).ok_or(FsError::Io)?;
        Ok(())
    }
    fn write_sector(&mut self, lba: u64, b: &[u8; SECTOR]) -> Result<(), FsError> {
        *self.0.get_mut(lba as usize).ok_or(FsError::Io)? = *b;
        Ok(())
    }
}
/// Open descriptors: file index per fd (global to the service).
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

/// Client buffer mappings, reused by physical address.
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
        // SAFETY: client buffer mapped RW in this process.
        Some(unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, len as usize) })
    }
}

/// Full name in the table: `prefix/name` inside a private directory, or the
/// name as-is from the main endpoint. `None` if it does not fit or tries to
/// escape the prefix.
fn qualified<'a>(prefix: &str, name: &str, buf: &'a mut [u8; 64]) -> Option<&'a str> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    if prefix.is_empty() {
        buf[..name.len()].copy_from_slice(name.as_bytes());
        return core::str::from_utf8(&buf[..name.len()]).ok();
    }
    let total = prefix.len() + 1 + name.len();
    if total > buf.len() {
        return None;
    }
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    buf[prefix.len()] = b'/';
    buf[prefix.len() + 1..total].copy_from_slice(name.as_bytes());
    core::str::from_utf8(&buf[..total]).ok()
}

fn handle(
    fs: &mut AriFs<PartitionDevice>,
    fds: &mut Fds,
    buffers: &mut ClientBuffers,
    prefix: &str,
    req: Request,
    data: Option<Handle>,
) -> Reply {
    let mut qbuf = [0u8; 64];
    match req.op {
        Op::Bind => Reply::status(Status::Invalid), // handled elsewhere, only on the main endpoint
        Op::List => {
            // From a private directory only its own files are visible, without prefix;
            // from the main endpoint, all of them with their full name.
            let mut visible = fs.entries().filter(|(_, e)| {
                prefix.is_empty()
                    || e.name_str()
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            match visible.nth(req.fd as usize) {
                Some((_, e)) => {
                    let shown = if prefix.is_empty() {
                        e.name_str()
                    } else {
                        &e.name_str()[prefix.len() + 1..]
                    };
                    Reply::ok(e.size as u64).with_name(shown)
                }
                None => Reply::status(Status::NotFound),
            }
        }
        Op::Open => match qualified(prefix, req.name_str(), &mut qbuf).and_then(|q| fs.find(q)) {
            Some(i) => fds
                .open(i)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            None => Reply::status(Status::NotFound),
        },
        Op::Create => match qualified(prefix, req.name_str(), &mut qbuf)
            .ok_or(FsError::BadName)
            .and_then(|q| fs.create(q))
        {
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

fn main() {
    let dma = Box::leak(Box::new([0u8; 4096]));
    DMA.with(|p| p.set(dma.as_mut_ptr() as u64));
    let mut fs = AriFs::format(PartitionDevice(vec![[0; 512]; 4096]), 4096).unwrap();
    let mut fds = Fds([None; 16]);
    let mut buffers = ClientBuffers {
        entries: [(0, 0, 0); 8],
        count: 0,
    };
    let create = Request::new(Op::Create).with_name("secret.txt").unwrap();
    let opened = handle(&mut fs, &mut fds, &mut buffers, "victim", create, None);
    assert_eq!(opened.status, Status::Ok);
    let fd = opened.value as u32;
    dma[..6].copy_from_slice(b"SECRET");
    let mut write = Request::new(Op::Write);
    write.fd = fd;
    write.len = 6;
    assert_eq!(
        handle(
            &mut fs,
            &mut fds,
            &mut buffers,
            "victim",
            write,
            Some(Handle(1))
        )
        .status,
        Status::Ok
    );
    dma.fill(0);
    let mut read = Request::new(Op::Read);
    read.fd = fd;
    read.len = 6;
    let result = handle(
        &mut fs,
        &mut fds,
        &mut buffers,
        "attacker",
        read,
        Some(Handle(1)),
    );
    println!(
        "Read through attacker namespace of victim fd {fd}: {:?}, bytes={:?}",
        result.status,
        std::str::from_utf8(&dma[..6])
    );
    assert_ne!(
        result.status,
        Status::Ok,
        "private directory boundary bypassed through guessed global fd"
    );
}
