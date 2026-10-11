//! fsd (Phase 20): AriFS file service in ring 3 on top of the block
//! service. Handles: 0x10004 = own endpoint (READ|WRITE), 0x10005 =
//! block service endpoint (WRITE), 0x10006 = HARDWARE (DMA),
//! 0x10007 = SYSTEM (READ, clock for timeouts). `arg` = absolute LBA of
//! the AriFS partition.
#![no_std]
#![no_main]

use aricode_arifs::{AriFs, BlockDevice, FsError, Recovery, SECTOR};
use aricode_proto::fs::{Op, Reply, Request, Status, MAX_IO};
use aricode_rt::sys::{self, block::BlockClient, Handle};

const SERVICE: Handle = Handle(0x0001_0004);
const BLOCK: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
/// A chunked replace idle for this long is considered abandoned (its
/// owner died or went away): the next one supersedes it
/// (audit 6ee19fb, finding 3).
const STAGING_TIMEOUT_MS: u64 = 2_000;

fn now_ms() -> u64 {
    aricode_rt::sys::time::uptime_ms(SYSTEM).unwrap_or(0)
}

fn say(msg: &[u8]) {
    sys::print(b"fsd: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// The block service seen as the partition's device.
struct PartitionDevice {
    blk: BlockClient,
    base_lba: u64,
}

impl BlockDevice for PartitionDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
        self.read_sectors(lba, buf)
    }
    fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
        self.write_sectors(lba, buf)
    }
    fn flush(&mut self) -> Result<(), FsError> {
        self.blk.flush().map_err(|_| FsError::Io)
    }
    /// A whole batch in one request to the block service (stage 5).
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        let count = (buf.len() / SECTOR) as u16;
        self.blk
            .read(self.base_lba + lba, count)
            .map_err(|_| FsError::Io)?;
        buf.copy_from_slice(&self.blk.buffer()[..buf.len()]);
        Ok(())
    }
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        let count = (buf.len() / SECTOR) as u16;
        self.blk.buffer()[..buf.len()].copy_from_slice(buf);
        self.blk
            .write(self.base_lba + lba, count)
            .map_err(|_| FsError::Io)
    }
}

/// Open descriptors: (file index, owner endpoint). A descriptor only
/// answers the endpoint (private directory) that opened it: without this,
/// an application could read, write or close another one's files by
/// guessing descriptor numbers.
struct Fds([Option<(usize, usize)>; 16]);

impl Fds {
    fn open(&mut self, index: usize, owner: usize) -> Option<u32> {
        let slot = self.0.iter().position(Option::is_none)?;
        self.0[slot] = Some((index, owner));
        Some(slot as u32)
    }
    fn get(&self, fd: u32, owner: usize) -> Option<usize> {
        match self.0.get(fd as usize).copied().flatten() {
            Some((index, o)) if o == owner => Some(index),
            _ => None,
        }
    }
    fn close(&mut self, fd: u32, owner: usize) -> bool {
        match self.0.get_mut(fd as usize) {
            Some(s) if s.is_some_and(|(_, o)| o == owner) => {
                *s = None;
                true
            }
            _ => false,
        }
    }
}

fn status_of(e: FsError) -> Status {
    match e {
        FsError::Io | FsError::BadMagic | FsError::Corrupt => Status::Io,
        FsError::NotFound => Status::NotFound,
        FsError::Exists => Status::Exists,
        FsError::NoSpace => Status::NoSpace,
        FsError::BadName | FsError::BadTransaction => Status::Invalid,
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

/// What an endpoint granted by the broker exposes (ADR 0016).
#[derive(Clone, Copy, PartialEq, Eq)]
enum NsKind {
    /// Private directory: the files under `name/`.
    Dir,
    /// A single file (full name), read-only.
    ReadOnlyFile,
}

/// Namespace bound to an endpoint: read side, name (prefix or file)
/// and kind.
#[derive(Clone, Copy)]
struct Namespace {
    ep: Handle,
    name: [u8; 32],
    len: usize,
    kind: NsKind,
}

impl Namespace {
    fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.len]).unwrap_or("")
    }
    /// Name under which a shared file is shown and opened: without the
    /// directory that contains it.
    fn bare(&self) -> &str {
        let n = self.name_str();
        n.rsplit_once('/').map_or(n, |(_, b)| b)
    }
}

