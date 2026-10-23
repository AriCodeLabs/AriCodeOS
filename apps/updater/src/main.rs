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
//! - `CHANNEL_DEV`, `CHANNEL_EXPLICIT` (flags, with `REFRESH` or `UPGRADE`,
//!   ADR 0045): act on the development channel's sources instead of the
//!   stable ones; the owner named the channel, the only way `UPGRADE` moves
//!   the system to the other channel.
//! - `ROLLBACK` (3, shell `update rollback`, ADR 0045): schedules the image
//!   already in the inactive slot to boot next, with no network, so a build
//!   that boots but breaks the network can be left from the console.
//!
//! The development channel is trusted only when this build carries its key
//! and the machine's boot loader trusts it too (`BootState::
//! loader_dev_channel`, written by AriBoot): the updater never installs a
//! build its loader would refuse.
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
use aricode_ab::{schedule, BootState, Image, ImageError, Manifest, Slot, PART_STATE, PART_UPDATE};
use aricode_crypto::sha256::sha256;
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::net::NetClient;
use aricode_rt::sys::{self, Handle};
use aricode_update_index::files::{self, Files, ReadError};
use aricode_update_index::lists::{self, PlanItem, StagedEntry};
use aricode_update_index::{
    self as upd, best_app, best_system, check_fresh, compare, compare_system, decide_system,
    stored, updater_arg, Change, Channel, Index, Keys, Migration, Refusal, Sources, SystemChange,
    SystemDecision, SystemId, SystemRules,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SYSTEM: Handle = Handle(0x0001_0004);
const FS: Handle = Handle(0x0001_0005);
const NET: Handle = Handle(0x0001_0006);
const BLOCK: Handle = Handle(0x0001_0007);
const HW: Handle = Handle(0x0001_0008);

/// DMA buffer where a downloaded image (header + container) is held and
/// verified before it is written anywhere (ADR 0029, decision 3). Images
/// already on the disk are verified piece by piece instead (incident 0021).
const IMAGE_BUFFER: u64 = upd::IMAGE_DOWNLOAD_MAX as u64;
const SECTOR: usize = 512;
const CHUNK: u16 = 16;
/// Wait for one network operation.
const HTTP_TIMEOUT_MS: u32 = 15_000;
/// Budget of a whole run: a source that trickles bytes cannot hold the
/// updater forever (audit 1, M3).
const RUN_DEADLINE_MS: u64 = 10 * 60 * 1000;
/// Largest package: an installed package is replaced in one go (the AriFS
/// journal, ADR 0018), the limit the whole system shares (incident 0026).
const MAX_PACKAGE: usize = aricode_pkg::PACKAGE_MAX;
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
    /// The install history could not be read (incident 0008): it is not
    /// rewritten, so a failed request never replaces it by one line.
    pub const HISTORY_READ: u64 = 48;
    pub const DOWNGRADE_REFUSED: u64 = 40;
    pub const DOWNLOAD: u64 = 41;
    pub const DOWNLOAD_MISMATCH: u64 = 42;
    pub const IMAGE_REFUSED: u64 = 43;
    pub const PACKAGE_REFUSED: u64 = 44;
    pub const FILE_WRITE: u64 = 45;
    pub const INSTALLED_UNKNOWN: u64 = 46;
    pub const BAD_ARGUMENT: u64 = 50;
    pub const DEADLINE: u64 = 51;
    /// The system moved to the other channel, as the owner asked (ADR
    /// 0045): recorded, not a failure.
    pub const CHANNEL_SWITCH: u64 = 52;
    /// An image of the other channel without `--channel`: refused.
    pub const CHANNEL_SWITCH_REFUSED: u64 = 53;
    /// `update rollback` scheduled the inactive slot: recorded.
    pub const ROLLBACK: u64 = 54;
    /// `update rollback` refused (no valid image, or one already pending).
    pub const ROLLBACK_REFUSED: u64 = 55;
    /// The development channel asked for on a machine that does not trust
    /// it (no development source, or its loader or updater lacks the key).
    pub const DEV_NOT_TRUSTED: u64 = 56;
    /// A development build number seen again with another commit (audit of
    /// ADR 0045, M2): refused, never taken for the installed build.
    pub const BUILD_CONFLICT: u64 = 57;
    /// The stable floor in `state` is damaged (audit of ADR 0045, M1).
    pub const FLOOR: u64 = 58;
    /// The active slot's image could not be read or verified: said with
    /// the reason, so "installed version unknown" is never unexplained
    /// (incident 0021).
    pub const ACTIVE_IMAGE: u64 = 59;
}

/// Everything printed, kept in `last.log` as the record of the last run
/// (read later with `cat`, or with `fs-cat` from a disk dump).
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

/// The updater's private directory as `files::Files`: only an explicit
/// `NotFound` from the service means a file is absent; a lost reply, a
/// timeout or any other status is an error (incident 0008).
struct Private<'a>(&'a mut FsClient);

