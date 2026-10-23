// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! fsd (Phase 20): AriFS file service in ring 3 on top of the block
//! service. Handles: 0x10004 = own endpoint (READ|WRITE), 0x10005 =
//! block service endpoint (WRITE), 0x10006 = HARDWARE (DMA),
//! 0x10007 = SYSTEM (READ, clock for timeouts). `arg` = absolute LBA of
//! the AriFS partition.
//!
//! Besides its main endpoint (init, the broker) fsd serves *sessions* (ADR
//! 0036): endpoints it creates on request of the main endpoint, bound to a
//! private directory (`Bind`), a read-only file (`Share`) or the whole
//! namespace (`Attach`), of which it keeps only the receive side. Every
//! descriptor belongs to the endpoint that opened it, and when the kernel
//! reports that nobody holds a session's send side any more (its client
//! died) the session is retired and its descriptors are released
//! (incident 0011).
#![no_std]
#![no_main]

use aricode_arifs::descriptors::{Descriptors, MAX_OPEN};
use aricode_arifs::{AriFs, BlockDevice, FsError, Recovery, SECTOR};
use aricode_faults::Meter;
use aricode_proto::fs::{Op, OwnerDescriptors, Reply, Request, Status, MAX_IO};
use aricode_rt::sys::{self, block::BlockClient, client_buffer, Handle};

/// Client buffer mappings kept across requests (ADR 0038: bounded, the
/// oldest is unmapped to make room).
type ClientBuffers = client_buffer::ClientBuffers<8>;

// The protocol's atomic replace is the AriFS journal, and the package limit
// of the whole system follows it (incident 0026): they never drift apart.
const _: () = assert!(
    aricode_proto::fs::MAX_REPLACE as usize == aricode_arifs::JOURNAL_CAPACITY_BYTES
        && aricode_arifs::JOURNAL_CAPACITY_BYTES
            == aricode_arifs::FILE_CAPACITY_SECTORS as usize * SECTOR
);

const SERVICE: Handle = Handle(0x0001_0004);
const BLOCK: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
/// A chunked replace idle for this long is considered abandoned (its
/// owner died or went away): the next one supersedes it
/// (audit 6ee19fb, finding 3).
const STAGING_TIMEOUT_MS: u64 = 2_000;
/// Sessions served at once: every one of them and the main endpoint fit
/// in a single `ipc_receive_any`.
const MAX_SESSIONS: usize = sys::abi::IPC_MAX_ANY - 1;
/// How often fsd asks the kernel whether a session's client is still
/// there, besides doing so whenever a table is full.
const SWEEP_MS: u64 = 1_000;

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

/// What a session exposes (ADR 0016, ADR 0036).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Private directory: the files under `name/`.
    Dir,
    /// A single file (full name), read-only.
    ReadOnlyFile,
    /// The whole namespace, as the main endpoint sees it (`install`
    /// capability, the shells).
    Root,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Dir => "dir",
            Kind::ReadOnlyFile => "file",
            Kind::Root => "root",
        }
    }
}

/// A client's session: the receive side of its endpoint, the name it is
/// bound to (prefix or file; empty for `Root`) and its kind.
#[derive(Clone, Copy)]
struct Session {
    ep: Handle,
    name: [u8; 32],
    len: usize,
    kind: Kind,
}

impl Session {
    fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.len]).unwrap_or("")
    }
    /// Name under which a shared file is shown and opened: without the
    /// directory that contains it.
    fn bare(&self) -> &str {
        let n = self.name_str();
        n.rsplit_once('/').map_or(n, |(_, b)| b)
    }
    /// Prefix the names of requests on this session are qualified with.
    fn prefix(&self) -> &str {
        match self.kind {
            Kind::Dir => self.name_str(),
            Kind::ReadOnlyFile | Kind::Root => "",
        }
    }
}

/// Full name in the table: `prefix/name` inside a private directory, or
/// the name as is from the main namespace. `None` if it does not fit or
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