/// Full name in the table: `prefix/name` inside a private directory, or
/// the name as is from the main endpoint. `None` if it does not fit or
/// tries to escape the prefix.
fn qualified<'a>(prefix: &str, name: &str, buf: &'a mut [u8; 64]) -> Option<&'a str> {
    if name.is_empty() {
        return None;
    }
    if prefix.is_empty() {
        // Main namespace (init): full names, with directories
        // (`apps/x.aripkg`); the library validates the shape.
        if name.len() > buf.len() {
            return None;
        }
        buf[..name.len()].copy_from_slice(name.as_bytes());
        return core::str::from_utf8(&buf[..name.len()]).ok();
    }
    if name.contains('/') {
        return None;
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

/// Chunked replace in progress (ADR 0018): file, owner endpoint and time
/// of the last chunk. Only one at a time, because there is a single
/// journal; it is released by the commit, an error, closing the descriptor
/// or, if its owner disappears, the `STAGING_TIMEOUT_MS` timeout. The
/// library tracks which bytes it covers: the commit only accepts what the
/// transaction wrote from offset zero without gaps.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Staging {
    index: usize,
    owner: usize,
    last_ms: u64,
}

impl Staging {
    fn same(&self, index: usize, owner: usize) -> bool {
        self.index == index && self.owner == owner
    }
    fn abandoned(&self, now: u64) -> bool {
        now.saturating_sub(self.last_ms) >= STAGING_TIMEOUT_MS
    }
}

/// Service state shared by all requests.
struct Server {
    fds: Fds,
    buffers: ClientBuffers,
    staging: Option<Staging>,
}

fn handle(
    fs: &mut AriFs<PartitionDevice>,
    srv: &mut Server,
    ns: Option<&Namespace>,
    owner: usize,
    req: Request,
    data: Option<Handle>,
) -> Reply {
    let mut qbuf = [0u8; 64];
    // Read-only shared file: a single visible name and no operation that
    // writes (ADR 0016).
    if let Some(shared) = ns.filter(|n| n.kind == NsKind::ReadOnlyFile) {
        match req.op {
            Op::List => {
                return match (req.fd, fs.find(shared.name_str())) {
                    (0, Some(i)) => {
                        let size = fs.entry(i).map_or(0, |e| e.size);
                        Reply::ok(size as u64).with_name(shared.bare())
                    }
                    _ => Reply::status(Status::NotFound),
                };
            }
            Op::Open => {
                return match fs.find(shared.name_str()) {
                    Some(i) if req.name_str() == shared.bare() => srv
                        .fds
                        .open(i, owner)
                        .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
                    _ => Reply::status(Status::NotFound),
                };
            }
            Op::Read | Op::Close => {}
            _ => return Reply::status(Status::Denied),
        }
    }
    let prefix = ns.map_or("", Namespace::name_str);
    match req.op {
        // Handled outside, only on the main endpoint.
        Op::Bind | Op::Share => Reply::status(Status::Invalid),
        Op::List => {
            // From a private directory only its own files are visible, without
            // the prefix; from the main endpoint, all of them with full names.
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
            Some(i) => srv
                .fds
                .open(i, owner)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            None => Reply::status(Status::NotFound),
        },
        Op::Create => match qualified(prefix, req.name_str(), &mut qbuf)
            .ok_or(FsError::BadName)
            .and_then(|q| fs.create(q))
        {
            Ok(i) => srv
                .fds
                .open(i, owner)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            Err(e) => Reply::status(status_of(e)),
        },
        Op::Close => {
            let index = srv.fds.get(req.fd, owner);
            if srv.fds.close(req.fd, owner) {
                if index.is_some_and(|i| srv.staging.is_some_and(|s| s.same(i, owner))) {
                    srv.staging = None;
                    fs.abandon_replace();
                }
                Reply::ok(0)
            } else {
                Reply::status(Status::BadFd)
            }
        }
        Op::ReplaceCommit => {
            let Some(index) = srv.fds.get(req.fd, owner) else {
                return Reply::status(Status::BadFd);
            };
            if !srv.staging.is_some_and(|s| s.same(index, owner)) {
                return Reply::status(Status::Busy);
            }
            srv.staging = None;
            match fs.replace_commit(index, req.len) {
                Ok(()) => Reply::ok(req.len as u64),
                Err(e) => {
                    fs.abandon_replace();
                    Reply::status(status_of(e))
                }
            }
        }
        Op::Flush => match fs.flush() {
            Ok(()) => Reply::ok(0),
            Err(e) => Reply::status(status_of(e)),
        },
        Op::Read | Op::Write | Op::Replace | Op::ReplaceChunk => {
            let Some(index) = srv.fds.get(req.fd, owner) else {
                return Reply::status(Status::BadFd);
            };
            let Some(dma) = data else {
                return Reply::status(Status::BadHandles);
            };
            let len = req.len.min(MAX_IO) as usize;
            let Some(buf) = srv.buffers.map(dma) else {
                return Reply::status(Status::BadHandles);
            };
            if buf.len() < len {
                return Reply::status(Status::Invalid);
            }
            let now = now_ms();
            if matches!(req.op, Op::Replace | Op::ReplaceChunk) {
                if let Some(s) = srv.staging.filter(|s| !s.same(index, owner)) {
                    if !s.abandoned(now) {
                        return Reply::status(Status::Busy);
                    }
                    // The previous owner did not come back: its transaction is discarded.
                    srv.staging = None;
                    fs.abandon_replace();
                }
            }
            let result = match req.op {
                Op::Read => fs.read(index, req.offset, &mut buf[..len]),
                Op::Write => fs.write(index, req.offset, &buf[..len]),
                Op::Replace => {
                    srv.staging = None;
                    fs.replace(index, &buf[..len]).map(|()| len)
                }
                _ => {
                    let r = fs.replace_chunk(index, req.offset, &buf[..len]);
                    srv.staging = r.is_ok().then_some(Staging {
                        index,
                        owner,
                        last_ms: now,
                    });
                    if r.is_err() {
                        fs.abandon_replace();
                    }
                    r.map(|()| len)
                }
            };
            match result {
                Ok(n) => Reply::ok(n as u64),
                Err(e) => Reply::status(status_of(e)),
            }
        }
    }
}

