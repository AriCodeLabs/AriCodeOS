// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `updater` (Phase 30, ADR 0027, ADR 0029): the system's update client.
//!
//! Argument (`aricode_update_index::updater_arg`):
//!
//! - `APPLY` (0): installs the image the `UPDATE` partition holds into the
//!   inactive slot and marks it pending with three tries; AriBoot tries it
//!   on the next boot and init confirms it if the system comes up.
//! - `REFRESH` (1, shell `update`): fetches `index` and `index.sig` from
//!   every registered source, verifies them with the project key, stores
//!   the verified copies and reports what is newer than what is installed.
//!   Installs nothing.
//! - `UPGRADE` (2, shell `upgrade`): verifies the stored indexes again and
//!   installs what they offer over the installed versions: the system image
//!   into the inactive slot, app packages into `pkgs/` for the installer.
//!   Each download is checked against the index (size, SHA-256) and against
//!   its own signature before anything is written. Older versions only with
//!   the `ALLOW_DOWNGRADE` flag.
//!
//! Handles, in manifest order: SYSTEM, network, block service, the main
//! file space (`install`), hardware.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_ab::{schedule, BootState, Image, Slot, PART_STATE, PART_UPDATE};
use aricode_crypto::sha256::sha256;
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::net::NetClient;
use aricode_rt::sys::{self, Handle};
use aricode_update_index::{
    self as upd, best_app, best_system, check_fresh, compare, stored, updater_arg, Change, Index,
    Sources,
};

const SYSTEM: Handle = Handle(0x0001_0004);
const NET: Handle = Handle(0x0001_0005);
const BLOCK: Handle = Handle(0x0001_0006);
const ROOT: Handle = Handle(0x0001_0007);
const HW: Handle = Handle(0x0001_0008);

const PUBKEY_HEX: &str = include_str!("../../../keys/dev/ed25519.pub");
/// DMA buffer where a whole slot image (header + container) is loaded and
/// verified before it is written anywhere.
const IMAGE_BUFFER: u64 = 4 * 1024 * 1024;
const SECTOR: usize = 512;
const CHUNK: u16 = 16;
const HTTP_TIMEOUT_MS: u32 = 15_000;
/// Largest package: an installed package is replaced in one go (the AriFS
/// journal, ADR 0018).
const MAX_PACKAGE: usize = 32 * 1024;
/// Longest AriFS name (`pkgs/NAME.aripkg` must fit).
const MAX_FS_NAME: usize = 32;

/// Exit codes; failures are also fault events with the same code (ADR 0028).
mod code {
    pub const NO_BLOCK: u64 = 2;
    pub const KEY: u64 = 3;
    pub const NO_UPDATE_PART: u64 = 4;
    pub const NO_STATE_PART: u64 = 5;
    pub const STATE_READ: u64 = 6;
    pub const NO_SLOTS: u64 = 7;
    pub const SLOT_TOO_SMALL: u64 = 10;
    pub const NO_MEMORY: u64 = 11;
    pub const UPDATE_INVALID: u64 = 13;
    pub const SLOT_WRITE: u64 = 14;
    pub const STATE_WRITE: u64 = 15;
    pub const STATE_READBACK: u64 = 17;
    pub const NO_SERVICES: u64 = 20;
    pub const NO_SOURCES: u64 = 21;
    pub const SOURCES_FILE: u64 = 22;
    pub const INDEX_FETCH: u64 = 30;
    pub const INDEX_REFUSED: u64 = 31;
    pub const INDEX_STALE: u64 = 32;
    pub const NO_INDEX: u64 = 33;
    pub const INDEX_STORE: u64 = 34;
    pub const DOWNGRADE_REFUSED: u64 = 40;
    pub const DOWNLOAD: u64 = 41;
    pub const DOWNLOAD_MISMATCH: u64 = 42;
    pub const IMAGE_REFUSED: u64 = 43;
    pub const PACKAGE_REFUSED: u64 = 44;
    pub const STAGE_WRITE: u64 = 45;
    pub const BAD_ARGUMENT: u64 = 50;
}

/// Everything printed, kept for `last.log` (the shell shows it to a remote
/// session, whose user cannot see the console).
struct Log(core::cell::RefCell<Vec<u8>>);
// SAFETY: the updater is single-threaded.
unsafe impl Sync for Log {}
static LOG: Log = Log(core::cell::RefCell::new(Vec::new()));
const LOG_MAX: usize = 4096;

fn out(msg: &[u8]) {
    sys::print(msg);
    let mut log = LOG.0.borrow_mut();
    let room = LOG_MAX.saturating_sub(log.len());
    log.extend_from_slice(&msg[..msg.len().min(room)]);
}