/// Chunked replace in progress (ADR 0018): file, owner and time of the
/// last chunk. Only one at a time, because there is a single journal; it
/// is released by the commit, an error, closing the descriptor, the
/// retirement of its owner's session or, if its owner disappears without
/// one, the `STAGING_TIMEOUT_MS` timeout. The library tracks which bytes
/// it covers: the commit only accepts what the transaction wrote from
/// offset zero without gaps.
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

/// A request a sweep took from a session while asking the kernel whether
/// its client was alive; served before the next wait.
struct Pending {
    n: usize,
    nh: usize,
    msg: [u8; Request::LEN],
    handles: [Handle; sys::abi::IPC_MAX_HANDLES],
}

/// What fsd records in the kernel event ring (ADR 0028), at most once per
/// second per kind with the count (`aricode_faults`). The code is the
/// status the refused client got; 0 when nobody was refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Descriptors released from clients that died holding them
    /// (incident 0011): the count is the number of descriptors.
    Reclaimed = 0,
    /// `Open`/`Create` refused: the table was full even after looking for
    /// dead clients.
    FdsFull = 1,
    /// `Bind`/`Share`/`Attach` refused: every session slot taken.
    SessionsFull = 2,
    /// `Bind`/`Share`/`Attach` refused because the kernel could not create
    /// the session's endpoint pair (out of handles or memory).
    SessionCreate = 3,
}

// Every event name plus ` x999` fits the kernel's name (incident 0012).
const _: () = {
    assert!(aricode_faults::LABEL_LEN == sys::abi::sysinfo::EVENT_NAME_LEN);
    assert!(Fault::Reclaimed.name().len() + 5 <= aricode_faults::LABEL_LEN);
    assert!(Fault::FdsFull.name().len() + 5 <= aricode_faults::LABEL_LEN);
    assert!(Fault::SessionsFull.name().len() + 5 <= aricode_faults::LABEL_LEN);
    assert!(Fault::SessionCreate.name().len() + 5 <= aricode_faults::LABEL_LEN);
};

impl Fault {
    const COUNT: usize = 4;
    const fn name(self) -> &'static str {
        match self {
            Fault::Reclaimed => "fs fds reclaimed",
            Fault::FdsFull => "fs fds full",
            Fault::SessionsFull => "fs sessions full",
            Fault::SessionCreate => "fs session create",
        }
    }
    fn code(self) -> u64 {
        match self {
            Fault::Reclaimed => Status::Ok as u64,
            Fault::FdsFull | Fault::SessionsFull | Fault::SessionCreate => Status::NoSpace as u64,
        }
    }
}

/// The service: the file system, its clients' state and its sessions.
struct Service {
    fs: AriFs<PartitionDevice>,
    fds: Descriptors,
    buffers: ClientBuffers,
    staging: Option<Staging>,
    /// Sessions by slot; owner number = slot + 1 (0 is the main endpoint).
    sessions: [Option<Session>; MAX_SESSIONS],
    pending: [Option<Pending>; MAX_SESSIONS],
    faults: Meter<{ Fault::COUNT }>,
    /// Descriptors released from dead clients since fsd started.
    reclaimed: u64,
    last_sweep_ms: u64,
    /// An unexpected probe error was already said (once is enough to be
    /// seen; the session is kept and probed again).
    probe_error_said: bool,
}

impl Service {
    fn session(&self, owner: usize) -> Option<Session> {
        owner
            .checked_sub(1)
            .and_then(|i| self.sessions.get(i).copied().flatten())
    }

    /// Records `count` faults of `kind`, bounded by the meter.
    fn fault(&mut self, kind: Fault, count: u32, now: u64) {
        let due = self.faults.report_many(kind as usize, now, count);
        if due > 0 {
            self.record(kind, due);
        }
    }

    fn drain_faults(&mut self, now: u64) {
        while let Some((kind, count)) = self.faults.drain_due(now) {
            let kind = match kind {
                0 => Fault::Reclaimed,
                1 => Fault::FdsFull,
                2 => Fault::SessionsFull,
                _ => Fault::SessionCreate,
            };
            self.record(kind, count);
        }
    }