fn main(partition_lba: u64) -> u64 {
    let Ok(blk) = BlockClient::new(BLOCK, HW, aricode_arifs::MAX_BATCH_SECTORS as u16) else {
        return 1;
    };
    let dev = PartitionDevice {
        blk,
        base_lba: partition_lba,
    };
    let mut fs = match AriFs::open(dev) {
        Ok(fs) => fs,
        Err(FsError::Corrupt) => {
            say(b"AriFS corrupt: not mounting");
            return 3;
        }
        Err(_) => {
            say(b"partition does not contain AriFS");
            return 2;
        }
    };
    match fs.recovery() {
        Recovery::Clean => {}
        Recovery::Replayed { index } => {
            sys::print(b"fsd: pending replace of '");
            sys::print(fs.entry(index).map_or("?", |e| e.name_str()).as_bytes());
            sys::print(b"' redone at mount\n");
        }
        Recovery::Discarded => say(b"broken journal discarded at mount"),
    }
    let mut buf = [0u8; 20];
    sys::print(b"fsd: AriFS mounted, ");
    sys::print(aricode_rt::fmt_u64(fs.entries().count() as u64, &mut buf));
    sys::print(b" files\n");
    let mut srv = Server {
        fds: Fds([None; 16]),
        buffers: ClientBuffers {
            entries: [(0, 0, 0); 8],
            count: 0,
        },
        staging: None,
    };
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 2];
    // Granted namespaces: private directories and shared files, each with
    // its own endpoint (read side).
    let mut namespaces: [Option<Namespace>; 8] = [None; 8];
    say(b"serving requests");
    loop {
        // Wait on the main endpoint and on all granted ones at once
        // (ADR 0020): the index tells which one the request arrived on.
        let mut eps = [SERVICE; 9];
        let mut slots = [0usize; 9];
        let mut count = 1;
        for (i, ns) in namespaces.iter().enumerate() {
            if let Some(ns) = ns {
                eps[count] = ns.ep;
                slots[count] = i + 1;
                count += 1;
            }
        }
        let mut received = [Handle(0); sys::abi::IPC_MAX_HANDLES];
        let Ok((which, n, nh)) = sys::ipc_receive_any(&eps[..count], &mut msg, &mut received, 1000)
        else {
            continue;
        };
        handles[..2].copy_from_slice(&received[..2]);
        {
            let slot = slots[which];
            let ns = (slot > 0).then(|| namespaces[slot - 1]).flatten();
            if nh == 0 {
                continue;
            }
            let reply_to = handles[0];
            let data = (nh >= 2).then_some(handles[1]);
            let request = Request::decode(&msg[..n]);
            if let Some(
                req @ Request {
                    op: Op::Bind | Op::Share,
                    ..
                },
            ) = request
            {
                if slot != 0 {
                    answer(reply_to, Reply::status(Status::Invalid), None);
                } else {
                    let kind = if req.op == Op::Bind {
                        NsKind::Dir
                    } else {
                        NsKind::ReadOnlyFile
                    };
                    match grant(&fs, &mut namespaces, req.name_str(), kind) {
                        Ok(client_ep) => answer(reply_to, Reply::ok(0), Some(client_ep)),
                        Err(status) => answer(reply_to, Reply::status(status), None),
                    }
                }
                if let Some(d) = data {
                    let _ = sys::handle_close(d);
                }
                continue;
            }
            let reply = match request {
                Some(req) => handle(&mut fs, &mut srv, ns.as_ref(), slot, req, data),
                None => Reply::status(Status::Invalid),
            };
            if let Some(d) = data {
                let _ = sys::handle_close(d);
            }
            answer(reply_to, reply, None);
        }
    }
}