macro_rules! say {
    ($($arg:tt)*) => {
        out(format!("updater: {}\n", format_args!($($arg)*)).as_bytes())
    };
}

/// A failure or a refusal: said on the console and recorded as a fault
/// event in the kernel ring, where `diag` shows it (ADR 0028). Returns the
/// code so callers can `return fault(...)`.
fn fault(code: u64, event: &str, msg: &str) -> u64 {
    say!("{msg}");
    let _ = sys::event_record(code, event);
    code
}

// ---------------------------------------------------------------- files

/// Reads a whole file of at most `max` bytes. `Ok(None)` if it does not
/// exist; any other failure is an error (never mistaken for "missing").
fn read_file(fs: &mut FsClient, name: &str, max: usize) -> Result<Option<Vec<u8>>, ()> {
    let fd = match fs.open(name) {
        Ok(fd) => fd,
        Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut data = Vec::new();
    let mut chunk = [0u8; 1024];
    let result = loop {
        match fs.read(fd, data.len() as u32, &mut chunk) {
            Ok(0) => break Ok(Some(data)),
            Ok(n) if data.len() + n <= max => data.extend_from_slice(&chunk[..n]),
            Ok(_) | Err(_) => break Err(()),
        }
    };
    let _ = fs.close(fd);
    result
}

/// Replaces (or creates) a file atomically (ADR 0014).
fn write_file(fs: &mut FsClient, name: &str, data: &[u8]) -> bool {
    let Ok(fd) = fs.open(name).or_else(|_| fs.create(name)) else {
        return false;
    };
    let ok = fs.replace(fd, data) == Ok(data.len());
    let _ = fs.close(fd);
    ok
}

/// One line in the install history (`updates.log`).
fn history(fs: &mut FsClient, line: &str) {
    let existing = read_file(fs, upd::HISTORY_FILE, upd::HISTORY_MAX)
        .ok()
        .flatten()
        .unwrap_or_default();
    let log = upd::history::append(&existing, line, upd::HISTORY_MAX);
    if !write_file(fs, upd::HISTORY_FILE, &log) {
        let _ = sys::event_record(code::STAGE_WRITE, "upd-history-write");
    }
}

/// The registered sources, migrating `update_url` (ADR 0027) on the way.
fn load_sources(fs: &mut FsClient) -> Result<Sources, u64> {
    let read = |fs: &mut FsClient, name| match read_file(fs, name, 2048) {
        Ok(data) => Ok(data.map(|d| String::from_utf8_lossy(&d).into_owned())),
        Err(()) => Err(fault(
            code::SOURCES_FILE,
            "upd-sources-read",
            "cannot read the source list",
        )),
    };
    let text = read(fs, upd::SOURCES_FILE)?;
    if text.is_none() {
        let legacy = read(fs, upd::LEGACY_URL_FILE)?;
        if let Some(sources) = Sources::migrate(None, legacy.as_deref()) {
            if !write_file(fs, upd::SOURCES_FILE, sources.to_text().as_bytes()) {
                return Err(fault(
                    code::SOURCES_FILE,
                    "upd-sources-write",
                    "cannot save the migrated source list",
                ));
            }
            say!("update_url migrated to the source list (ADR 0029)");
            return Ok(sources);
        }
    }
    let (sources, rejected) = Sources::parse(text.as_deref().unwrap_or(""));
    if rejected > 0 {
        fault(
            code::SOURCES_FILE,
            "upd-sources-damaged",
            &format!("{rejected} invalid line(s) in the source list ignored"),
        );
    }
    Ok(sources)
}

// ---------------------------------------------------------------- disk

fn find_partition(blk: &mut BlockClient, name: &str) -> Option<(u64, u64)> {
    blk.read(1, 1).ok()?;
    let header = aricode_gpt::Header::parse(&blk.buffer()[..SECTOR]).ok()?;
    let sectors = header.entries_bytes().div_ceil(SECTOR) as u64;
    let mut table = alloc::vec![0u8; sectors as usize * SECTOR];
    let mut lba = 0;
    while lba < sectors {
        let n = (sectors - lba).min(CHUNK as u64) as u16;
        blk.read(header.entries_lba + lba, n).ok()?;
        let o = lba as usize * SECTOR;
        table[o..o + n as usize * SECTOR].copy_from_slice(&blk.buffer()[..n as usize * SECTOR]);
        lba += n as u64;
    }
    let mut parts = [None; 16];
    let count = aricode_gpt::partitions(&header, &table, &mut parts).ok()?;
    parts[..count]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| (p.first_lba, p.last_lba))
}

