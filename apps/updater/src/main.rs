// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `updater` (Phase 30, ADR 0027, ADR 0029): the system's update client.
//!
//! Argument (`aricode_update_index::updater_arg`):
//!
//! - `APPLY` (0, shell `update apply`): installs the image the `UPDATE`
//!   partition holds into the inactive slot and marks it pending with three
//!   tries, under the downgrade rule.
//! - `REFRESH` (1, shell `update`): fetches `index` and `index.sig` from
//!   every registered source, verifies them with the project key, stores
//!   the verified copies, reports what differs from what is installed and
//!   writes that report as the plan. Installs nothing.
//! - `UPGRADE` (2, shell `upgrade`): verifies the stored indexes again and
//!   installs what the plan lists: the system image into the inactive slot,
//!   app packages staged in this directory for the installer. Every
//!   download is checked against the index (size, SHA-256) and against its
//!   own signature before anything is written.
//! - `ALLOW_DOWNGRADE` (flag, with `APPLY` or `UPGRADE`): older versions, or
//!   a system whose installed version cannot be read, may be replaced.
//!
//! Handles, in manifest order: SYSTEM, files (the private directory
//! `updater/`), network, block service, hardware. The main file space is
//! not granted (audit 1, M2): installed apps arrive in `installed`, written
//! by the installer, and staged packages leave through `staged`.
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
use aricode_update_index::lists::{self, PlanItem, StagedEntry};
use aricode_update_index::{
    self as upd, best_app, best_system, check_fresh, compare, stored, updater_arg, Change, Index,
    Migration, Sources,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SYSTEM: Handle = Handle(0x0001_0004);
const FS: Handle = Handle(0x0001_0005);
const NET: Handle = Handle(0x0001_0006);
const BLOCK: Handle = Handle(0x0001_0007);
const HW: Handle = Handle(0x0001_0008);

/// DMA buffer where a whole slot image (header + container) is loaded and
/// verified before it is written anywhere.
const IMAGE_BUFFER: u64 = 4 * 1024 * 1024;
const SECTOR: usize = 512;
const CHUNK: u16 = 16;
/// Wait for one network operation.
const HTTP_TIMEOUT_MS: u64 = 15_000;
/// Budget of a whole run: a source that trickles bytes cannot hold the
/// updater forever (audit 1, M3).
const RUN_DEADLINE_MS: u64 = 10 * 60 * 1000;
/// Largest package: an installed package is replaced in one go (the AriFS
/// journal, ADR 0018).
const MAX_PACKAGE: usize = 32 * 1024;
/// Largest exchange file with the installer (`state`).
const MAX_STATE: usize = 16 * 1024;
/// Largest `index.sig` (128 hex digits and a newline, with room).
const MAX_SIGNATURE: usize = 256;

/// Exit codes; failures are also fault events with the same code (ADR 0028).
mod code {
    pub const NO_BLOCK: u64 = 2;
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
    pub const INDEX_MISSING: u64 = 35;
    pub const INSTALLED_LIST: u64 = 36;
    pub const PLAN: u64 = 37;
    pub const STATE_UNREADABLE: u64 = 38;
    pub const FS_FULL: u64 = 39;
    pub const STATE_REPAIRED: u64 = 47;
    pub const DOWNGRADE_REFUSED: u64 = 40;
    pub const DOWNLOAD: u64 = 41;
    pub const DOWNLOAD_MISMATCH: u64 = 42;
    pub const IMAGE_REFUSED: u64 = 43;
    pub const PACKAGE_REFUSED: u64 = 44;
    pub const FILE_WRITE: u64 = 45;
    pub const INSTALLED_UNKNOWN: u64 = 46;
    pub const BAD_ARGUMENT: u64 = 50;
    pub const DEADLINE: u64 = 51;
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

// ---------------------------------------------------------------- deadline

/// End of the run's budget, in ms since boot.
static DEADLINE: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    sys::time::uptime_ms(SYSTEM).unwrap_or(u64::MAX)
}

/// Time left for this run (0 once the deadline passed).
fn remaining_ms() -> u64 {
    DEADLINE.load(Ordering::Relaxed).saturating_sub(now_ms())
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

fn read_text(fs: &mut FsClient, name: &str, max: usize) -> Result<Option<String>, ()> {
    read_file(fs, name, max).map(|d| d.map(|d| String::from_utf8_lossy(&d).into_owned()))
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

/// Makes sure every file in `names` exists, creating the missing ones
/// empty, so later writes cannot fail for lack of an AriFS entry. `Err`
/// with the number of files that could not be created.
fn reserve(fs: &mut FsClient, names: &[&str]) -> Result<(), usize> {
    let mut missing = 0;
    for name in names {
        match fs.open(name) {
            Ok(fd) => {
                let _ = fs.close(fd);
            }
            Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => match fs.create(name) {
                Ok(fd) => {
                    let _ = fs.close(fd);
                }
                Err(_) => missing += 1,
            },
            Err(_) => missing += 1,
        }
    }
    if missing == 0 {
        Ok(())
    } else {
        Err(missing)
    }
}

/// One line in the install history (ADR 0029, decision 7).
fn history(fs: &mut FsClient, line: &str) {
    let existing = read_file(fs, upd::HISTORY_FILE, upd::HISTORY_MAX)
        .ok()
        .flatten()
        .unwrap_or_default();
    let log = upd::history::append(&existing, line, upd::HISTORY_MAX);
    if !write_file(fs, upd::HISTORY_FILE, &log) {
        fault(
            code::FILE_WRITE,
            "upd-history-write",
            "cannot write the install history",
        );
    }
}

/// The registered sources, migrating `update_url` (ADR 0027) on the way.
fn load_sources(fs: &mut FsClient) -> Result<Sources, u64> {
    let unreadable = |_| {
        fault(
            code::SOURCES_FILE,
            "upd-sources-read",
            "cannot read the source list",
        )
    };
    let text = read_text(fs, upd::SOURCES_FILE, 2048).map_err(unreadable)?;
    if text.is_none() {
        let legacy = read_text(fs, upd::LEGACY_URL_FILE, 2048).map_err(unreadable)?;
        match Sources::migrate(None, legacy.as_deref()) {
            Migration::Migrated(sources) => {
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
            Migration::InvalidLegacy(e) => {
                fault(
                    code::SOURCES_FILE,
                    "upd-legacy-url-invalid",
                    &format!("update_url is not a valid source ({e:?}): refused, not migrated"),
                );
            }
            Migration::NotNeeded => {}
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

/// The exchange file with the installer (`state`); empty when missing. A
/// read error or an oversized file is a fault, and then the file is
/// neither used nor rewritten in this run (final re-audit, L-a): its other
/// sections are never reset silently.
fn read_state(fs: &mut FsClient) -> Result<String, u64> {
    let text = read_text(fs, upd::STATE_FILE, MAX_STATE)
        .map(Option::unwrap_or_default)
        .map_err(|()| {
            fault(
                code::STATE_UNREADABLE,
                "upd-state-unreadable",
                "updater/state cannot be read or is too large: not used, not rewritten",
            )
        })?;
    // A damaged file (repeated or unknown sections) is read without
    // misattributing lines and rewritten canonically by the next write;
    // the repair is recorded once per run (fix update-state-sections).
    if lists::needs_repair(&text) && !STATE_REPAIR_RECORDED.swap(true, Ordering::Relaxed) {
        fault(
            code::STATE_REPAIRED,
            "upd-state-repaired",
            "updater/state was damaged (repeated or unknown sections): recovered and rewritten canonically",
        );
    }
    Ok(text)
}

/// The state repair fault is recorded once per run.
static STATE_REPAIR_RECORDED: AtomicBool = AtomicBool::new(false);

/// Replaces one section of `state`; refused when it could not be read.
fn write_state_section(fs: &mut FsClient, name: &str, body: &str) -> bool {
    let Ok(state) = read_state(fs) else {
        return false;
    };
    write_file(
        fs,
        upd::STATE_FILE,
        lists::with_section(&state, name, body).as_bytes(),
    )
}

/// The installed apps, as the installer listed them just before this run.
/// `None` (after a fault) when the list is missing or unreadable.
fn installed_apps(fs: &mut FsClient) -> Option<Vec<(String, u32)>> {
    let state = read_state(fs).ok()?;
    let text = match lists::section(&state, lists::INSTALLED) {
        Some(t) => t,
        None => {
            fault(
                code::INSTALLED_LIST,
                "upd-installed-missing",
                "no installed app list (the installer writes it before update and upgrade): apps not compared",
            );
            return None;
        }
    };
    let (apps, rejected) = lists::parse_installed(&text);
    if rejected > 0 {
        fault(
            code::INSTALLED_LIST,
            "upd-installed-damaged",
            &format!("{rejected} invalid line(s) in the installed app list ignored"),
        );
    }
    Some(
        apps.into_iter()
            .map(|(n, v)| (String::from(n), v))
            .collect(),
    )
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

/// The A/B layout of the boot disk and a buffer for one whole image. Only
/// opened when a system image is involved (audit 1, M5).
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
        Self::open_with(true)
    }

    /// The disk if this machine has the A/B layout; nothing said or
    /// recorded when it has not (final re-audit, L-b).
    fn probe() -> Option<Self> {
        Self::open_with(false).ok()
    }

    /// `report`: whether failures are faults (`open`) or a silent `None`
    /// (`probe`).
    fn open_with(report: bool) -> Result<Self, u64> {
        let fault = |code: u64, event: &str, msg: &str| {
            if report {
                fault(code, event, msg)
            } else {
                code
            }
        };
        let Ok(mut blk) = BlockClient::new(BLOCK, HW, CHUNK) else {
            return Err(fault(
                code::NO_BLOCK,
                "upd-no-block",
                "need the block service (block capability)",
            ));
        };
        let Some(update) = find_partition(&mut blk, PART_UPDATE) else {
            return Err(fault(
                code::NO_UPDATE_PART,
                "upd-no-update-part",
                "no UPDATE partition",
            ));
        };
        let Some(state_part) = find_partition(&mut blk, PART_STATE) else {
            return Err(fault(
                code::NO_STATE_PART,
                "upd-no-state-part",
                "no ARIBOOT partition",
            ));
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
            return Err(fault(code::NO_SLOTS, "upd-no-slots", "no slot partitions"));
        };
        let Ok(dma) = sys::dma_alloc(HW, IMAGE_BUFFER) else {
            return Err(fault(
                code::NO_MEMORY,
                "upd-no-memory",
                "no memory for the image (quota)",
            ));
        };
        let Ok(virt) = sys::mmio_map(dma) else {
            return Err(fault(
                code::NO_MEMORY,
                "upd-no-memory",
                "cannot map the image buffer",
            ));
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

    /// Version of the active slot's image, verified; `None` if it cannot be
    /// read or does not verify (then the downgrade rule treats it as
    /// unknown, ADR 0029, decision 5).
    fn active_version(&mut self, pk: &[u8; 32]) -> Option<u32> {
        let part = self.active_part;
        self.load(part, pk).map(|(_, v)| v)
    }

    fn write(&mut self, lba: u64, sectors: u16) -> Result<(), u64> {
        self.blk
            .write(lba, sectors)
            .map_err(|_| fault(code::SLOT_WRITE, "upd-slot-write", "error writing the slot"))
    }

    /// Writes the verified image in the buffer (`total` bytes) into the
    /// inactive slot and schedules it as pending with tries. The caller has
    /// verified the image.
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
        // The old header goes first (audit 1, L1): an interrupted copy then
        // leaves a slot without a valid header, which AriBoot never boots,
        // instead of an old header over a half-new container.
        self.blk.buffer()[..SECTOR].fill(0);
        self.write(self.slot_part.0, 1)?;
        if self.blk.flush().is_err() {
            return Err(fault(
                code::SLOT_WRITE,
                "upd-slot-write",
                "error flushing the slot",
            ));
        }
        let mut done = 1u64;
        while done < sectors {
            let n = (sectors - done).min(CHUNK as u64) as u16;
            let o = done as usize * SECTOR;
            let bytes = n as usize * SECTOR;
            self.blk.buffer()[..bytes].copy_from_slice(&self.image[o..o + bytes]);
            self.write(self.slot_part.0 + done, n)?;
            done += n as u64;
        }
        self.blk.buffer()[..SECTOR].copy_from_slice(&self.image[..SECTOR]);
        self.write(self.slot_part.0, 1)?;
        if self.blk.flush().is_err() {
            return Err(fault(
                code::SLOT_WRITE,
                "upd-slot-write",
                "error flushing the slot",
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

// ---------------------------------------------------------------- policy

/// The downgrade rule (ADR 0029, decision 5) for one item; `Ok(true)` when
/// it must be installed. An unknown installed system version counts as a
/// downgrade (audit 1, H1).
fn decide(
    what: &str,
    installed: Option<u32>,
    offered: u32,
    allow_downgrade: bool,
) -> Result<bool, u64> {
    let Some(current) = installed else {
        if allow_downgrade {
            say!("{what}: installed version unknown; installing {offered} (--allow-downgrade)");
            return Ok(true);
        }
        return Err(fault(
            code::INSTALLED_UNKNOWN,
            "upd-installed-unknown",
            &format!(
                "{what}: installed version unknown (unreadable or unverifiable): refused (--allow-downgrade to force)"
            ),
        ));
    };
    let change = compare(Some(current), offered);
    if change.permitted(allow_downgrade) {
        if change == Change::Downgrade {
            say!("{what}: downgrading from {current} to {offered} (--allow-downgrade)");
        }
        return Ok(true);
    }
    match change {
        Change::Downgrade => Err(fault(
            code::DOWNGRADE_REFUSED,
            "upd-downgrade-refused",
            &format!(
                "{what}: refused: version {offered} is older than the installed {current} (--allow-downgrade to force)"
            ),
        )),
        _ => {
            say!("{what}: version {offered} already installed");
            Ok(false)
        }
    }
}

/// `APPLY`: installs the image of the UPDATE partition (Phase 30), under the
/// downgrade rule (audit 1, M4).
fn apply(allow_downgrade: bool, pk: &[u8; 32]) -> u64 {
    let mut disk = match Disk::open() {
        Ok(d) => d,
        Err(code) => return code,
    };
    let installed = disk.active_version(pk);
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
    match decide("system", installed, version, allow_downgrade) {
        Ok(true) => match disk.install(total) {
            Ok(()) => 0,
            Err(code) => code,
        },
        Ok(false) => 0,
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
        E::Aborted => "aborted",
    }
}

/// Downloads `url` into `out`, never more than `out.len()` bytes and never
/// past the run's deadline (audit 1, M3): the sink refuses both. Returns
/// the bytes received.
fn fetch(net: &mut NetClient, url: &str, out: &mut [u8]) -> Result<usize, String> {
    let left = remaining_ms();
    if left == 0 {
        return Err(String::from("run deadline passed"));
    }
    let (mut got, mut overflow, mut late) = (0usize, false, false);
    let mut sink = |chunk: &[u8]| {
        if got + chunk.len() > out.len() {
            overflow = true;
            return false;
        }
        if remaining_ms() == 0 {
            late = true;
            return false;
        }
        out[got..got + chunk.len()].copy_from_slice(chunk);
        got += chunk.len();
        true
    };
    let timeout = left.min(HTTP_TIMEOUT_MS) as u32;
    let result = aricode_https::get_with(net, SYSTEM, &[], url, timeout, &mut sink);
    match result {
        Ok(200) => Ok(got),
        Ok(status) => Err(format!("HTTP {status}")),
        Err(_) if overflow => Err(format!("larger than {} bytes", out.len())),
        Err(_) if late => Err(String::from("run deadline passed")),
        Err(e) => Err(String::from(http_error(e))),
    }
}

/// `fetch` of exactly `out.len()` bytes (what the index announced).
fn fetch_exact(net: &mut NetClient, url: &str, out: &mut [u8]) -> Result<(), String> {
    let want = out.len();
    match fetch(net, url, out)? {
        n if n == want => Ok(()),
        n => Err(format!("{n} bytes received, the index announced {want}")),
    }
}

/// `fetch` of a small file of at most `max` bytes.
fn fetch_small(net: &mut NetClient, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut buf = alloc::vec![0u8; max];
    let n = fetch(net, url, &mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// The fault of a failed download: the deadline has its own event.
fn download_fault(code: u64, event: &str, msg: String) -> u64 {
    if remaining_ms() == 0 {
        fault(
            code::DEADLINE,
            "upd-deadline",
            &format!("{msg} (run deadline)"),
        )
    } else {
        fault(code, event, &msg)
    }
}

// ---------------------------------------------------------------- indexes

/// Set when `indexes` could not be read in this run: it is then never
/// rewritten, so the stored serials (rollback protection) are not forgotten
/// silently (final re-audit, L-a).
static INDEXES_UNREADABLE: AtomicBool = AtomicBool::new(false);

/// The stored indexes (`indexes`), raw, one per source slot.
fn read_stored(fs: &mut FsClient) -> Vec<Vec<u8>> {
    let max = upd::MAX_SOURCES * (upd::MAX_INDEX + 1024);
    let bytes = match read_file(fs, upd::INDEXES_FILE, max) {
        Ok(b) => b.unwrap_or_default(),
        Err(()) => {
            INDEXES_UNREADABLE.store(true, Ordering::Relaxed);
            fault(
                code::INDEX_STORE,
                "upd-indexes-unreadable",
                "the stored indexes cannot be read or are too large: not used, not rewritten in this run",
            );
            Vec::new()
        }
    };
    stored::decode_all(&bytes).unwrap_or_else(|| {
        fault(
            code::INDEX_STORE,
            "upd-indexes-damaged",
            "the stored indexes are damaged: ignored; run update",
        );
        alloc::vec![Vec::new(); upd::MAX_SOURCES]
    })
}

/// Saves the stored copies (one file for every source, audit 1 R3).
fn write_stored(fs: &mut FsClient, slots: &[Vec<u8>]) -> bool {
    if INDEXES_UNREADABLE.load(Ordering::Relaxed) {
        return false;
    }
    let bytes = if slots.iter().all(Vec::is_empty) {
        Vec::new()
    } else {
        stored::encode_all(slots)
    };
    write_file(fs, upd::INDEXES_FILE, &bytes)
}

/// With no source left, `update` drops every stored copy: a source added
/// again starts from its current serial (ADR 0029, decision 6).
fn forget_stored(fs: &mut FsClient) {
    if read_stored(fs).iter().any(|b| !b.is_empty()) && !write_stored(fs, &[]) {
        fault(
            code::INDEX_STORE,
            "upd-index-store",
            "cannot drop the stored indexes",
        );
    }
}

/// A stored file holding a verified index for `url`.
fn verified_for<'a>(bytes: &'a [u8], url: &str, pk: &[u8; 32]) -> Option<Index<'a>> {
    stored::decode(bytes)
        .filter(|s| s.source == url)
        .and_then(|s| upd::verify(s.index, s.signature, pk).ok())
}

/// `REFRESH`: fetch, verify and store every source's index, then report.
fn refresh(fs: &mut FsClient, net: &mut NetClient, sources: &Sources, pk: &[u8; 32]) -> u64 {
    let previous = read_stored(fs);
    let mut next: Vec<Vec<u8>> = alloc::vec![Vec::new(); upd::MAX_SOURCES];
    let mut result = 0;
    for (slot, url) in sources.iter().enumerate() {
        let n = slot + 1;
        // The newest verified copy already stored for this URL, wherever it is.
        let earlier = previous
            .iter()
            .filter_map(|b| verified_for(b, url, pk).map(|i| (i.serial, b)))
            .max_by_key(|(serial, _)| *serial);
        // On any failure that copy stays: it is still authentic, and
        // dropping it would forget the serial that protects against a
        // replayed older index.
        let keep = |next: &mut Vec<Vec<u8>>| {
            if let Some((serial, bytes)) = earlier {
                next[slot] = bytes.clone();
                say!("source {n}: keeping the index verified earlier (serial {serial})");
            }
        };
        let fetched = fetch_small(net, &format!("{url}{}", upd::INDEX_FILE), upd::MAX_INDEX)
            .and_then(|index| {
                fetch_small(net, &format!("{url}{}", upd::SIGNATURE_FILE), MAX_SIGNATURE)
                    .map(|sig| (index, sig))
            });
        let (index_bytes, sig) = match fetched {
            Ok(v) => v,
            Err(e) => {
                result = download_fault(
                    code::INDEX_FETCH,
                    "upd-index-fetch",
                    format!("source {n} {url}: cannot fetch the index: {e}"),
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
        if let Err(e) = check_fresh(earlier.map(|(s, _)| s), index.serial) {
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
    if next != previous && !write_stored(fs, &next) {
        result = fault(
            code::INDEX_STORE,
            "upd-index-store",
            "cannot store the verified indexes",
        );
    }
    let status = report(fs, sources, pk);
    if result != 0 {
        result
    } else {
        status
    }
}

/// The stored indexes of the registered sources, verified again (ADR 0029,
/// decision 4): (list position, raw bytes).
fn verified_stored(fs: &mut FsClient, sources: &Sources, pk: &[u8; 32]) -> Vec<(usize, Vec<u8>)> {
    let files = read_stored(fs);
    let mut list = Vec::new();
    for (slot, url) in sources.iter().enumerate() {
        let n = slot + 1;
        let Some(s) = files.get(slot).and_then(|b| stored::decode(b)) else {
            fault(
                code::INDEX_MISSING,
                "upd-no-verified-index",
                &format!("source {n}: no verified index yet; run update"),
            );
            continue;
        };
        if s.source != url {
            fault(
                code::INDEX_MISSING,
                "upd-list-changed",
                &format!("source {n}: the list changed since the last update; run update"),
            );
            continue;
        }
        if upd::verify(s.index, s.signature, pk).is_err() {
            fault(
                code::INDEX_REFUSED,
                "upd-stored-refused",
                &format!("source {n}: stored index fails verification; ignored"),
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

fn change_text(installed: Option<u32>, offered: u32) -> &'static str {
    match (installed, compare(installed, offered)) {
        (None, _) => "installed version unknown; upgrade refuses it without --allow-downgrade",
        (_, Change::Same) => "up to date",
        (_, Change::Upgrade) => "upgrade available",
        (_, Change::Downgrade) => {
            "older than installed; upgrade refuses it without --allow-downgrade"
        }
        (_, Change::New) => "new",
    }
}

/// What the stored indexes offer versus what is installed; installs
/// nothing. Everything that differs is saved as the plan, the only thing
/// `upgrade` may install (audit 1, H2).
fn report(fs: &mut FsClient, sources: &Sources, pk: &[u8; 32]) -> u64 {
    let raw = verified_stored(fs, sources, pk);
    let (slots, indexes) = parse_all(&raw, pk);
    let mut plan: Vec<PlanItem> = Vec::new();
    let mut available = 0;
    let mut result = 0;
    match best_system(&indexes) {
        Some((i, s)) => {
            let installed = match Disk::open() {
                Ok(mut disk) => disk.active_version(pk),
                Err(code) => {
                    result = code;
                    None
                }
            };
            available += usize::from(compare(installed, s.version) == Change::Upgrade);
            if installed != Some(s.version) {
                plan.push(PlanItem::System(s.version));
            }
            say!(
                "system: installed {}, source {} offers {}: {}",
                installed.map_or(String::from("unknown"), |v| format!("{v}")),
                slots[i] + 1,
                s.version,
                change_text(installed, s.version)
            );
        }
        None => say!("system: no source offers a system image"),
    }
    let apps = installed_apps(fs).unwrap_or_default();
    for (name, version) in &apps {
        match best_app(&indexes, name) {
            Some((i, a)) => {
                let change = compare(Some(*version), a.version);
                available += usize::from(change == Change::Upgrade);
                if change != Change::Same {
                    plan.push(PlanItem::App(a.name, a.version));
                }
                say!(
                    "app {name}: installed {version}, source {} offers {}: {}",
                    slots[i] + 1,
                    a.version,
                    change_text(Some(*version), a.version)
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
    if !write_state_section(fs, lists::PLAN, &lists::encode_plan(&plan)) {
        result = fault(code::PLAN, "upd-plan-write", "cannot save the plan");
    }
    if indexes.is_empty() {
        say!("no verified index: nothing to compare");
    } else if available == 0 {
        say!("everything is up to date");
    } else {
        say!("{available} upgrade(s) available: run upgrade");
    }
    result
}

// ---------------------------------------------------------------- upgrade

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
        return Err(download_fault(
            code::DOWNLOAD,
            "upd-image-download",
            format!("download failed: {e}"),
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

/// Downloads and checks the package `entry` and writes it to the stage
/// file `file` of this directory; the installer verifies it again.
fn stage_app<'e>(
    fs: &mut FsClient,
    net: &mut NetClient,
    url: &str,
    entry: &upd::AppEntry<'e>,
    file: &'static str,
    pk: &[u8; 32],
) -> Result<StagedEntry<'e>, u64> {
    let size = entry.size as usize;
    if size > MAX_PACKAGE {
        return Err(fault(
            code::PACKAGE_REFUSED,
            "upd-pkg-size",
            &format!("app {}: package too large; skipped", entry.name),
        ));
    }
    say!("downloading {} ({size} bytes)", entry.file);
    let mut bytes = alloc::vec![0u8; size];
    if let Err(e) = fetch_exact(net, &format!("{url}{}", entry.file), &mut bytes) {
        return Err(download_fault(
            code::DOWNLOAD,
            "upd-pkg-download",
            format!("app {}: download failed: {e}", entry.name),
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
    if !write_file(fs, file, &bytes) {
        return Err(fault(
            code::FILE_WRITE,
            "upd-pkg-stage",
            &format!("cannot write {file}"),
        ));
    }
    say!(
        "app {} version {} verified and staged as {}/{file}",
        entry.name,
        entry.version,
        upd::UPDATER_DIR
    );
    Ok(StagedEntry {
        file,
        name: entry.name,
        version: entry.version,
        sha256: entry.sha256,
    })
}

/// `UPGRADE`: installs what the plan lists, from the verified stored
/// indexes.
fn upgrade(
    fs: &mut FsClient,
    net: &mut NetClient,
    sources: &Sources,
    allow_downgrade: bool,
    pk: &[u8; 32],
) -> u64 {
    let state = match read_state(fs) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let Some(plan_text) = lists::section(&state, lists::PLAN) else {
        return fault(
            code::PLAN,
            "upd-no-plan",
            "nothing reported yet: run update first",
        );
    };
    let (plan, rejected) = lists::parse_plan(&plan_text);
    if rejected > 0 {
        fault(
            code::PLAN,
            "upd-plan-damaged",
            &format!("{rejected} invalid line(s) in the plan ignored"),
        );
    }
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
    // Only what `update` reported, at the version it reported (audit 1, H2).
    let not_reported = |what: &str, version: u32| {
        fault(
            code::PLAN,
            "upd-not-reported",
            &format!("{what} {version} was not reported by the last update: run update"),
        )
    };
    // 1. Decide everything first: nothing is downloaded or written yet.
    let mut system_job = None;
    if let Some((i, entry)) = best_system(&indexes) {
        if !plan.contains(&PlanItem::System(entry.version)) {
            if plan.iter().any(|p| matches!(p, PlanItem::System(_))) {
                result = not_reported("system", entry.version);
            } else if let Some(mut disk) = Disk::probe() {
                // The plan has no system line (it was up to date at
                // `update`), yet an index offers a newer one: say so instead
                // of staying silent (re-audit, UX). Machines without the A/B
                // layout are skipped silently.
                if let Some(installed) = disk.active_version(pk) {
                    if entry.version > installed {
                        result = not_reported("system", entry.version);
                    }
                }
            }
        } else {
            match Disk::open() {
                Ok(mut disk) => {
                    let installed = disk.active_version(pk);
                    match decide("system", installed, entry.version, allow_downgrade) {
                        Ok(true) => system_job = Some((disk, i, entry, installed)),
                        Ok(false) => {}
                        Err(code) => result = code,
                    }
                }
                Err(code) => result = code,
            }
        }
    }
    let mut app_jobs = Vec::new();
    for (name, version) in installed_apps(fs).unwrap_or_default() {
        let Some((i, entry)) = best_app(&indexes, &name) else {
            continue;
        };
        if !plan.contains(&PlanItem::App(entry.name, entry.version)) {
            if entry.version != version {
                result = not_reported(&format!("app {name}"), entry.version);
            }
            continue;
        }
        match decide(
            &format!("app {name}"),
            Some(version),
            entry.version,
            allow_downgrade,
        ) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(code) => {
                result = code;
                continue;
            }
        }
        if app_jobs.len() == upd::MAX_STAGED {
            say!(
                "app {name}: more upgrades than stage slots ({}): run upgrade again",
                upd::MAX_STAGED
            );
            break;
        }
        app_jobs.push((i, entry));
    }
    // 2. Reserve every AriFS entry the installs will need (the history
    //    after a system install, one stage file per package) before the
    //    first irreversible step: a full table must refuse the whole
    //    upgrade, never leave the system scheduled and an app not staged
    //    (QEMU run of update.fs_nearly_full).
    let mut needed: Vec<&str> = Vec::new();
    if system_job.is_some() {
        needed.push(upd::HISTORY_FILE);
    }
    needed.extend((0..app_jobs.len()).filter_map(upd::staged_package_file));
    if let Err(missing) = reserve(fs, &needed) {
        return fault(
            code::FS_FULL,
            "upd-fs-full",
            &format!(
                "AriFS has no room for {missing} more file(s) this upgrade needs: nothing installed"
            ),
        );
    }
    // 3. Install.
    if let Some((mut disk, i, entry, installed)) = system_job {
        if let Err(code) = upgrade_system(fs, net, &mut disk, urls[slots[i]], entry, installed, pk)
        {
            result = code;
        }
    }
    let mut staged: Vec<StagedEntry> = Vec::new();
    for (k, (i, entry)) in app_jobs.into_iter().enumerate() {
        let Some(file) = upd::staged_package_file(k) else {
            break;
        };
        match stage_app(fs, net, urls[slots[i]], entry, file, pk) {
            Ok(e) => staged.push(e),
            Err(code) => result = code,
        }
    }
    if !write_state_section(fs, lists::STAGED, &lists::encode_staged(&staged)) {
        return fault(
            code::FILE_WRITE,
            "upd-staged-write",
            "cannot write the staged list",
        );
    }
    if staged.is_empty() {
        result
    } else {
        say!("{} package(s) staged for the installer", staged.len());
        result | updater_arg::EXIT_APPS_STAGED
    }
}

fn run(arg: u64) -> u64 {
    DEADLINE.store(now_ms().saturating_add(RUN_DEADLINE_MS), Ordering::Relaxed);
    let pk = aricode_crypto::trust::public_key();
    let allow_downgrade = arg & updater_arg::ALLOW_DOWNGRADE != 0;
    let command = arg & !updater_arg::ALLOW_DOWNGRADE;
    match (command, allow_downgrade) {
        (updater_arg::APPLY, _) => return apply(allow_downgrade, &pk),
        (updater_arg::REFRESH, false) | (updater_arg::UPGRADE, _) => {}
        _ => {
            return fault(
                code::BAD_ARGUMENT,
                "upd-bad-argument",
                &format!("unknown argument {arg}"),
            )
        }
    }
    let (Ok(mut fs), Ok(mut net)) = (FsClient::new(FS, HW), NetClient::new(NET, HW)) else {
        return fault(
            code::NO_SERVICES,
            "upd-no-services",
            "need the file and network services",
        );
    };
    let sources = match load_sources(&mut fs) {
        Ok(s) => s,
        Err(code) => return code,
    };
    if sources.is_empty() {
        if command == updater_arg::REFRESH {
            forget_stored(&mut fs);
        }
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
    // The log for the shell (private directory).
    if let Ok(mut fs) = FsClient::new(FS, HW) {
        let log = LOG.0.borrow();
        if !write_file(&mut fs, upd::UPDATER_LOG_FILE, &log) {
            let _ = sys::event_record(code::FILE_WRITE, "upd-log-write");
        }
    }
    code
}

aricode_rt::entry!(main);