/// Replies on the transferred endpoint (with an optional handle) and closes it.
fn answer(reply_to: Handle, reply: Reply, handle: Option<Handle>) {
    let sent = match handle {
        Some(h) => sys::ipc_send(reply_to, &reply.encode(), &[h], 0),
        None => sys::ipc_send(reply_to, &reply.encode(), &[], 0),
    };
    if sent.is_err() {
        if let Some(h) = handle {
            let _ = sys::handle_close(h);
        }
    }
    let _ = sys::handle_close(reply_to);
}

/// Grants a namespace: a private directory (`Bind`, new or existing
/// prefix) or a read-only shared file (`Share`, which must exist). A new
/// endpoint whose write side is transferred to the client; fsd keeps the
/// read side.
fn grant(
    fs: &AriFs<PartitionDevice>,
    namespaces: &mut [Option<Namespace>; 8],
    name: &str,
    kind: NsKind,
) -> Result<Handle, Status> {
    let valid = match kind {
        NsKind::Dir => {
            !name.is_empty()
                && name.len() <= 32
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        }
        NsKind::ReadOnlyFile => name.len() <= 32,
    };
    if !valid {
        return Err(Status::Invalid);
    }
    if kind == NsKind::ReadOnlyFile && fs.find(name).is_none() {
        return Err(Status::NotFound);
    }
    // Same name and kind share an endpoint: each launch of an application
    // gets another handle to the same namespace.
    if let Some(ns) = namespaces
        .iter()
        .flatten()
        .find(|ns| ns.kind == kind && ns.name_str() == name)
    {
        return sys::handle_duplicate(ns.ep, sys::rights::WRITE | sys::rights::TRANSFER)
            .map_err(|_| Status::NoSpace);
    }
    let slot = namespaces
        .iter()
        .position(Option::is_none)
        .ok_or(Status::NoSpace)?;
    let ep = sys::endpoint_create().map_err(|_| Status::NoSpace)?;
    let client = sys::handle_duplicate(ep, sys::rights::WRITE | sys::rights::TRANSFER)
        .map_err(|_| Status::NoSpace)?;
    let mut n = [0u8; 32];
    n[..name.len()].copy_from_slice(name.as_bytes());
    namespaces[slot] = Some(Namespace {
        ep,
        name: n,
        len: name.len(),
        kind,
    });
    sys::print(match kind {
        NsKind::Dir => b"fsd: private directory '".as_slice(),
        NsKind::ReadOnlyFile => b"fsd: shared file '".as_slice(),
    });
    sys::print(name.as_bytes());
    sys::print(match kind {
        NsKind::Dir => b"'\n".as_slice(),
        NsKind::ReadOnlyFile => b"' (read-only)\n".as_slice(),
    });
    Ok(client)
}

aricode_rt::entry!(main);