/// The A/B layout of the boot disk and a buffer for one whole image.
struct Disk {
    blk: BlockClient,
    update: (u64, u64),
    state_part: (u64, u64),
    state: BootState,
    active_part: (u64, u64),
    target: Slot,
    slot_part: (u64, u64),
    image: &'static mut [u8],
}

impl Disk {
    fn open() -> Result<Self, u64> {
        let Ok(mut blk) = BlockClient::new(BLOCK, HW, CHUNK) else {
            say!("need the block service (block capability)");
            return Err(code::NO_BLOCK);
        };
        let Some(update) = find_partition(&mut blk, PART_UPDATE) else {
            say!("no UPDATE partition");
            return Err(code::NO_UPDATE_PART);
        };
        let Some(state_part) = find_partition(&mut blk, PART_STATE) else {
            say!("no ARIBOOT partition");
            return Err(code::NO_STATE_PART);
        };
        if blk.read(state_part.0, 1).is_err() {
            return Err(fault(
                code::STATE_READ,
                "upd-state-read",
                "cannot read the boot state",
            ));
        }
        let state = BootState::decode(&blk.buffer()[..SECTOR]).unwrap_or(BootState::fresh(Slot::A));
        let target = state.active.other();
        let (Some(active_part), Some(slot_part)) = (
            find_partition(&mut blk, state.active.partition()),
            find_partition(&mut blk, target.partition()),
        ) else {
            say!("no slot partitions");
            return Err(code::NO_SLOTS);
        };
        let Ok(dma) = sys::dma_alloc(HW, IMAGE_BUFFER) else {
            say!("no memory for the image (quota)");
            return Err(code::NO_MEMORY);
        };
        let Ok(virt) = sys::mmio_map(dma) else {
            return Err(code::NO_MEMORY);
        };
        // SAFETY: our own DMA buffer of IMAGE_BUFFER bytes, mapped RW, used
        // only through this exclusive reference for the life of the process.
        let image =
            unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, IMAGE_BUFFER as usize) };
        Ok(Self {
            blk,
            update,
            state_part,
            state,
            active_part,
            target,
            slot_part,
            image,
        })
    }

    /// Loads the slot image of `part` (header + container) into the buffer
    /// and verifies it: (total bytes, version). `None` if there is no image
    /// or it does not verify.
    fn load(&mut self, part: (u64, u64), pk: &[u8; 32]) -> Option<(usize, u32)> {
        self.blk.read(part.0, 1).ok()?;
        let len = Image::container_len(&self.blk.buffer()[..SECTOR]).ok()? as usize;
        let sectors = (SECTOR + len).div_ceil(SECTOR) as u64;
        if sectors as usize * SECTOR > self.image.len() || sectors > part.1 - part.0 + 1 {
            return None;
        }
        let mut done = 0u64;
        while done < sectors {
            let n = (sectors - done).min(CHUNK as u64) as u16;
            self.blk.read(part.0 + done, n).ok()?;
            let o = done as usize * SECTOR;
            let bytes = n as usize * SECTOR;
            self.image[o..o + bytes].copy_from_slice(&self.blk.buffer()[..bytes]);
            done += n as u64;
        }
        let version = Image::verify(&self.image[SECTOR..SECTOR + len], pk)
            .ok()?
            .manifest
            .version;
        Some((SECTOR + len, version))
    }

    /// Version of the active slot's image, verified.
    fn active_version(&mut self, pk: &[u8; 32]) -> Option<u32> {
        let part = self.active_part;
        self.load(part, pk).map(|(_, v)| v)
    }

    /// Writes the verified image in the buffer (`total` bytes) into the
    /// inactive slot, the header sector last, and schedules the slot as
    /// pending with tries. The caller has verified the image.
    fn install(&mut self, total: usize) -> Result<(), u64> {
        let sectors = total.div_ceil(SECTOR) as u64;
        if sectors > self.slot_part.1 - self.slot_part.0 + 1 {
            return Err(fault(
                code::SLOT_TOO_SMALL,
                "upd-slot-small",
                "image does not fit the slot",
            ));
        }
        self.image[total..sectors as usize * SECTOR].fill(0);
        // Container first, header last: an interrupted copy leaves a slot
        // without a valid header, which AriBoot never boots.
        let mut done = 1u64;
        while done < sectors {
            let n = (sectors - done).min(CHUNK as u64) as u16;
            let o = done as usize * SECTOR;
            let bytes = n as usize * SECTOR;
            self.blk.buffer()[..bytes].copy_from_slice(&self.image[o..o + bytes]);
            if self.blk.write(self.slot_part.0 + done, n).is_err() {
                return Err(fault(
                    code::SLOT_WRITE,
                    "upd-slot-write",
                    "error writing the slot",
                ));
            }
            done += n as u64;
        }
        self.blk.buffer()[..SECTOR].copy_from_slice(&self.image[..SECTOR]);
        if self.blk.write(self.slot_part.0, 1).is_err() || self.blk.flush().is_err() {
            return Err(fault(
                code::SLOT_WRITE,
                "upd-slot-write",
                "error writing the slot",
            ));
        }
        say!("installed into slot {}", self.target.name());
        let next = schedule(self.state, self.target);
        self.blk.buffer()[..SECTOR].copy_from_slice(&next.encode());
        if self.blk.write(self.state_part.0, 1).is_err()
            || self.blk.read(self.state_part.0, 1).is_err()
        {
            return Err(fault(
                code::STATE_WRITE,
                "upd-state-write",
                "error writing the boot state",
            ));
        }
        match BootState::decode(&self.blk.buffer()[..SECTOR]) {
            Some(s) if s.pending == Some((self.target, aricode_ab::DEFAULT_TRIES)) => {
                say!(
                    "state: active {}, pending {} ({} tries); reboot to try it",
                    s.active.name(),
                    self.target.name(),
                    aricode_ab::DEFAULT_TRIES
                );
                Ok(())
            }
            _ => Err(fault(
                code::STATE_READBACK,
                "upd-state-readback",
                "boot state read back does not match",
            )),
        }
    }
}