impl Files for Private<'_> {
    type Fd = u32;
    type Error = FsError;

    fn open(&mut self, name: &str) -> Result<Option<u32>, FsError> {
        match self.0.open(name) {
            Ok(fd) => Ok(Some(fd)),
            Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read(&mut self, fd: &u32, offset: u32, out: &mut [u8]) -> Result<usize, FsError> {
        self.0.read(*fd, offset, out)
    }

    fn close(&mut self, fd: u32) {
        let _ = self.0.close(fd);
    }

    fn list(
        &mut self,
        index: u32,
        name_out: &mut [u8; files::NAME_LEN],
    ) -> Result<Option<(usize, u64)>, FsError> {
        self.0.list(index, name_out)
    }
}

type Read<T> = Result<Option<T>, ReadError<FsError>>;

/// Reads a whole file of at most `max` bytes. `Ok(None)` if it does not
/// exist; any other failure is an error (never mistaken for "missing").
fn read_file(fs: &mut FsClient, name: &str, max: usize) -> Read<Vec<u8>> {
    files::read_optional(&mut Private(fs), name, max)
}

fn read_text(fs: &mut FsClient, name: &str, max: usize) -> Read<String> {
    files::read_text(&mut Private(fs), name, max)
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

/// One line in the install history (ADR 0029, decision 7). A history that
/// cannot be read is not rewritten: the install happened and was said on
/// the console, but the older lines are never replaced by one line
/// (incident 0008).
fn history(fs: &mut FsClient, line: &str) {
    let mut files = Private(fs);
    let appended = upd::history::appended(&mut files, upd::HISTORY_FILE, line, upd::HISTORY_MAX);
    let log = match appended {
        Ok(log) => log,
        Err(e) => {
            fault(
                code::HISTORY_READ,
                "upd-history-read",
                &format!(
                    "cannot read the install history ({e:?}): not rewritten; unrecorded: {line}"
                ),
            );
            return;
        }
    };
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
        .map_err(|e| {
            fault(
                code::STATE_UNREADABLE,
                "upd-state-unreadable",
                &format!("updater/state cannot be read ({e:?}): not used, not rewritten"),
            )
        })?;
    // Structural damage (repeated or unknown sections) is repaired as
    // `lists` documents and stored at once (an atomic replace), so the
    // fault says exactly what happened (fix update-state-sections).
    if !lists::needs_repair(&text) {
        return Ok(text);
    }
    let fixed = lists::repaired(&text);
    if write_file(fs, upd::STATE_FILE, fixed.as_bytes()) {
        fault(
            code::STATE_REPAIRED,
            "upd-state-repaired",
            "updater/state was damaged (repeated or unknown sections): repaired and saved; a repeated plan or staged list was dropped (run update again)",
        );
    } else {
        fault(
            code::STATE_REPAIRED,
            "upd-state-repaired",
            "updater/state was damaged (repeated or unknown sections): repaired for this run, but the repair could not be saved",
        );
    }
    Ok(fixed)
}

/// Replaces one section of the exchange file, whose text `state` was read
/// once at the start of the run (`read_state`): a run whose read failed
/// never gets here, so the other sections are never replaced by what a
/// failed read did not return.
fn write_state_section(fs: &mut FsClient, state: &str, name: &str, body: &str) -> bool {
    write_file(
        fs,
        upd::STATE_FILE,
        lists::with_section(state, name, body).as_bytes(),
    )
}

/// The installed apps, as the installer listed them just before this run,
/// from the `state` text read once per run. `None` (after a fault) when
/// the list is missing.
fn installed_apps(state: &str) -> Option<Vec<(String, u32)>> {
    let text = match lists::section(state, lists::INSTALLED) {
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

/// Why a slot holds no image this updater accepts (incident 0021: an image
/// too large for memory once read as one that does not verify).
enum LoadError {
    /// No valid slot header: the slot is empty, was being written, or its
    /// header is damaged.
    NoImage,
    /// The header announces more sectors than the partition holds.
    TooLarge { sectors: u64, room: u64 },
    /// The disk could not be read.
    Unreadable,
    /// The container does not verify.
    Invalid(ImageError),
}

impl LoadError {
    fn describe(&self) -> String {
        match self {
            LoadError::NoImage => {
                String::from("no valid slot header (empty slot or damaged header)")
            }
            LoadError::TooLarge { sectors, room } => format!(
                "the header announces {sectors} sectors, more than the {room} of the partition"
            ),
            LoadError::Unreadable => String::from("cannot be read from the disk"),
            LoadError::Invalid(e) => format!("does not verify ({})", image_error(e)),
        }
    }
}

/// What a failed image verification means, for messages.
fn image_error(e: &ImageError) -> &'static str {
    match e {
        ImageError::BadHeader => "invalid slot header",
        ImageError::BadContainer => "malformed container",
        ImageError::MissingFile(_) => "a file is missing from the container",
        ImageError::BadManifest => "invalid manifest",
        ImageError::BadSignature => "signature invalid or by a key this machine does not trust",
        ImageError::HashMismatch(_) => "kernel or initrd hash mismatch",
        ImageError::ChannelMismatch => "the manifest names another channel than its key",
        ImageError::Unreadable => "a read failed",
    }
}

/// The A/B layout of the boot disk and a buffer for one downloaded image.
/// Only opened when a system image is involved (audit 1, M5).
struct Disk {
    blk: BlockClient,
    update: (u64, u64),
    state_part: (u64, u64),
    state: BootState,
    active_part: (u64, u64),
    target: Slot,
    slot_part: (u64, u64),
    /// The download buffer, allocated when a download first needs it: the
    /// DMA quota is spent once per process, and only `upgrade` downloads.
    image: Option<&'static mut [u8]>,
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
        Ok(Self {
            blk,
            update,
            state_part,
            state,
            active_part,
            target,
            slot_part,
            image: None,
        })
    }

    /// The download buffer (`IMAGE_BUFFER` bytes), allocated on first use.
    fn download_buffer(&mut self) -> Result<&mut [u8], u64> {
        if self.image.is_none() {
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
            // SAFETY: our own DMA buffer of IMAGE_BUFFER bytes, mapped RW,
            // used only through this exclusive reference for the life of the
            // process.
            self.image = Some(unsafe {
                core::slice::from_raw_parts_mut(virt as *mut u8, IMAGE_BUFFER as usize)
            });
        }
        Ok(self.image.as_deref_mut().expect("allocated above"))
    }

    /// Verifies the container of `len` bytes that starts one sector after
    /// `first` (the slot header's sector) piece by piece, through the block
    /// client's buffer, so its size is bounded by the partition and not by
    /// memory (incident 0021).
    fn verify_container(
        &mut self,
        first: u64,
        len: usize,
        keys: &Keys,
    ) -> Result<Manifest, ImageError> {
        let blk = &mut self.blk;
        let sectors = len.div_ceil(SECTOR) as u64;
        // The chunk of container sectors now in the client's buffer.
        let mut cached: Option<u64> = None;
        let read = |offset: usize, buf: &mut [u8]| {
            let mut done = 0;
            while done < buf.len() {
                let pos = offset + done;
                let chunk = (pos / SECTOR) as u64 / CHUNK as u64 * CHUNK as u64;
                if cached != Some(chunk) {
                    let n = (sectors - chunk).min(CHUNK as u64) as u16;
                    blk.read(first + 1 + chunk, n).map_err(|_| ())?;
                    cached = Some(chunk);
                }
                let at = pos - chunk as usize * SECTOR;
                let n = (CHUNK as usize * SECTOR - at).min(buf.len() - done);
                buf[done..done + n].copy_from_slice(&blk.buffer()[at..at + n]);
                done += n;
            }
            Ok(())
        };
        Image::verify_streaming(len, read, keys).map(|v| v.manifest)
    }

    /// Verifies the slot image of `part` (header + container) with `keys`
    /// without loading it: (total bytes, manifest), or why not.
    fn load(&mut self, part: (u64, u64), keys: &Keys) -> Result<(usize, Manifest), LoadError> {
        self.blk
            .read(part.0, 1)
            .map_err(|_| LoadError::Unreadable)?;
        let len = Image::container_len(&self.blk.buffer()[..SECTOR])
            .map_err(|_| LoadError::NoImage)? as usize;
        let sectors = (SECTOR + len).div_ceil(SECTOR) as u64;
        let room = part.1 - part.0 + 1;
        if sectors > room {
            return Err(LoadError::TooLarge { sectors, room });
        }
        match self.verify_container(part.0, len, keys) {
            Ok(manifest) => Ok((SECTOR + len, manifest)),
            Err(ImageError::Unreadable) => Err(LoadError::Unreadable),
            Err(e) => Err(LoadError::Invalid(e)),
        }
    }

    /// The active slot's manifest (version, channel, commit), verified;
    /// `None`, with the reason said and recorded, if it cannot be read or
    /// does not verify (then the downgrade rule treats it as unknown, ADR
    /// 0029, decision 5).
    fn active_system(&mut self, keys: &Keys) -> Option<Manifest> {
        let part = self.active_part;
        match self.load(part, keys) {
            Ok((_, m)) => Some(m),
            Err(e) => {
                fault(
                    code::ACTIVE_IMAGE,
                    "upd-active-image",
                    &format!(
                        "the image in the active slot {}: {}",
                        self.state.active.name(),
                        e.describe()
                    ),
                );
                None
            }
        }
    }

    fn write(&mut self, lba: u64, sectors: u16) -> Result<(), u64> {
        self.blk
            .write(lba, sectors)
            .map_err(|_| fault(code::SLOT_WRITE, "upd-slot-write", "error writing the slot"))
    }

    /// Checks that `total` bytes fit the inactive slot and clears its
    /// header first (audit 1, L1): an interrupted copy then leaves a slot
    /// without a valid header, which AriBoot never boots, instead of an old
    /// header over a half-new container. Returns the image's sectors.
    fn begin_install(&mut self, total: usize) -> Result<u64, u64> {
        let sectors = total.div_ceil(SECTOR) as u64;
        if sectors > self.slot_part.1 - self.slot_part.0 + 1 {
            return Err(fault(
                code::SLOT_TOO_SMALL,
                "upd-slot-small",
                "image does not fit the slot",
            ));
        }
        self.blk.buffer()[..SECTOR].fill(0);
        self.write(self.slot_part.0, 1)?;
        self.flush_slot()?;
        Ok(sectors)
    }

    /// Writes the header last, then schedules the slot.
    fn finish_install(&mut self, header: &[u8; SECTOR]) -> Result<(), u64> {
        self.blk.buffer()[..SECTOR].copy_from_slice(header);
        self.write(self.slot_part.0, 1)?;
        self.flush_slot()?;
        say!("installed into slot {}", self.target.name());
        self.schedule_target()
    }

    fn flush_slot(&mut self) -> Result<(), u64> {
        self.blk.flush().map_err(|_| {
            fault(
                code::SLOT_WRITE,
                "upd-slot-write",
                "error flushing the slot",
            )
        })
    }

    /// Writes the verified image in the download buffer (`total` bytes,
    /// verified as `expected`) into the inactive slot, checks the copy there
    /// and schedules it as pending with tries.
    fn install(&mut self, total: usize, expected: &Manifest, keys: &Keys) -> Result<(), u64> {
        let sectors = self.begin_install(total)?;
        let image = self.image.take().expect("the image was downloaded");
        image[total..sectors as usize * SECTOR].fill(0);
        let mut done = 1u64;
        let mut written = Ok(());
        while done < sectors && written.is_ok() {
            let n = (sectors - done).min(CHUNK as u64) as u16;
            let o = done as usize * SECTOR;
            let bytes = n as usize * SECTOR;
            self.blk.buffer()[..bytes].copy_from_slice(&image[o..o + bytes]);
            written = self.write(self.slot_part.0 + done, n);
            done += n as u64;
        }
        let mut header = [0u8; SECTOR];
        header.copy_from_slice(&image[..SECTOR]);
        self.image = Some(image);
        written?;
        self.flush_slot()?;
        self.check_copy(total, expected, keys)?;
        self.finish_install(&header)
    }

    /// Verifies the container just written to the inactive slot, before its
    /// header: the copy must be the image verified before it was written
    /// (audit of incident 0021, L2 and L4).
    fn check_copy(&mut self, total: usize, expected: &Manifest, keys: &Keys) -> Result<(), u64> {
        let refused = |event: &str, why: String| {
            Err(fault(
                code::SLOT_WRITE,
                event,
                &format!("the copy in slot {why}; not scheduled"),
            ))
        };
        let slot = self.target.name();
        match self.verify_container(self.slot_part.0, total - SECTOR, keys) {
            Ok(m) if m == *expected => Ok(()),
            Ok(m) => refused(
                "upd-slot-differs",
                format!(
                    "{slot} is {}, not the {} verified before the copy",
                    m.describe(),
                    expected.describe()
                ),
            ),
            Err(ImageError::Unreadable) => {
                refused("upd-slot-read", format!("{slot} cannot be read back"))
            }
            Err(e) => refused(
                "upd-slot-verify",
                format!("{slot} does not verify ({})", image_error(&e)),
            ),
        }
    }

    /// Copies the image of `total` bytes verified in `UPDATE` (as
    /// `expected`) into the inactive slot, verifies the copy in the slot
    /// itself before writing its header, and schedules it: what boots is
    /// what was verified, without holding the image in memory (incident
    /// 0021).
    fn install_from_update(
        &mut self,
        total: usize,
        expected: &Manifest,
        keys: &Keys,
    ) -> Result<(), u64> {
        let sectors = self.begin_install(total)?;
        let mut done = 1u64;
        while done < sectors {
            let n = (sectors - done).min(CHUNK as u64) as u16;
            if self.blk.read(self.update.0 + done, n).is_err() {
                return Err(fault(
                    code::UPDATE_INVALID,
                    "upd-update-read",
                    "cannot read UPDATE while copying it; not installing",
                ));
            }
            self.write(self.slot_part.0 + done, n)?;
            done += n as u64;
        }
        self.flush_slot()?;
        self.check_copy(total, expected, keys)?;
        self.finish_install(&Image::header((total - SECTOR) as u32))
    }

    /// Marks the inactive slot pending with tries and reads the state back.
    fn schedule_target(&mut self) -> Result<(), u64> {
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

/// A system as reports name it: `12` for a release (as before ADR 0045),
/// `dev build 7 (commit 0123abc)` for a development build.
fn ident(channel: Channel, version: u32, commit: Option<&str>) -> String {
    match (channel, commit) {
        (Channel::Stable, _) => format!("{version}"),
        (Channel::Dev, Some(c)) => format!("dev build {version} (commit {c})"),
        (Channel::Dev, None) => format!("dev build {version}"),
    }
}

/// [`ident`] of a verified manifest.
fn manifest_ident(m: &Manifest) -> String {
    ident(m.channel, m.version, m.commit.as_ref().map(|c| c.as_str()))
}

/// The downgrade rule (ADR 0029, decision 5) for one app; `Ok(true)` when
/// it must be installed.
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

fn installed_id(m: &Manifest) -> SystemId<'_> {
    SystemId {
        channel: m.channel,
        version: m.version,
        commit: m.commit.as_ref().map(|c| c.as_str()),
    }
}

/// The stable floor (audit of ADR 0045, M1): the newest stable release
/// this machine ran, kept in the `[floor]` section of `state` (seeded by
/// `usb-image`), raised to the active system when that is a newer stable
/// release, and saved. `state` is updated in place, so the sections this
/// run writes later keep it.
fn stable_floor(
    fs: &mut FsClient,
    state: &mut String,
    installed: &Option<Manifest>,
) -> Option<u32> {
    let body = lists::section(state, lists::FLOOR).unwrap_or_default();
    let floor = lists::parse_floor(&body).unwrap_or_else(|_| {
        fault(
            code::FLOOR,
            "upd-floor-damaged",
            "the stable floor in updater/state is damaged: ignored (the running stable release sets it again)",
        );
        None
    });
    let raised = upd::raise_floor(floor, installed.as_ref().map(installed_id));
    if let (true, Some(version)) = (raised != floor, raised) {
        let next = lists::with_section(state, lists::FLOOR, &lists::encode_floor(version));
        if write_file(fs, upd::STATE_FILE, next.as_bytes()) {
            *state = next;
        } else {
            fault(
                code::FILE_WRITE,
                "upd-floor-write",
                "cannot save the stable floor",
            );
        }
    }
    raised
}

/// The system install rule (ADR 0029 decision 5, ADR 0045 decision 2,
/// `decide_system`): `Ok(true)` when the offered image must be installed.
/// A switch is said here and recorded once installed (`upgrade_system`).
fn decide_system_install(
    installed: &Option<Manifest>,
    offered: SystemId,
    rules: SystemRules,
) -> Result<bool, u64> {
    let change = compare_system(installed.as_ref().map(installed_id), offered);
    let current = installed.as_ref().map(manifest_ident).unwrap_or_default();
    let offered_ident = ident(offered.channel, offered.version, offered.commit);
    match decide_system(change, offered, rules) {
        SystemDecision::Install => {
            let below_floor = rules
                .stable_floor
                .is_some_and(|f| offered.channel == Channel::Stable && offered.version < f);
            if below_floor && change != SystemChange::Within(Change::Downgrade) {
                say!(
                    "system: downgrading below the stable floor {} to {offered_ident} (--allow-downgrade)",
                    rules.stable_floor.unwrap_or(0)
                );
            }
            match change {
                SystemChange::Unknown => say!(
                    "system: installed version unknown; installing {offered_ident} (--allow-downgrade)"
                ),
                SystemChange::Within(Change::Downgrade) => say!(
                    "system: downgrading from {current} to {offered_ident} (--allow-downgrade)"
                ),
                SystemChange::Switch { from, to } => say!(
                    "system: switching from the {} channel ({current}) to the {} channel ({offered_ident}), as asked with --channel {}",
                    from.name(),
                    to.name(),
                    to.name()
                ),
                SystemChange::Within(_) | SystemChange::Conflict => {}
            }
            Ok(true)
        }
        SystemDecision::AlreadyInstalled => {
            say!("system: version {offered_ident} already installed");
            Ok(false)
        }
        SystemDecision::Refused(Refusal::Unknown) => Err(fault(
            code::INSTALLED_UNKNOWN,
            "upd-installed-unknown",
            "system: installed version unknown (unreadable or unverifiable): refused (--allow-downgrade to force)",
        )),
        SystemDecision::Refused(Refusal::Downgrade) => Err(fault(
            code::DOWNGRADE_REFUSED,
            "upd-downgrade-refused",
            &format!(
                "system: refused: version {offered_ident} is older than the installed {current} (--allow-downgrade to force)"
            ),
        )),
        SystemDecision::Refused(Refusal::BelowStableFloor(floor)) => Err(fault(
            code::DOWNGRADE_REFUSED,
            "upd-downgrade-refused",
            &format!(
                "system: refused: a downgrade: stable {offered_ident} is older than stable {floor}, the newest release this machine ran (--allow-downgrade to force)"
            ),
        )),
        SystemDecision::Refused(Refusal::SwitchNotExplicit) if !rules.explicit_channel => {
            Err(fault(
                code::CHANNEL_SWITCH_REFUSED,
                "upd-channel-switch-refused",
                &format!(
                    "system: refused: {offered_ident} belongs to the {} channel and {} is installed; the channel only changes when asked: upgrade --channel {}",
                    offered.channel.name(),
                    if current.is_empty() { "an unknown system" } else { &current },
                    offered.channel.name()
                ),
            ))
        }
        SystemDecision::Refused(Refusal::SwitchNotExplicit) => Err(fault(
            code::CHANNEL_SWITCH_REFUSED,
            "upd-channel-switch-refused",
            &format!("system: refused: {offered_ident} belongs to another channel"),
        )),
        SystemDecision::Refused(Refusal::Conflict) => Err(fault(
            code::BUILD_CONFLICT,
            "upd-build-conflict",
            &format!(
                "system: refused: {offered_ident} reuses the number of the installed {current} with another commit; publish again for a new build number"
            ),
        )),
    }
}

/// `APPLY`: installs the image of the UPDATE partition (Phase 30), under the
/// downgrade rule (audit 1, M4) and the stable floor (ADR 0045). It never
/// changes the channel (that takes `upgrade --channel`).
fn apply(allow_downgrade: bool, keys: &Keys) -> u64 {
    let mut disk = match Disk::open() {
        Ok(d) => d,
        Err(code) => return code,
    };
    let installed = disk.active_system(keys);
    // The stable floor holds here too (no open downgrade path): without
    // the file service or a readable `state` the floor is unknown, and
    // nothing is installed.
    let Ok(mut fs) = FsClient::new(FS, HW) else {
        return fault(
            code::NO_SERVICES,
            "upd-no-services",
            "need the file service (the stable floor is in updater/state)",
        );
    };
    let mut state = match read_state(&mut fs) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let stable_floor = stable_floor(&mut fs, &mut state, &installed);
    let update = disk.update;
    let (total, manifest) = match disk.load(update, keys) {
        Ok(loaded) => loaded,
        Err(e) => {
            return fault(
                code::UPDATE_INVALID,
                "upd-update-invalid",
                &format!(
                    "UPDATE holds no valid system image: {}; not installing",
                    e.describe()
                ),
            )
        }
    };
    let offered = manifest_ident(&manifest);
    say!(
        "UPDATE image version {offered} verified ({} bytes)",
        total - SECTOR
    );
    // Never a channel change (audit of ADR 0045, L4): no named channel;
    // never below the stable floor without --allow-downgrade.
    let rules = SystemRules {
        allow_downgrade,
        explicit_channel: false,
        stable_floor,
    };
    match decide_system_install(&installed, installed_id(&manifest), rules) {
        Ok(true) => match disk.install_from_update(total, &manifest, keys) {
            Ok(()) => 0,
            Err(code) => code,
        },
        Ok(false) => 0,
        Err(code) => code,
    }
}

/// `ROLLBACK` (shell `update rollback`, ADR 0045 decision 4): the image
/// already in the inactive slot (the system before the last install) is
/// verified and scheduled to boot next with tries, exactly like a fresh
/// install; init confirms it once it boots and it becomes active. No
/// network, no download: the way back from a build that boots but lost
/// the network. A slot with no valid image, or an install already pending,
/// is refused.
fn rollback(fs: Option<&mut FsClient>, keys: &Keys) -> u64 {
    let mut disk = match Disk::open() {
        Ok(d) => d,
        Err(code) => return code,
    };
    if let Some((slot, _)) = disk.state.pending {
        return fault(
            code::ROLLBACK_REFUSED,
            "upd-rollback-pending",
            &format!(
                "rollback refused: slot {} is already pending; reboot to try it (or let it roll back)",
                slot.name()
            ),
        );
    }
    let active = disk.active_system(keys);
    let current = active
        .as_ref()
        .map_or(String::from("unknown"), manifest_ident);
    let target = disk.slot_part;
    let manifest = match disk.load(target, keys) {
        Ok((_, m)) => m,
        Err(e) => {
            return fault(
                code::ROLLBACK_REFUSED,
                "upd-rollback-none",
                &format!(
                    "rollback refused: slot {} holds no image this machine can boot: {}",
                    disk.target.name(),
                    e.describe()
                ),
            )
        }
    };
    let previous = manifest_ident(&manifest);
    say!(
        "slot {} holds system {previous}, verified",
        disk.target.name()
    );
    // What the rollback does, named (audit of ADR 0045, L3): it may leave
    // the channel or go back to an older release or build. The slot holds
    // the system before the last install, or, after an install the loader
    // rejected or rolled back, that failed image.
    let kind = match &active {
        Some(a) if a.channel != manifest.channel => format!(
            ": a channel switch from {} to {}",
            a.channel.name(),
            manifest.channel.name()
        ),
        Some(a) if manifest.version < a.version => String::from(": a downgrade"),
        Some(a) if manifest.version == a.version => String::from(": the same version"),
        Some(_) => String::from(": a newer system (the last install did not stay)"),
        None => String::new(),
    };
    if let Err(code) = disk.schedule_target() {
        return code;
    }
    let line = format!(
        "rollback: system {previous} in slot {} scheduled in place of {current}{kind} (pending until reboot)",
        disk.target.name()
    );
    fault(code::ROLLBACK, "upd-rollback", &line);
    if let Some(fs) = fs {
        history(fs, &line);
    }
    say!("reboot to boot system {previous}");
    0
}

// ---------------------------------------------------------------- network

fn http_error(e: aricode_https::HttpsError) -> String {
    use aricode_https::HttpsError as E;
    String::from(match e {
        E::BadUrl => "invalid URL",
        E::Resolve(_) => "cannot resolve the host",
        E::Connect(_) => "cannot connect",
        E::Io(_) => "connection lost",
        E::Tls(_) => "TLS failure (certificate or protocol)",
        E::Malformed => "invalid HTTP response",
        E::TooBig => "response headers too large",
        E::Unsupported => "unsupported HTTP response",
        E::Aborted => "aborted",
        E::Deadline(phase) => return format!("deadline passed {}", phase.describe()),
        E::TooManyInformational => "too many interim responses",
        E::HeaderBytes => "response headers too large in all (interim responses included)",
        E::EmptyRecords => "TLS stall (records without data kept coming)",
    })
}

/// The run's budget as the HTTP client sees it: what is left until
/// `DEADLINE`, asked before every network operation.
fn run_budget() -> u32 {
    remaining_ms().min(u32::MAX as u64) as u32
}

/// Downloads `url` into `out`, never more than `out.len()` bytes (the sink
/// refuses more) and never past the run's deadline (audit 1, M3): the
/// budget bounds the name resolution, the connection, the TLS handshake and
/// every send and receive, headers and chunk framing included, so a server
/// that keeps the response coming without ever delivering a body byte
/// cannot hold the run (incident 0009). Returns the bytes received.
fn fetch(net: &mut NetClient, url: &str, out: &mut [u8]) -> Result<usize, String> {
    let (mut got, mut overflow) = (0usize, false);
    let mut sink = |chunk: &[u8]| {
        if got + chunk.len() > out.len() {
            overflow = true;
            return false;
        }
        out[got..got + chunk.len()].copy_from_slice(chunk);
        got += chunk.len();
        true
    };
    let result = aricode_https::get_with(
        net,
        SYSTEM,
        &[],
        url,
        HTTP_TIMEOUT_MS,
        &run_budget,
        &mut sink,
    );
    match result {
        Ok(200) => Ok(got),
        Ok(status) => Err(format!("HTTP {status}")),
        Err(_) if overflow => Err(format!("larger than {} bytes", out.len())),
        Err(e) => Err(http_error(e)),
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
        Err(e) => {
            INDEXES_UNREADABLE.store(true, Ordering::Relaxed);
            fault(
                code::INDEX_STORE,
                "upd-indexes-unreadable",
                &format!("the stored indexes cannot be read ({e:?}): not used, not rewritten in this run"),
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

/// A stored file holding a verified index for `url`, a source of
/// `channel` (verified with that channel's key, ADR 0045).
fn verified_for<'a>(
    bytes: &'a [u8],
    url: &str,
    keys: &Keys,
    channel: Channel,
) -> Option<Index<'a>> {
    stored::decode(bytes)
        .filter(|s| s.source == url)
        .and_then(|s| upd::verify_channel(s.index, s.signature, keys, channel).ok())
}

/// What a source's index offers, for the console: `system 2` or
/// `dev build 7 (commit 0123abc)`.
fn offer_text(index: &Index) -> String {
    match &index.system {
        Some(s) if index.channel == Channel::Dev => ident(Channel::Dev, s.version, index.commit),
        Some(s) => format!("system {}", s.version),
        None => String::from("no system image"),
    }
}

/// `REFRESH`: fetch, verify and store the index of every source of
/// `channel`, then report. The stored copies of the other channel's
/// sources are kept as they are.
fn refresh(
    fs: &mut FsClient,
    net: &mut NetClient,
    sources: &Sources,
    keys: &Keys,
    channel: Channel,
) -> u64 {
    let previous = read_stored(fs);
    let mut next: Vec<Vec<u8>> = alloc::vec![Vec::new(); upd::MAX_SOURCES];
    let mut result = 0;
    for (slot, source) in sources.entries().enumerate() {
        let (n, url) = (slot + 1, source.url.as_str());
        if source.channel != channel {
            // Not this run's channel: its stored copy stays (verified again
            // whenever it is used).
            if let Some(bytes) = previous
                .iter()
                .find(|b| stored::decode(b).is_some_and(|s| s.source == url))
            {
                next[slot] = bytes.clone();
            }
            continue;
        }
        // The newest verified copy already stored for this URL, wherever it is.
        let earlier = previous
            .iter()
            .filter_map(|b| verified_for(b, url, keys, channel).map(|i| (i.serial, b)))
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
        let index = match upd::verify_channel(&index_bytes, &sig, keys, channel) {
            Ok(index) => index,
            Err(e) => {
                let what = match channel {
                    Channel::Stable => "signature or format invalid",
                    Channel::Dev => {
                        "signature or format invalid, or not signed with the development channel's key"
                    }
                };
                result = fault(
                    code::INDEX_REFUSED,
                    "upd-index-refused",
                    &format!(
                        "source {n} {url}: index refused ({e:?}): {what}; nothing from it is used"
                    ),
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
            offer_text(&index),
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
    let status = report(fs, sources, keys, channel);
    if result != 0 {
        result
    } else {
        status
    }
}

/// The stored indexes of the registered sources of `channel`, verified
/// again (ADR 0029, decision 4): (list position, raw bytes).
fn verified_stored(
    fs: &mut FsClient,
    sources: &Sources,
    keys: &Keys,
    channel: Channel,
) -> Vec<(usize, Vec<u8>)> {
    let files = read_stored(fs);
    let mut list = Vec::new();
    for (slot, source) in sources.entries().enumerate() {
        if source.channel != channel {
            continue;
        }
        let (n, url) = (slot + 1, source.url.as_str());
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
        if upd::verify_channel(s.index, s.signature, keys, channel).is_err() {
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
fn parse_all<'a>(
    raw: &'a [(usize, Vec<u8>)],
    keys: &Keys,
    channel: Channel,
) -> (Vec<usize>, Vec<Index<'a>>) {
    raw.iter()
        .filter_map(|(slot, bytes)| {
            let s = stored::decode(bytes)?;
            upd::verify_channel(s.index, s.signature, keys, channel)
                .ok()
                .map(|index| (*slot, index))
        })
        .unzip()
}

const UNKNOWN_TEXT: &str =
    "installed version unknown; upgrade refuses it without --allow-downgrade";

/// What installing an offer of the same channel would do, for the report.
fn within_text(c: Change) -> &'static str {
    match c {
        Change::Same => "up to date",
        Change::Upgrade => "upgrade available",
        Change::Downgrade => "older than installed; upgrade refuses it without --allow-downgrade",
        Change::New => "new",
    }
}

fn change_text(installed: Option<u32>, offered: u32) -> &'static str {
    match installed {
        None => UNKNOWN_TEXT,
        Some(_) => within_text(compare(installed, offered)),
    }
}

/// [`change_text`] for a system image, channels included (ADR 0045).
fn system_change_text(change: SystemChange) -> String {
    match change {
        SystemChange::Within(c) => String::from(within_text(c)),
        SystemChange::Unknown => String::from(UNKNOWN_TEXT),
        SystemChange::Switch { from, to } => format!(
            "the {} channel, this system is on the {} channel: upgrade --channel {} switches to it",
            to.name(),
            from.name(),
            to.name()
        ),
        SystemChange::Conflict => String::from(
            "the installed build number with another commit (published twice?); upgrade refuses it: publish again",
        ),
    }
}

/// The exit code of `report` names the first failure of the run (each
/// failure is a fault event of its own, so nothing is lost).
fn first(result: &mut u64, code: u64) {
    if *result == 0 {
        *result = code;
    }
}

/// What the stored indexes of `channel` offer versus what is installed;
/// installs nothing. Everything that differs is saved as the plan, the
/// only thing `upgrade` may install (audit 1, H2). Apps come from stable
/// sources only (a development index offers a system image alone).
fn report(fs: &mut FsClient, sources: &Sources, keys: &Keys, channel: Channel) -> u64 {
    // Read once per run. Unreadable: the system is still reported, the
    // apps are not compared (an unreadable list is not "none installed")
    // and no plan is saved (incident 0008 audit, M2).
    let mut state = read_state(fs);
    let raw = verified_stored(fs, sources, keys, channel);
    let (slots, indexes) = parse_all(&raw, keys, channel);
    let mut plan: Vec<PlanItem> = Vec::new();
    let (mut available, mut switches) = (0, 0);
    let mut result = 0;
    match best_system(&indexes) {
        Some((i, s)) => {
            let installed = match Disk::open() {
                Ok(mut disk) => disk.active_system(keys),
                Err(code) => {
                    first(&mut result, code);
                    None
                }
            };
            let floor = match &mut state {
                Ok(text) => stable_floor(fs, text, &installed),
                Err(_) => None,
            };
            let offered = SystemId {
                channel,
                version: s.version,
                commit: indexes[i].commit,
            };
            let change = compare_system(installed.as_ref().map(installed_id), offered);
            // Below the floor but not below the installed release (that is
            // a plain downgrade, named as such).
            let below_floor = channel == Channel::Stable
                && floor.is_some_and(|f| s.version < f)
                && change != SystemChange::Within(Change::Downgrade);
            available +=
                usize::from(change == SystemChange::Within(Change::Upgrade) && !below_floor);
            switches += usize::from(matches!(change, SystemChange::Switch { .. }) && !below_floor);
            if installed.as_ref().map(installed_id) != Some(offered) {
                plan.push(PlanItem::System(channel, s.version));
            }
            let text = match (change, floor) {
                (SystemChange::Within(Change::Same), _) => system_change_text(change),
                (_, Some(f)) if below_floor => format!(
                    "older than stable {f}, the newest release this machine ran; upgrade refuses it without --allow-downgrade"
                ),
                _ => system_change_text(change),
            };
            say!(
                "system: installed {}, source {} offers {}: {text}",
                installed
                    .as_ref()
                    .map_or(String::from("unknown"), manifest_ident),
                slots[i] + 1,
                ident(channel, s.version, indexes[i].commit),
            );
        }
        None => say!("system: no source offers a system image"),
    }
    let apps = match (&state, channel) {
        (_, Channel::Dev) => Vec::new(),
        (Ok(state), Channel::Stable) => installed_apps(state).unwrap_or_default(),
        (Err(code), Channel::Stable) => {
            first(&mut result, *code);
            say!("apps not compared: updater/state could not be read");
            Vec::new()
        }
    };
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
    for index in indexes.iter().filter(|_| state.is_ok()) {
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
    let mut saved = false;
    if let Ok(state) = &state {
        let body = lists::encode_plan(channel, &plan);
        saved = write_state_section(fs, state, lists::PLAN, &body);
        if !saved {
            first(
                &mut result,
                fault(code::PLAN, "upd-plan-write", "cannot save the plan"),
            );
        }
    }
    let upgrade_command = match channel {
        Channel::Stable => "upgrade",
        Channel::Dev => "upgrade --channel dev",
    };
    if indexes.is_empty() {
        say!("no verified index: nothing to compare");
    } else if !saved {
        // Without a saved plan `upgrade` has nothing (or something stale)
        // to follow: the owner runs `update` again.
        say!(
            "{available} upgrade(s) found but the plan was not saved ({}): run update again",
            if state.is_ok() {
                "write failed"
            } else {
                "updater/state could not be read"
            }
        );
    } else if available == 0 && switches == 0 {
        say!("everything is up to date");
    } else if available == 0 {
        say!(
            "the {} channel offers another system: {upgrade_command} switches to it",
            channel.name()
        );
    } else {
        say!("{available} upgrade(s) available: run {upgrade_command}");
    }
    result
}

// ---------------------------------------------------------------- upgrade

/// The system image a source offers, with what its index says about it.
struct SystemOffer<'a> {
    url: &'a str,
    entry: &'a upd::SystemEntry<'a>,
    channel: Channel,
    /// The commit a development index names.
    commit: Option<&'a str>,
}

/// Downloads, checks and installs the system image `offer`.
fn upgrade_system(
    fs: &mut FsClient,
    net: &mut NetClient,
    disk: &mut Disk,
    offer: &SystemOffer,
    installed: &Option<Manifest>,
    keys: &Keys,
) -> Result<(), u64> {
    let (url, entry) = (offer.url, offer.entry);
    let size = entry.size as usize;
    if size <= SECTOR || size > IMAGE_BUFFER as usize {
        let why = if size <= SECTOR {
            String::from("too small to hold an image")
        } else {
            format!(
                "more than the {} this updater can download and verify in memory",
                upd::IMAGE_DOWNLOAD_MAX
            )
        };
        return Err(fault(
            code::IMAGE_REFUSED,
            "upd-image-size",
            &format!("system image {}: {size} bytes, {why}", entry.file),
        ));
    }
    let image = disk.download_buffer()?;
    say!("downloading {} ({size} bytes)", entry.file);
    // Into memory only: nothing touches the disk until every check passed.
    if let Err(e) = fetch_exact(net, &format!("{url}{}", entry.file), &mut image[..size]) {
        return Err(download_fault(
            code::DOWNLOAD,
            "upd-image-download",
            format!("download failed: {e}"),
        ));
    }
    if sha256(&image[..size]) != entry.sha256 {
        return Err(fault(
            code::DOWNLOAD_MISMATCH,
            "upd-image-sha256",
            "system image SHA-256 does not match the signed index; not installing",
        ));
    }
    // Its own signature, verified again as AriBoot will (Phase 30): the
    // index authenticates the file, the manifest authenticates the content,
    // and the key that signed the manifest is the channel's (ADR 0045).
    let manifest = Image::container_len(&image[..SECTOR])
        .ok()
        .filter(|&len| SECTOR + len as usize == size)
        .and_then(|_| Image::verify_with(&image[SECTOR..size], keys).ok())
        .map(|v| v.manifest);
    let offered = ident(offer.channel, entry.version, offer.commit);
    let verified = match manifest {
        Some(m) if upd::image_matches_offer(&m, offer.channel, entry.version, offer.commit) => m,
        Some(m) => {
            return Err(fault(
                code::IMAGE_REFUSED,
                "upd-image-version",
                &format!(
                    "system image says {}, the index says {offered}; not installing",
                    m.describe()
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
    };
    let described = aricode_ab::Describe {
        channel: offer.channel,
        version: entry.version,
        commit: offer.commit.and_then(aricode_ab::Commit::parse),
    };
    say!("system image {described} verified (signature and SHA-256)");
    disk.install(size, &verified, keys)?;
    let switched = installed.as_ref().filter(|m| m.channel != offer.channel);
    history(
        fs,
        &format!(
            "system {} -> {offered} installed into slot {} from {url} (pending until reboot){}",
            installed.as_ref().map_or(String::from("?"), manifest_ident),
            disk.target.name(),
            if switched.is_some() {
                ", channel switch asked with --channel"
            } else {
                ""
            }
        ),
    );
    if let Some(from) = switched {
        // The owner's explicit switch, recorded where `diag` shows it
        // (ADR 0045, decision 2).
        fault(
            code::CHANNEL_SWITCH,
            "upd-channel-switch",
            &format!(
                "system: channel switched from {} to {} (pending until reboot)",
                from.channel.name(),
                offer.channel.name()
            ),
        );
    }
    say!("system {offered} installed: reboot to apply");
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
            &format!(
                "app {}: package of {size} bytes, larger than the {MAX_PACKAGE} a package holds; skipped",
                entry.name
            ),
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
/// indexes of `channel`. `explicit`: the owner named the channel
/// (`--channel`), the only way the system moves to the other one.
fn upgrade(
    fs: &mut FsClient,
    net: &mut NetClient,
    sources: &Sources,
    keys: &Keys,
    channel: Channel,
    allow_downgrade: bool,
    explicit: bool,
) -> u64 {
    let mut state = match read_state(fs) {
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
    let update_command = |c: Channel| match c {
        Channel::Stable => "update",
        Channel::Dev => "update --channel dev",
    };
    if plan.channel != channel {
        return fault(
            code::PLAN,
            "upd-plan-channel",
            &format!(
                "the last update reported the {} channel, not the {} channel: run {} first",
                plan.channel.name(),
                channel.name(),
                update_command(channel)
            ),
        );
    }
    let raw = verified_stored(fs, sources, keys, channel);
    let (slots, indexes) = parse_all(&raw, keys, channel);
    if indexes.is_empty() {
        return fault(
            code::NO_INDEX,
            "upd-no-index",
            &format!("no verified index: run {} first", update_command(channel)),
        );
    }
    let urls: Vec<&str> = sources.iter().collect();
    let mut result = 0;
    // Only what `update` reported, at the version it reported (audit 1, H2).
    let not_reported = |what: &str| {
        fault(
            code::PLAN,
            "upd-not-reported",
            &format!(
                "{what} was not reported by the last update: run {}",
                update_command(channel)
            ),
        )
    };
    // 1. Decide everything first: nothing is downloaded or written yet.
    let mut system_job = None;
    if let Some((i, entry)) = best_system(&indexes) {
        let offered = SystemId {
            channel,
            version: entry.version,
            commit: indexes[i].commit,
        };
        let what = format!("system {}", ident(channel, entry.version, offered.commit));
        if !plan
            .items
            .contains(&PlanItem::System(channel, entry.version))
        {
            if plan.items.iter().any(|p| matches!(p, PlanItem::System(..))) {
                result = not_reported(&what);
            } else if let Some(mut disk) = Disk::probe() {
                // The plan has no system line (it was up to date at
                // `update`), yet an index offers a newer one or another
                // channel's: say so instead of staying silent (re-audit,
                // UX). Machines without the A/B layout are skipped silently.
                if let Some(installed) = disk.active_system(keys) {
                    let change = compare_system(Some(installed_id(&installed)), offered);
                    if matches!(
                        change,
                        SystemChange::Within(Change::Upgrade)
                            | SystemChange::Switch { .. }
                            | SystemChange::Conflict
                    ) {
                        result = not_reported(&what);
                    }
                }
            }
        } else {
            match Disk::open() {
                Ok(mut disk) => {
                    let installed = disk.active_system(keys);
                    let rules = SystemRules {
                        allow_downgrade,
                        explicit_channel: explicit,
                        stable_floor: stable_floor(fs, &mut state, &installed),
                    };
                    match decide_system_install(&installed, offered, rules) {
                        Ok(true) => {
                            let offer = SystemOffer {
                                url: urls[slots[i]],
                                entry,
                                channel,
                                commit: indexes[i].commit,
                            };
                            system_job = Some((disk, offer, installed));
                        }
                        Ok(false) => {}
                        Err(code) => result = code,
                    }
                }
                Err(code) => result = code,
            }
        }
    }
    // Apps come from stable sources only: a development index offers a
    // system image alone (ADR 0045).
    let installed_list = match channel {
        Channel::Stable => installed_apps(&state).unwrap_or_default(),
        Channel::Dev => Vec::new(),
    };
    let mut app_jobs = Vec::new();
    for (name, version) in installed_list {
        let Some((i, entry)) = best_app(&indexes, &name) else {
            continue;
        };
        if !plan
            .items
            .contains(&PlanItem::App(entry.name, entry.version))
        {
            if entry.version != version {
                result = not_reported(&format!("app {name} {}", entry.version));
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
    if let Some((mut disk, offer, installed)) = system_job {
        if let Err(code) = upgrade_system(fs, net, &mut disk, &offer, &installed, keys) {
            result = code;
        }
    }
    let mut staged: Vec<StagedEntry> = Vec::new();
    for (k, (i, entry)) in app_jobs.into_iter().enumerate() {
        let Some(file) = upd::staged_package_file(k) else {
            break;
        };
        match stage_app(fs, net, urls[slots[i]], entry, file, &keys.stable) {
            Ok(e) => staged.push(e),
            Err(code) => result = code,
        }
    }
    if !write_state_section(fs, &state, lists::STAGED, &lists::encode_staged(&staged)) {
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

/// Whether this machine's boot loader trusts the development channel: the
/// flag AriBoot writes in the boot state on every boot (ADR 0045). Without
/// the A/B layout, or with a state written by a loader before ADR 0045,
/// it does not.
fn loader_trusts_dev() -> bool {
    let Ok(mut blk) = BlockClient::new(BLOCK, HW, CHUNK) else {
        return false;
    };
    let Some((first_lba, _)) = find_partition(&mut blk, PART_STATE) else {
        return false;
    };
    blk.read(first_lba, 1).is_ok()
        && BootState::decode(&blk.buffer()[..SECTOR]).is_some_and(|s| s.loader_dev_channel)
}

/// The keys of this run (ADR 0045): the release key, and the development
/// channel's key only when this build carries it and the machine's loader
/// trusts it too, so nothing of that channel is accepted that AriBoot
/// would refuse.
fn run_keys() -> Keys {
    let compiled = aricode_crypto::trust::dev_channel_key();
    upd::trusted_keys(
        aricode_crypto::trust::public_key(),
        compiled,
        compiled.is_some() && loader_trusts_dev(),
    )
}

/// Why the development channel is closed on this machine, said and
/// recorded (owner's rule: refusals are logged).
fn dev_not_trusted() -> u64 {
    let why = if aricode_crypto::trust::dev_channel_key().is_none() {
        "this build does not carry the development channel's key"
    } else {
        "this machine's boot loader does not trust the development channel (install it from a stick made with cargo xtask usb-image --dev-source URL)"
    };
    fault(
        code::DEV_NOT_TRUSTED,
        "upd-dev-not-trusted",
        &format!("development channel refused: {why}"),
    )
}

fn run(arg: u64) -> u64 {
    DEADLINE.store(now_ms().saturating_add(RUN_DEADLINE_MS), Ordering::Relaxed);
    let allow_downgrade = arg & updater_arg::ALLOW_DOWNGRADE != 0;
    let explicit = arg & updater_arg::CHANNEL_EXPLICIT != 0;
    let channel = updater_arg::channel(arg);
    let channel_flags = arg & (updater_arg::CHANNEL_DEV | updater_arg::CHANNEL_EXPLICIT);
    let command = arg & !updater_arg::FLAGS;
    // The development channel is always named (`--channel dev`).
    let channel_ok = channel == Channel::Stable || explicit;
    let valid = match command {
        updater_arg::APPLY => channel_flags == 0,
        updater_arg::ROLLBACK => arg == updater_arg::ROLLBACK,
        updater_arg::REFRESH => !allow_downgrade && channel_ok,
        updater_arg::UPGRADE => channel_ok,
        _ => false,
    };
    if !valid {
        return fault(
            code::BAD_ARGUMENT,
            "upd-bad-argument",
            &format!("unknown argument {arg}"),
        );
    }
    let keys = run_keys();
    match command {
        updater_arg::APPLY => return apply(allow_downgrade, &keys),
        updater_arg::ROLLBACK => {
            let mut fs = FsClient::new(FS, HW).ok();
            return rollback(fs.as_mut(), &keys);
        }
        _ => {}
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
    if !sources.has_channel(channel) {
        return match channel {
            Channel::Dev => fault(
                code::NO_SOURCES,
                "upd-no-dev-source",
                "no development source: the dev channel is closed on this machine (update add --channel dev URL opens it)",
            ),
            Channel::Stable => {
                say!("no stable update sources: update add URL");
                code::NO_SOURCES
            }
        };
    }
    if channel == Channel::Dev && keys.dev.is_none() {
        return dev_not_trusted();
    }
    if command == updater_arg::REFRESH {
        refresh(&mut fs, &mut net, &sources, &keys, channel)
    } else {
        upgrade(
            &mut fs,
            &mut net,
            &sources,
            &keys,
            channel,
            allow_downgrade,
            explicit,
        )
    }
}

fn main(arg: u64) -> u64 {
    let code = run(arg);
    // The record of this run (private directory).
    if let Ok(mut fs) = FsClient::new(FS, HW) {
        let log = LOG.0.borrow();
        if !write_file(&mut fs, upd::UPDATER_LOG_FILE, &log) {
            let _ = sys::event_record(code::FILE_WRITE, "upd-log-write");
        }
    }
    code
}

aricode_rt::entry!(main);