    fn record(&self, kind: Fault, count: u32) {
        let mut label = [0u8; aricode_faults::LABEL_LEN];
        let name = aricode_faults::label(kind.name(), count, &mut label);
        let _ = sys::event_record(kind.code(), name);
        sys::print(b"fsd: fault ");
        sys::print(name.as_bytes());
        sys::print(b"\n");
    }

    /// Asks the kernel, session by session, whether the client is still
    /// there: a receive that would not wait reports `PeerClosed` once no
    /// send handle is left and the queue is empty (ADR 0030). A request
    /// found waiting is kept for the main loop; a session with one kept is
    /// alive and not asked.
    fn sweep(&mut self, now: u64) {
        self.last_sweep_ms = now;
        for i in 0..MAX_SESSIONS {
            self.probe(i, now);
        }
    }

    /// Asks the kernel whether the client of session `i` is still there
    /// and retires the session if not. `true` while the session lives.
    fn probe(&mut self, i: usize, now: u64) -> bool {
        let Some(s) = self.sessions[i] else {
            return false;
        };
        if self.pending[i].is_some() {
            return true;
        }
        let mut p = Pending {
            n: 0,
            nh: 0,
            msg: [0; Request::LEN],
            handles: [Handle(0); sys::abi::IPC_MAX_HANDLES],
        };
        match sys::ipc_receive(s.ep, &mut p.msg, &mut p.handles, 0) {
            Ok((n, nh)) => {
                p.n = n;
                p.nh = nh;
                self.pending[i] = Some(p);
                true
            }
            Err(e) if e.endpoint_gone() => {
                self.retire(i, now);
                false
            }
            // `Timeout` is the live answer. Anything else (a bad handle, an
            // invalid buffer) is a defect, not a dead client: said once,
            // never silent (ADR 0028), and the session stays.
            Err(sys::Error::Timeout) => true,
            Err(e) => {
                if !self.probe_error_said {
                    self.probe_error_said = true;
                    aricode_rt::print_error(b"fsd: session probe", e);
                }
                true
            }
        }
    }

    /// The client of session `i` is gone: releases its descriptors, its
    /// replace in progress and the endpoint, and accounts the descriptors
    /// it left open as a fault of that client (incident 0011).
    fn retire(&mut self, i: usize, now: u64) {
        let owner = i + 1;
        let Some(s) = self.sessions[i].take() else {
            return;
        };
        let _ = sys::handle_close(s.ep);
        if self.staging.is_some_and(|st| st.owner == owner) {
            self.staging = None;
            self.fs.abandon_replace();
        }
        let n = self.fds.reclaim(owner);
        if n == 0 {
            return;
        }
        self.reclaimed += n as u64;
        let mut buf = [0u8; 20];
        sys::print(b"fsd: client gone (");
        sys::print(s.kind.label().as_bytes());
        if s.len > 0 {
            sys::print(b" '");
            sys::print(&s.name[..s.len]);
            sys::print(b"'");
        }
        sys::print(b"), ");
        sys::print(aricode_rt::fmt_u64(n as u64, &mut buf));
        sys::print(b" descriptor(s) left open: reclaimed\n");
        self.fault(Fault::Reclaimed, n as u32, now);
    }