/// `APPLY`: installs the image of the UPDATE partition (Phase 30).
fn apply(pk: &[u8; 32]) -> u64 {
    let mut disk = match Disk::open() {
        Ok(d) => d,
        Err(code) => return code,
    };
    let update = disk.update;
    let Some((total, version)) = disk.load(update, pk) else {
        return fault(
            code::UPDATE_INVALID,
            "upd-update-invalid",
            "UPDATE does not contain a valid system image (missing, too large, or signature or hashes fail); not installing",
        );
    };
    say!(
        "UPDATE image version {version} verified ({} bytes)",
        total - SECTOR
    );
    match disk.install(total) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

// ---------------------------------------------------------------- network

fn http_error(e: aricode_https::HttpsError) -> &'static str {
    use aricode_https::HttpsError as E;
    match e {
        E::BadUrl => "invalid URL",
        E::Resolve(_) => "cannot resolve the host",
        E::Connect(_) => "cannot connect",
        E::Io(_) => "connection lost",
        E::Tls(_) => "TLS failure (certificate or protocol)",
        E::Malformed => "invalid HTTP response",
        E::TooBig => "response headers too large",
        E::Unsupported => "unsupported HTTP response",
        E::Aborted => "larger than announced",
    }
}

/// Downloads `url` into `out`, which must be filled exactly: the index
/// announced `out.len()` bytes. The sink refuses anything beyond.
fn fetch_exact(net: &mut NetClient, url: &str, out: &mut [u8]) -> Result<(), String> {
    let mut got = 0usize;
    let mut sink = |chunk: &[u8]| {
        if got + chunk.len() > out.len() {
            return false;
        }
        out[got..got + chunk.len()].copy_from_slice(chunk);
        got += chunk.len();
        true
    };
    match aricode_https::get_with(net, SYSTEM, &[], url, HTTP_TIMEOUT_MS, &mut sink) {
        Ok(200) if got == out.len() => Ok(()),
        Ok(200) => Err(format!(
            "{got} bytes received, the index announced {}",
            out.len()
        )),
        Ok(status) => Err(format!("HTTP {status}")),
        Err(e) => Err(String::from(http_error(e))),
    }
}

/// Downloads a small file (index, signature) of at most `max` bytes.
fn fetch_small(net: &mut NetClient, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut buf = alloc::vec![0u8; max];
    match aricode_https::get(net, SYSTEM, &[], url, &mut buf, HTTP_TIMEOUT_MS) {
        Ok(f) if f.status == 200 && !f.truncated => {
            buf.truncate(f.body_len);
            Ok(buf)
        }
        Ok(f) if f.status == 200 => Err(format!("larger than {max} bytes")),
        Ok(f) => Err(format!("HTTP {}", f.status)),
        Err(e) => Err(String::from(http_error(e))),
    }
}

// ---------------------------------------------------------------- indexes

/// The stored indexes (`updater/index-N`), raw, one per slot.
fn read_stored(fs: &mut FsClient) -> Vec<Vec<u8>> {
    (0..upd::MAX_SOURCES)
        .map(|slot| {
            upd::stored_index_file(slot)
                .and_then(|name| read_file(fs, name, upd::MAX_INDEX + 512).ok().flatten())
                .unwrap_or_default()
        })
        .collect()
}

/// The serial of the verified index stored for `url`, if any.
fn stored_serial(stored_files: &[Vec<u8>], url: &str, pk: &[u8; 32]) -> Option<u64> {
    stored_files
        .iter()
        .filter_map(|bytes| stored::decode(bytes))
        .filter(|s| s.source == url)
        .filter_map(|s| upd::verify(s.index, s.signature, pk).ok())
        .map(|index| index.serial)
        .max()
}

/// `REFRESH`: fetch, verify and store every source's index, then report.
fn refresh(fs: &mut FsClient, net: &mut NetClient, sources: &Sources, pk: &[u8; 32]) -> u64 {
    let previous = read_stored(fs);
    let mut next: Vec<Vec<u8>> = alloc::vec![Vec::new(); upd::MAX_SOURCES];
    let mut result = 0;
    for (slot, url) in sources.iter().enumerate() {
        let n = slot + 1;
        let known = stored_serial(&previous, url, pk);
        // On any failure the index verified earlier for this source stays:
        // it is still authentic, and dropping it would forget the serial
        // that protects against a replayed older index.
        let keep = |next: &mut Vec<Vec<u8>>| {
            if let Some(old) = previous.iter().find(|b| {
                stored::decode(b).is_some_and(|s| {
                    s.source == url && upd::verify(s.index, s.signature, pk).is_ok()
                })
            }) {
                next[slot] = old.clone();
                say!("source {n}: keeping the index verified earlier");
            }
        };
        let fetched = fetch_small(net, &format!("{url}{}", upd::INDEX_FILE), upd::MAX_INDEX)
            .and_then(|index| {
                fetch_small(net, &format!("{url}{}", upd::SIGNATURE_FILE), 256)
                    .map(|sig| (index, sig))
            });
        let (index_bytes, sig) = match fetched {
            Ok(v) => v,
            Err(e) => {
                result = fault(
                    code::INDEX_FETCH,
                    "upd-index-fetch",
                    &format!("source {n} {url}: cannot fetch the index: {e}"),
                );
                keep(&mut next);
                continue;
            }
        };
        let index = match upd::verify(&index_bytes, &sig, pk) {
            Ok(index) => index,
            Err(e) => {
                result = fault(
                    code::INDEX_REFUSED,
                    "upd-index-refused",
                    &format!("source {n} {url}: index refused ({e:?}): signature or format invalid; nothing from it is used"),
                );
                keep(&mut next);
                continue;
            }
        };
        if let Err(e) = check_fresh(known, index.serial) {
            result = fault(
                code::INDEX_STALE,
                "upd-index-stale",
                &format!(
                    "source {n} {url}: index refused ({e:?}): older than the one already verified"
                ),
            );
            keep(&mut next);
            continue;
        }
        say!(
            "source {n} {url}: index verified (serial {}, {}, {} app(s))",
            index.serial,
            match &index.system {
                Some(s) => format!("system {}", s.version),
                None => String::from("no system image"),
            },
            index.apps.len()
        );
        next[slot] = stored::encode(url, &sig, &index_bytes);
    }
    for (slot, bytes) in next.iter().enumerate() {
        if *bytes == previous[slot] {
            continue;
        }
        let Some(name) = upd::stored_index_file(slot) else {
            continue;
        };
        if !write_file(fs, name, bytes) {
            result = fault(
                code::INDEX_STORE,
                "upd-index-store",
                "cannot store a verified index",
            );
        }
    }
    let status = report(fs, sources, pk);
    if result != 0 {
        result
    } else {
        status
    }
}

/// The stored indexes of the registered sources, verified again (ADR 0029,
/// decision 4): (raw bytes per source, in list order).
fn verified_stored(fs: &mut FsClient, sources: &Sources, pk: &[u8; 32]) -> Vec<(usize, Vec<u8>)> {
    let files = read_stored(fs);
    let mut list = Vec::new();
    for (slot, url) in sources.iter().enumerate() {
        let Some(s) = files.get(slot).and_then(|b| stored::decode(b)) else {
            say!("source {}: no verified index yet; run update", slot + 1);
            continue;
        };
        if s.source != url {
            say!(
                "source {}: the list changed since the last update; run update",
                slot + 1
            );
            continue;
        }
        if upd::verify(s.index, s.signature, pk).is_err() {
            fault(
                code::INDEX_REFUSED,
                "upd-stored-refused",
                &format!(
                    "source {}: stored index fails verification; ignored",
                    slot + 1
                ),
            );
            continue;
        }
        list.push((slot, files[slot].clone()));
    }
    list
}