    /// Serves one request that arrived on `owner`'s endpoint (0 = main).
    fn serve(&mut self, owner: usize, msg: &[u8], handles: &[Handle]) {
        let Some(&reply_to) = handles.first() else {
            return;
        };
        let data = handles.get(1).copied();
        for h in handles.iter().skip(2) {
            let _ = sys::handle_close(*h);
        }
        let reply = match Request::decode(msg) {
            None => Reply::status(Status::Invalid),
            Some(req) if matches!(req.op, Op::Bind | Op::Share | Op::Attach) => {
                // Only the main endpoint (init, the broker) creates sessions.
                let granted = if owner != 0 {
                    Err(Status::Invalid)
                } else {
                    let kind = match req.op {
                        Op::Bind => Kind::Dir,
                        Op::Share => Kind::ReadOnlyFile,
                        _ => Kind::Root,
                    };
                    self.grant(req.name_str(), kind)
                };
                if let Some(d) = data {
                    let _ = sys::handle_close(d);
                }
                match granted {
                    Ok(send) => answer(reply_to, Reply::ok(0), Some(send)),
                    Err(status) => answer(reply_to, Reply::status(status), None),
                }
                return;
            }
            Some(req) => {
                // A full table is first cleared of dead clients' descriptors.
                if matches!(req.op, Op::Open | Op::Create) && self.fds.is_full() {
                    self.sweep(now_ms());
                }
                self.handle(owner, req, data)
            }
        };
        if let Some(d) = data {
            let _ = sys::handle_close(d);
        }
        answer(reply_to, reply, None);
    }

    /// Opens `index` for `owner`, or refuses visibly when the table is
    /// still full.
    fn open(&mut self, index: usize, owner: usize) -> Reply {
        match self.fds.open(index, owner) {
            Some(fd) => Reply::ok(fd as u64),
            None => {
                self.fault(Fault::FdsFull, 1, now_ms());
                Reply::status(Status::NoSpace)
            }
        }
    }