/// The indexes inside verified stored files (already checked by
/// `verified_stored`), with the list position of each one's source.
fn parse_all<'a>(raw: &'a [(usize, Vec<u8>)], pk: &[u8; 32]) -> (Vec<usize>, Vec<Index<'a>>) {
    raw.iter()
        .filter_map(|(slot, bytes)| {
            let s = stored::decode(bytes)?;
            upd::verify(s.index, s.signature, pk)
                .ok()
                .map(|index| (*slot, index))
        })
        .unzip()
}

/// Installed apps: `apps/NAME.aripkg` that verify (what the broker and the
/// installer accept), as (name, version).
fn installed_apps(fs: &mut FsClient, pk: &[u8; 32]) -> Vec<(String, u32)> {
    let mut names = Vec::new();
    let mut entry = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, _))) = fs.list(i, &mut entry) {
        i += 1;
        if let Ok(name) = core::str::from_utf8(&entry[..n]) {
            if name.starts_with("apps/") && name.ends_with(aricode_pkg::EXTENSION) {
                names.push(String::from(name));
            }
        }
    }
    let mut apps = Vec::new();
    for path in names {
        let Ok(Some(bytes)) = read_file(fs, &path, MAX_PACKAGE) else {
            say!("{path}: cannot be read; not considered");
            continue;
        };
        match aricode_pkg::verify(&bytes, pk) {
            Ok(v) => apps.push((String::from(v.manifest.name_str()), v.manifest.version)),
            Err(_) => say!("{path}: fails verification; not considered"),
        }
    }
    apps
}

fn change_text(change: Change) -> &'static str {
    match change {
        Change::New => "new",
        Change::Same => "up to date",
        Change::Upgrade => "upgrade available",
        Change::Downgrade => "older than installed; upgrade refuses it without --allow-downgrade",
    }
}

/// What the stored indexes offer versus what is installed; installs nothing.
fn report(fs: &mut FsClient, sources: &Sources, pk: &[u8; 32]) -> u64 {
    let raw = verified_stored(fs, sources, pk);
    let (slots, indexes) = parse_all(&raw, pk);
    let installed_system = match Disk::open() {
        Ok(mut disk) => disk.active_version(pk),
        Err(code) => return code,
    };
    let mut available = 0;
    match best_system(&indexes) {
        Some((i, s)) => {
            let change = compare(installed_system, s.version);
            available += usize::from(matches!(change, Change::Upgrade | Change::New));
            say!(
                "system: installed {}, source {} offers {}: {}",
                installed_system.map_or(String::from("unknown"), |v| format!("{v}")),
                slots[i] + 1,
                s.version,
                change_text(change)
            );
        }
        None => say!("system: no source offers a system image"),
    }
    let apps = installed_apps(fs, pk);
    for (name, version) in &apps {
        match best_app(&indexes, name) {
            Some((i, a)) => {
                let change = compare(Some(*version), a.version);
                available += usize::from(change == Change::Upgrade);
                say!(
                    "app {name}: installed {version}, source {} offers {}: {}",
                    slots[i] + 1,
                    a.version,
                    change_text(change)
                );
            }
            None => say!("app {name}: installed {version}, no source offers it"),
        }
    }
    let mut listed: Vec<&str> = Vec::new();
    for index in &indexes {
        for a in &index.apps {
            if !apps.iter().any(|(n, _)| n == a.name) && !listed.contains(&a.name) {
                listed.push(a.name);
                say!(
                    "app {}: version {} available (not installed)",
                    a.name,
                    a.version
                );
            }
        }
    }
    if indexes.is_empty() {
        say!("no verified index: nothing to compare");
    } else if available == 0 {
        say!("everything is up to date");
    } else {
        say!("{available} upgrade(s) available: run upgrade");
    }
    0
}

// ---------------------------------------------------------------- upgrade

/// The downgrade rule (ADR 0029, decision 5) for one item; `Ok(true)` when
/// it must be installed.
fn decide(
    what: &str,
    installed: Option<u32>,
    offered: u32,
    allow_downgrade: bool,
) -> Result<bool, u64> {
    let change = compare(installed, offered);
    if change.permitted(allow_downgrade) {
        if change == Change::Downgrade {
            say!(
                "{what}: downgrading from {} to {offered} (--allow-downgrade)",
                installed.unwrap_or(0)
            );
        }
        return Ok(true);
    }
    match change {
        Change::Downgrade => Err(fault(
            code::DOWNGRADE_REFUSED,
            "upd-downgrade-refused",
            &format!(
                "{what}: refused: version {offered} is older than the installed {} (upgrade --allow-downgrade to force)",
                installed.unwrap_or(0)
            ),
        )),
        _ => {
            say!("{what}: version {offered} already installed");
            Ok(false)
        }
    }
}

/// Downloads, checks and installs the system image `entry` from `url`.
fn upgrade_system(
    fs: &mut FsClient,
    net: &mut NetClient,
    disk: &mut Disk,
    url: &str,
    entry: &upd::SystemEntry,
    installed: Option<u32>,
    pk: &[u8; 32],
) -> Result<(), u64> {
    let size = entry.size as usize;
    if size <= SECTOR || size > disk.image.len() {
        return Err(fault(
            code::IMAGE_REFUSED,
            "upd-image-size",
            &format!(
                "system image {}: {size} bytes does not fit the image buffer",
                entry.file
            ),
        ));
    }
    say!("downloading {} ({size} bytes)", entry.file);
    // Into memory only: nothing touches the disk until every check passed.
    if let Err(e) = fetch_exact(
        net,
        &format!("{url}{}", entry.file),
        &mut disk.image[..size],
    ) {
        return Err(fault(
            code::DOWNLOAD,
            "upd-image-download",
            &format!("download failed: {e}"),
        ));
    }
    if sha256(&disk.image[..size]) != entry.sha256 {
        return Err(fault(
            code::DOWNLOAD_MISMATCH,
            "upd-image-sha256",
            "system image SHA-256 does not match the signed index; not installing",
        ));
    }
    // Its own signature, verified again as AriBoot will (Phase 30): the
    // index authenticates the file, the manifest authenticates the content.
    let version = Image::container_len(&disk.image[..SECTOR])
        .ok()
        .filter(|&len| SECTOR + len as usize == size)
        .and_then(|_| Image::verify(&disk.image[SECTOR..size], pk).ok())
        .map(|v| v.manifest.version);
    match version {
        Some(v) if v == entry.version => {}
        Some(v) => {
            return Err(fault(
                code::IMAGE_REFUSED,
                "upd-image-version",
                &format!(
                    "system image says version {v}, the index says {}; not installing",
                    entry.version
                ),
            ))
        }
        None => {
            return Err(fault(
                code::IMAGE_REFUSED,
                "upd-image-signature",
                "system image signature or hashes invalid; not installing",
            ))
        }
    }
    say!(
        "system image version {} verified (signature and SHA-256)",
        entry.version
    );
    disk.install(size)?;
    history(
        fs,
        &format!(
            "system {} -> {} installed into slot {} from {url} (pending until reboot)",
            installed.map_or(String::from("?"), |v| format!("{v}")),
            entry.version,
            disk.target.name()
        ),
    );
    say!("system {} installed: reboot to apply", entry.version);
    Ok(())
}

/// Downloads and checks the package `entry` and leaves it in
/// `pkgs/NAME.aripkg` for the installer, which verifies it again.
fn stage_app(
    fs: &mut FsClient,
    net: &mut NetClient,
    url: &str,
    entry: &upd::AppEntry,
    pk: &[u8; 32],
) -> Result<(), u64> {
    let path = format!("pkgs/{}{}", entry.name, aricode_pkg::EXTENSION);
    let size = entry.size as usize;
    if size > MAX_PACKAGE || path.len() > MAX_FS_NAME {
        return Err(fault(
            code::PACKAGE_REFUSED,
            "upd-pkg-size",
            &format!(
                "app {}: package too large or name too long; skipped",
                entry.name
            ),
        ));
    }
    say!("downloading {} ({size} bytes)", entry.file);
    let mut bytes = alloc::vec![0u8; size];
    if let Err(e) = fetch_exact(net, &format!("{url}{}", entry.file), &mut bytes) {
        return Err(fault(
            code::DOWNLOAD,
            "upd-pkg-download",
            &format!("app {}: download failed: {e}", entry.name),
        ));
    }
    if sha256(&bytes) != entry.sha256 {
        return Err(fault(
            code::DOWNLOAD_MISMATCH,
            "upd-pkg-sha256",
            &format!(
                "app {}: SHA-256 does not match the signed index; skipped",
                entry.name
            ),
        ));
    }
    match aricode_pkg::verify(&bytes, pk) {
        Ok(v) if v.manifest.name_str() == entry.name && v.manifest.version == entry.version => {}
        Ok(_) => {
            return Err(fault(
                code::PACKAGE_REFUSED,
                "upd-pkg-mismatch",
                &format!(
                    "app {}: package name or version differs from the index; skipped",
                    entry.name
                ),
            ))
        }
        Err(_) => {
            return Err(fault(
                code::PACKAGE_REFUSED,
                "upd-pkg-signature",
                &format!(
                    "app {}: package signature or hashes invalid; skipped",
                    entry.name
                ),
            ))
        }
    }
    if !write_file(fs, &path, &bytes) {
        return Err(fault(
            code::STAGE_WRITE,
            "upd-pkg-stage",
            &format!("cannot write {path}"),
        ));
    }
    say!(
        "app {} version {} verified and staged in {path}",
        entry.name,
        entry.version
    );
    Ok(())
}