    fn handle(&mut self, owner: usize, req: Request, data: Option<Handle>) -> Reply {
        let mut qbuf = [0u8; 64];
        let session = self.session(owner);
        // Read-only shared file: a single visible name and no operation
        // that writes (ADR 0016).
        if let Some(shared) = session.filter(|s| s.kind == Kind::ReadOnlyFile) {
            match req.op {
                Op::List => {
                    return match (req.fd, self.fs.find(shared.name_str())) {
                        (0, Some(i)) => {
                            let size = self.fs.entry(i).map_or(0, |e| e.size);
                            Reply::ok(size as u64).with_name(shared.bare())
                        }
                        _ => Reply::status(Status::NotFound),
                    };
                }
                Op::Open => {
                    return match self.fs.find(shared.name_str()) {
                        Some(i) if req.name_str() == shared.bare() => self.open(i, owner),
                        _ => Reply::status(Status::NotFound),
                    };
                }
                Op::Read | Op::Close => {}
                _ => return Reply::status(Status::Denied),
            }
        }
        let prefix = session.as_ref().map_or("", Session::prefix);
        match req.op {
            // Handled in `serve`, only on the main endpoint.
            Op::Bind | Op::Share | Op::Attach => Reply::status(Status::Invalid),
            Op::Descriptors => {
                if session.is_some_and(|s| s.kind != Kind::Root) {
                    return Reply::status(Status::Denied);
                }
                self.descriptors(req.fd as usize)
            }
            Op::List => {
                // From a private directory only its own files are visible,
                // without the prefix; from the main namespace, all of them
                // with full names.
                let mut visible = self.fs.entries().filter(|(_, e)| {
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
            Op::Open => {
                match qualified(prefix, req.name_str(), &mut qbuf).and_then(|q| self.fs.find(q)) {
                    Some(i) => self.open(i, owner),
                    None => Reply::status(Status::NotFound),
                }
            }
            Op::Create => match qualified(prefix, req.name_str(), &mut qbuf)
                .ok_or(FsError::BadName)
                .and_then(|q| self.fs.create(q))
            {
                Ok(i) => self.open(i, owner),
                Err(e) => Reply::status(status_of(e)),
            },
            Op::Close => {
                let index = self.fds.get(req.fd, owner);
                if self.fds.close(req.fd, owner) {
                    if index.is_some_and(|i| self.staging.is_some_and(|s| s.same(i, owner))) {
                        self.staging = None;
                        self.fs.abandon_replace();
                    }
                    Reply::ok(0)
                } else {
                    Reply::status(Status::BadFd)
                }
            }
            Op::ReplaceCommit => {
                let Some(index) = self.fds.get(req.fd, owner) else {
                    return Reply::status(Status::BadFd);
                };
                if !self.staging.is_some_and(|s| s.same(index, owner)) {
                    return Reply::status(Status::Busy);
                }
                self.staging = None;
                match self.fs.replace_commit(index, req.len) {
                    Ok(()) => Reply::ok(req.len as u64),
                    Err(e) => {
                        self.fs.abandon_replace();
                        Reply::status(status_of(e))
                    }
                }
            }
            Op::Flush => match self.fs.flush() {
                Ok(()) => Reply::ok(0),
                Err(e) => Reply::status(status_of(e)),
            },
            Op::Read | Op::Write | Op::Replace | Op::ReplaceChunk => {
                let Some(index) = self.fds.get(req.fd, owner) else {
                    return Reply::status(Status::BadFd);
                };
                let Some(dma) = data else {
                    return Reply::status(Status::BadHandles);
                };
                let len = req.len.min(MAX_IO) as usize;
                let now = now_ms();
                if matches!(req.op, Op::Replace | Op::ReplaceChunk) {
                    if let Some(s) = self.staging.filter(|s| !s.same(index, owner)) {
                        // The journal is a one-slot table: before refusing,
                        // ask the kernel whether its owner is still there
                        // (`retire` abandons the transaction of a dead one).
                        if let Some(i) = s.owner.checked_sub(1) {
                            self.probe(i, now);
                        }
                        if self.staging.is_some_and(|s| !s.abandoned(now)) {
                            return Reply::status(Status::Busy);
                        }
                        // The previous owner did not come back: its
                        // transaction is discarded.
                        self.staging = None;
                        self.fs.abandon_replace();
                    }
                }
                let Some(buf) = self.buffers.map(dma) else {
                    return Reply::status(Status::BadHandles);
                };
                if buf.len() < len {
                    return Reply::status(Status::Invalid);
                }
                let result = match req.op {
                    Op::Read => self.fs.read(index, req.offset, &mut buf[..len]),
                    Op::Write => self.fs.write(index, req.offset, &buf[..len]),
                    Op::Replace => {
                        self.staging = None;
                        self.fs.replace(index, &buf[..len]).map(|()| len)
                    }
                    _ => {
                        let r = self.fs.replace_chunk(index, req.offset, &buf[..len]);
                        self.staging = r.is_ok().then_some(Staging {
                            index,
                            owner,
                            last_ms: now,
                        });
                        if r.is_err() {
                            self.fs.abandon_replace();
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

    /// `Op::Descriptors`: usage of owner slot `slot` and its label.
    fn descriptors(&self, slot: usize) -> Reply {
        if slot > MAX_SESSIONS {
            return Reply::status(Status::NotFound);
        }
        let usage = OwnerDescriptors {
            owned: self.fds.owned_by(slot) as u16,
            used: self.fds.used() as u16,
            capacity: MAX_OPEN as u16,
            reclaimed: self.reclaimed.min(u16::MAX as u64) as u16,
        };
        let mut label = [0u8; 40];
        let shown = if slot == 0 {
            "main"
        } else {
            match self.sessions[slot - 1] {
                None => "",
                Some(s) => {
                    let kind = s.kind.label().as_bytes();
                    label[..kind.len()].copy_from_slice(kind);
                    let mut n = kind.len();
                    if s.len > 0 {
                        label[n] = b' ';
                        label[n + 1..n + 1 + s.len].copy_from_slice(&s.name[..s.len]);
                        n += 1 + s.len;
                    }
                    core::str::from_utf8(&label[..n]).unwrap_or("")
                }
            }
        };
        Reply::ok(usage.encode()).with_name(shown)
    }

    /// Creates a session: a private directory (`Bind`, new or existing
    /// prefix), a read-only shared file (`Share`, which must exist) or the
    /// whole namespace (`Attach`, no name). A new endpoint pair of which
    /// fsd keeps the receive side; the send side goes to the client, and
    /// when its last holder dies the session is retired. Each launch gets
    /// its own session, so the descriptors of one never mix with another's.
    fn grant(&mut self, name: &str, kind: Kind) -> Result<Handle, Status> {
        let valid = match kind {
            Kind::Dir => {
                !name.is_empty()
                    && name.len() <= 32
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            }
            Kind::ReadOnlyFile => name.len() <= 32,
            Kind::Root => name.is_empty(),
        };
        if !valid {
            return Err(Status::Invalid);
        }
        if kind == Kind::ReadOnlyFile && self.fs.find(name).is_none() {
            return Err(Status::NotFound);
        }
        let free = |sessions: &[Option<Session>]| sessions.iter().position(Option::is_none);
        let slot = match free(&self.sessions) {
            Some(slot) => slot,
            None => {
                // Full: the sessions of dead clients go first.
                let now = now_ms();
                self.sweep(now);
                match free(&self.sessions) {
                    Some(slot) => slot,
                    None => {
                        self.fault(Fault::SessionsFull, 1, now);
                        return Err(Status::NoSpace);
                    }
                }
            }
        };
        let Ok((receive, send)) = sys::endpoint_create_pair() else {
            self.fault(Fault::SessionCreate, 1, now_ms());
            return Err(Status::NoSpace);
        };
        let mut n = [0u8; 32];
        n[..name.len()].copy_from_slice(name.as_bytes());
        self.sessions[slot] = Some(Session {
            ep: receive,
            name: n,
            len: name.len(),
            kind,
        });
        match kind {
            Kind::Dir => {
                sys::print(b"fsd: private directory '");
                sys::print(name.as_bytes());
                sys::print(b"'\n");
            }
            Kind::ReadOnlyFile => {
                sys::print(b"fsd: shared file '");
                sys::print(name.as_bytes());
                sys::print(b"' (read-only)\n");
            }
            Kind::Root => {}
        }
        Ok(send)
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
    let fs = match AriFs::open(dev) {
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
    let mut srv = Service {
        fs,
        fds: Descriptors::new(),
        buffers: ClientBuffers::new(),
        staging: None,
        sessions: [None; MAX_SESSIONS],
        pending: [const { None }; MAX_SESSIONS],
        faults: Meter::new(),
        reclaimed: 0,
        last_sweep_ms: 0,
        probe_error_said: false,
    };
    let mut msg = [0u8; Request::LEN];
    say(b"serving requests");
    loop {
        let now = now_ms();
        if now.saturating_sub(srv.last_sweep_ms) >= SWEEP_MS {
            srv.sweep(now);
        }
        srv.drain_faults(now);
        // Requests a sweep took while checking on the clients.
        while let Some(i) = srv.pending.iter().position(Option::is_some) {
            if let Some(p) = srv.pending[i].take() {
                srv.serve(i + 1, &p.msg[..p.n], &p.handles[..p.nh]);
            }
        }
        // Wait on the main endpoint and on every session at once (ADR
        // 0020): the index tells which one the request arrived on.
        let mut eps = [SERVICE; sys::abi::IPC_MAX_ANY];
        let mut owners = [0usize; sys::abi::IPC_MAX_ANY];
        let mut count = 1;
        for (i, s) in srv.sessions.iter().enumerate() {
            if let Some(s) = s {
                eps[count] = s.ep;
                owners[count] = i + 1;
                count += 1;
            }
        }
        let mut received = [Handle(0); sys::abi::IPC_MAX_HANDLES];
        match sys::ipc_receive_any(&eps[..count], &mut msg, &mut received, SWEEP_MS) {
            Ok((which, n, nh)) => srv.serve(owners[which], &msg[..n], &received[..nh]),
            // Every endpoint closed, or one revoked (ADR 0030): nothing
            // will arrive again and the call would return at once.
            Err(e) if e.endpoint_gone() => {
                say(b"service endpoints closed; exiting");
                return 8;
            }
            Err(_) => {}
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

aricode_rt::entry!(main);