/// `UPGRADE`: installs what the verified stored indexes offer.
fn upgrade(
    fs: &mut FsClient,
    net: &mut NetClient,
    sources: &Sources,
    allow_downgrade: bool,
    pk: &[u8; 32],
) -> u64 {
    let raw = verified_stored(fs, sources, pk);
    let (slots, indexes) = parse_all(&raw, pk);
    if indexes.is_empty() {
        return fault(
            code::NO_INDEX,
            "upd-no-index",
            "no verified index: run update first",
        );
    }
    let urls: Vec<&str> = sources.iter().collect();
    let mut result = 0;
    let mut staged = 0;
    let mut disk = match Disk::open() {
        Ok(d) => d,
        Err(code) => return code,
    };
    let installed = disk.active_version(pk);
    if let Some((i, entry)) = best_system(&indexes) {
        let url = urls[slots[i]];
        match decide("system", installed, entry.version, allow_downgrade) {
            Ok(true) => {
                if let Err(code) = upgrade_system(fs, net, &mut disk, url, entry, installed, pk) {
                    result = code;
                }
            }
            Ok(false) => {}
            Err(code) => result = code,
        }
    }
    for (name, version) in installed_apps(fs, pk) {
        let Some((i, entry)) = best_app(&indexes, &name) else {
            continue;
        };
        let url = urls[slots[i]];
        match decide(
            &format!("app {name}"),
            Some(version),
            entry.version,
            allow_downgrade,
        ) {
            Ok(true) => match stage_app(fs, net, url, entry, pk) {
                Ok(()) => staged += 1,
                Err(code) => result = code,
            },
            Ok(false) => {}
            Err(code) => result = code,
        }
    }
    if staged > 0 {
        say!("{staged} package(s) staged for the installer");
        result | updater_arg::EXIT_APPS_STAGED
    } else {
        result
    }
}

fn run(arg: u64) -> u64 {
    let mut pk = [0u8; 32];
    if aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pk).is_none() {
        return code::KEY;
    }
    let allow_downgrade = arg & updater_arg::ALLOW_DOWNGRADE != 0;
    let command = arg & !updater_arg::ALLOW_DOWNGRADE;
    match (command, allow_downgrade) {
        (updater_arg::APPLY, false) => return apply(&pk),
        (updater_arg::REFRESH, false) | (updater_arg::UPGRADE, _) => {}
        _ => {
            return fault(
                code::BAD_ARGUMENT,
                "upd-bad-argument",
                &format!("unknown argument {arg}"),
            )
        }
    }
    let (Ok(mut fs), Ok(mut net)) = (FsClient::new(ROOT, HW), NetClient::new(NET, HW)) else {
        say!("need the file and network services");
        return code::NO_SERVICES;
    };
    let sources = match load_sources(&mut fs) {
        Ok(s) => s,
        Err(code) => return code,
    };
    if sources.is_empty() {
        say!("no update sources: update add URL");
        return code::NO_SOURCES;
    }
    if command == updater_arg::REFRESH {
        refresh(&mut fs, &mut net, &sources, &pk)
    } else {
        upgrade(&mut fs, &mut net, &sources, allow_downgrade, &pk)
    }
}

fn main(arg: u64) -> u64 {
    let code = run(arg);
    // The log for the shell (private directory); best effort.
    if let Ok(mut fs) = FsClient::new(ROOT, HW) {
        let log = LOG.0.borrow();
        if !write_file(&mut fs, upd::UPDATER_LOG_FILE, &log) {
            let _ = sys::event_record(code::STAGE_WRITE, "upd-log-write");
        }
    }
    code
}

aricode_rt::entry!(main);
