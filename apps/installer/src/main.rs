// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `installer` (stage 4, ADR 0018, ADR 0029): trusted app that receives the
//! main fsd space (`install` capability) and installs or updates signed
//! packages: it verifies each one with the project key, compares its
//! version with the one installed at `apps/NAME.aripkg` and replaces it
//! with a single atomic `replace`.
//!
//! Argument (`aricode_update_index::installer_arg`):
//!
//! - no flag: the packages left in `pkgs/` (ADR 0018); never a downgrade.
//! - `STAGED`: only the packages the updater listed in `[staged]` of `updater/state`,
//!   from the fixed stage files of its directory, each checked against the
//!   SHA-256, name and version listed there; the list is emptied once
//!   consumed. `ALLOW_DOWNGRADE` (the shell's `upgrade --allow-downgrade`)
//!   is accepted only together with `STAGED` (ADR 0029, decision 5).
//! - `LIST`: only writes the installed apps to `[installed]` of `updater/state`, which
//!   is how the updater learns them without the main file space.
//!
//! Every install goes to the history (`updater/history.log`); every refusal
//! or failure is said and recorded as a fault event (ADR 0028).
//!
//! Files are read through `aricode_update_index::files` (incident 0008): a
//! request the file service does not answer is a failure, never "the file
//! is absent" or "the directory is empty". Whatever the installer would
//! have concluded from the missing answer (an empty state to rewrite, an
//! app not installed, nothing pending) is refused instead, with a fault.
//!
//! Memory: the DMA quota (64 KiB, manifest) only holds the file client's
//! 4 KiB buffer; packages live on the 256 KiB static heap of the runtime.
//! The peak is one package (at most 32 KiB) plus the installed copy read to
//! compare versions (at most 32 KiB, freed before the write) plus the
//! history (up to 4 x 2 KiB while it is trimmed) and a list (under 16 KiB):
//! under 80 KiB.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::sha256::sha256;
use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, kind, permissions};
use aricode_update_index::files::{self, Files, ReadError};
use aricode_update_index::{self as upd, compare, installer_arg, lists, Change};

/// An installed package is replaced in one go: the AriFS journal (32 KiB).
const MAX_PACKAGE: usize = 32 * 1024;
const MAX_PENDING: usize = 8;
/// Largest `updater/state` read (the three sections stay well under it).
const MAX_STATE: usize = 16 * 1024;
/// The history is kept within `HISTORY_MAX` by every append; a file above
/// this (four times that) was not written by this program and is a fault.
const MAX_HISTORY_READ: usize = 4 * upd::HISTORY_MAX;

/// Exit and fault event codes (ADR 0028).
mod code {
    pub const NO_SPACE: u64 = 1;
    pub const NO_CLIENT: u64 = 2;
    pub const OPEN: u64 = 4;
    pub const WRITE: u64 = 5;
    pub const BAD_FLAGS: u64 = 6;
    pub const VERIFY: u64 = 7;
    pub const DOWNGRADE: u64 = 8;
    pub const INSTALLED_INVALID: u64 = 9;
    pub const STAGE: u64 = 10;
    pub const LIST: u64 = 11;
    pub const STATE: u64 = 12;
    pub const STATE_REPAIRED: u64 = 13;
    /// The installed copy of an app could not be read (incident 0008): its
    /// version is unknown, so nothing replaces it and it is not listed.
    pub const INSTALLED_UNKNOWN: u64 = 14;
    /// The directory could not be listed (incident 0008): nothing is
    /// concluded about what is installed or pending.
    pub const DIR_LIST: u64 = 15;
    /// A pending or staged package could not be read (as opposed to
    /// missing, oversized or tampered).
    pub const PKG_READ: u64 = 16;
}

macro_rules! say {
    ($($arg:tt)*) => {
        sys::print(format!("installer: {}\n", format_args!($($arg)*)).as_bytes())
    };
}

/// Says `msg` and records the fault event.
fn fault(code: u64, event: &str, msg: &str) -> u64 {
    say!("{msg}");
    let _ = sys::event_record(code, event);
    code
}

/// The main file space as `files::Files`: only an explicit `NotFound` from
/// the service means a file is absent; a lost reply, a timeout or any other
/// status is an error the callers refuse to act on (incident 0008).
struct Main<'a>(&'a mut FsClient);

impl Files for Main<'_> {
    type Fd = u32;
    type Error = FsError;

    fn open(&mut self, name: &str) -> Result<Option<u32>, FsError> {
        match self.0.open(name) {
            Ok(fd) => Ok(Some(fd)),
            Err(FsError::Status(Status::NotFound)) => Ok(None),
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

type Read = Result<Option<Vec<u8>>, ReadError<FsError>>;

/// Reads a whole file (up to `max` bytes) from the main space. `Ok(None)`
/// only when it does not exist.
fn read_all(fs: &mut FsClient, name: &str, max: usize) -> Read {
    files::read_optional(&mut Main(fs), name, max)
}

/// The names in the main space for which `keep` holds; a failed listing is
/// a fault, never a shorter list.
fn list_names(fs: &mut FsClient, keep: impl FnMut(&str) -> bool) -> Result<Vec<String>, u64> {
    files::list_names(&mut Main(fs), keep).map_err(|e| {
        fault(
            code::DIR_LIST,
            "inst-dir-list",
            &format!("the directory cannot be listed ({e:?}): nothing concluded, nothing written"),
        )
    })
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

/// Appends a line to the install history (ADR 0029, decision 7). A history
/// that cannot be read is not rewritten: the install happened and was said
/// on the console, but the older lines are never replaced by one line.
fn history(fs: &mut FsClient, line: &str) {
    let path = upd::main_path(upd::HISTORY_FILE);
    let existing = match read_all(fs, &path, MAX_HISTORY_READ) {
        Ok(existing) => existing.unwrap_or_default(),
        Err(e) => {
            fault(
                code::WRITE,
                "inst-history-read",
                &format!(
                    "cannot read the install history ({e:?}): not rewritten; unrecorded: {line}"
                ),
            );
            return;
        }
    };
    let log = upd::history::append(&existing, line, upd::HISTORY_MAX);
    if !write_file(fs, &path, &log) {
        fault(
            code::WRITE,
            "inst-history-write",
            "cannot write the install history",
        );
    }
}

/// What is installed under a name.
enum Installed {
    Missing,
    Valid(aricode_pkg::PkgManifest),
    /// Present but fails verification (or is larger than any package can
    /// be): the broker refuses to launch it.
    Invalid,
    /// Present or not, nobody knows: the request failed (incident 0008).
    Unknown(FsError),
}

fn installed(fs: &mut FsClient, path: &str, pubkey: &[u8; 32]) -> Installed {
    match read_all(fs, path, MAX_PACKAGE) {
        Ok(None) => Installed::Missing,
        Err(ReadError::Failed(e)) => Installed::Unknown(e),
        Err(ReadError::TooLarge) => Installed::Invalid,
        Ok(Some(bytes)) => match aricode_pkg::verify(&bytes, pubkey) {
            Ok(v) => Installed::Valid(v.manifest),
            Err(_) => Installed::Invalid,
        },
    }
}

/// Installed apps that verify; the others are reported, never listed. A
/// listing or a read that fails is a fault and the list is not produced:
/// an app the installer could not see is not "not installed".
fn installed_apps(fs: &mut FsClient, pubkey: &[u8; 32]) -> Result<Vec<(String, u32)>, u64> {
    let paths = list_names(fs, |name| {
        name.starts_with("apps/") && name.ends_with(aricode_pkg::EXTENSION)
    })?;
    let mut apps = Vec::new();
    for path in paths {
        match installed(fs, &path, pubkey) {
            Installed::Valid(m) => apps.push((String::from(m.name_str()), m.version)),
            Installed::Unknown(e) => {
                return Err(fault(
                    code::INSTALLED_UNKNOWN,
                    "inst-installed-unknown",
                    &format!("{path} cannot be read ({e:?}): the installed list is not written"),
                ));
            }
            Installed::Missing | Installed::Invalid => {
                fault(
                    code::INSTALLED_INVALID,
                    "inst-installed-invalid",
                    &format!("{path} fails verification (it cannot be launched); not listed"),
                );
            }
        }
    }
    Ok(apps)
}

/// The exchange file with the updater (`updater/state`), empty only when
/// the service says it does not exist. A failed request or an oversized
/// file is a fault, and the file is then neither used nor rewritten (final
/// re-audit, L-a; incident 0008).
fn read_state(fs: &mut FsClient) -> Result<String, u64> {
    let path = upd::main_path(upd::STATE_FILE);
    let text = match files::read_text(&mut Main(fs), &path, MAX_STATE) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => {
            return Err(fault(
                code::STATE,
                "inst-state-unreadable",
                &format!("updater/state cannot be read ({e:?}): not used, not rewritten"),
            ));
        }
    };
    // Structural damage is repaired as `lists` documents and stored at
    // once (an atomic replace), so the fault says exactly what happened
    // (fix update-state-sections).
    if !lists::needs_repair(&text) {
        return Ok(text);
    }
    let fixed = lists::repaired(&text);
    if write_file(fs, &path, fixed.as_bytes()) {
        fault(
            code::STATE_REPAIRED,
            "inst-state-repaired",
            "updater/state was damaged (repeated or unknown sections): repaired and saved; a repeated plan or staged list was dropped",
        );
    } else {
        fault(
            code::STATE_REPAIRED,
            "inst-state-repaired",
            "updater/state was damaged (repeated or unknown sections): repaired for this run, but the repair could not be saved",
        );
    }
    Ok(fixed)
}

/// Replaces one section of the exchange file; refused when it could not
/// be read, so the other sections are never replaced by what a failed
/// read did not return.
fn write_state_section(fs: &mut FsClient, name: &str, body: &str) -> bool {
    let Ok(state) = read_state(fs) else {
        return false;
    };
    write_file(
        fs,
        &upd::main_path(upd::STATE_FILE),
        lists::with_section(&state, name, body).as_bytes(),
    )
}

/// `LIST`, and after every install: the installed apps for the updater.
fn write_installed(fs: &mut FsClient, pubkey: &[u8; 32]) -> u64 {
    let apps = match installed_apps(fs, pubkey) {
        Ok(apps) => apps,
        Err(code) => return code,
    };
    if write_state_section(fs, lists::INSTALLED, &lists::encode_installed(&apps)) {
        0
    } else {
        fault(
            code::LIST,
            "inst-list-write",
            "cannot write the installed list",
        )
    }
}

/// Installs one verified package (`bytes`, from `source`) unless the
/// version rule refuses it. `Ok(true)` when installed.
fn install_one(
    fs: &mut FsClient,
    bytes: &[u8],
    source: &str,
    allow_downgrade: bool,
    pubkey: &[u8; 32],
) -> Result<bool, u64> {
    let Ok(verified) = aricode_pkg::verify(bytes, pubkey) else {
        fault(
            code::VERIFY,
            "inst-pkg-verify",
            &format!("{source} fails verification: skipped"),
        );
        return Ok(false);
    };
    let app = verified.manifest.name_str();
    let version = verified.manifest.version;
    let target = format!("apps/{app}{}", aricode_pkg::EXTENSION);
    let current = match installed(fs, &target, pubkey) {
        Installed::Missing => None,
        Installed::Valid(m) => Some(m.version),
        Installed::Invalid => {
            // Never replaced silently (audit 1, M1): it is recorded, and the
            // new, verified package takes its place.
            fault(
                code::INSTALLED_INVALID,
                "inst-installed-invalid",
                &format!("{target} fails verification; replacing it with version {version}"),
            );
            None
        }
        Installed::Unknown(e) => {
            // ADR 0029, decision 5: what cannot be identified is not
            // replaced. A failed request is not a missing file (incident
            // 0008); the package is skipped and can be installed again.
            fault(
                code::INSTALLED_UNKNOWN,
                "inst-installed-unknown",
                &format!("{target} cannot be read ({e:?}): {app} version {version} refused (installed version unknown)"),
            );
            return Ok(false);
        }
    };
    let change = compare(current, version);
    if !change.permitted(allow_downgrade) {
        if change == Change::Downgrade {
            // ADR 0029, decision 5: no downgrade without the owner's
            // explicit `upgrade --allow-downgrade`.
            fault(
                code::DOWNGRADE,
                "inst-downgrade-refused",
                &format!(
                    "{app} version {version} is older than the installed {}: refused (downgrade)",
                    current.unwrap_or(0)
                ),
            );
        } else {
            say!(
                "{app} version {version} already installed (current {})",
                current.unwrap_or(0)
            );
        }
        return Ok(false);
    }
    say!("installing {app} version {version} (from {source})");
    let Ok(fd) = fs.open(&target).or_else(|_| fs.create(&target)) else {
        return Err(fault(
            code::OPEN,
            "inst-open",
            "could not open or create the installed package",
        ));
    };
    // A single atomic replace: after a power cut there is either the old
    // version or the new one, never a mix.
    let result = fs.replace(fd, bytes);
    let _ = fs.close(fd);
    if result != Ok(bytes.len()) {
        return Err(fault(
            code::WRITE,
            "inst-write",
            "error writing the installed package",
        ));
    }
    let from = current.map_or(String::from("none"), |v| format!("{v}"));
    history(
        fs,
        &format!("app {app} {from} -> {version} installed ({target})"),
    );
    say!("{app} {from} -> {version} installed ({target})");
    Ok(true)
}

/// No flag: the packages in `pkgs/` (ADR 0018), in directory order.
fn install_pending(fs: &mut FsClient, pubkey: &[u8; 32]) -> u64 {
    let mut pending = match list_names(fs, |name| {
        name.starts_with("pkgs/") && name.ends_with(aricode_pkg::EXTENSION)
    }) {
        Ok(pending) => pending,
        Err(code) => return code,
    };
    if pending.len() > MAX_PENDING {
        say!("too many pending packages; handling the first ones");
        pending.truncate(MAX_PENDING);
    }
    if pending.is_empty() {
        say!("nothing to install in pkgs/");
        return 0;
    }
    let (mut done, mut skipped) = (0u64, 0u64);
    for path in &pending {
        let bytes = match read_all(fs, path, MAX_PACKAGE) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(ReadError::TooLarge) => {
                say!("{path} cannot be read or is too large: skipped");
                skipped += 1;
                continue;
            }
            Err(ReadError::Failed(e)) => {
                fault(
                    code::PKG_READ,
                    "inst-pkg-read",
                    &format!("{path} cannot be read ({e:?}): skipped"),
                );
                skipped += 1;
                continue;
            }
        };
        match install_one(fs, &bytes, path, false, pubkey) {
            Ok(true) => done += 1,
            Ok(false) => skipped += 1,
            Err(code) => return code,
        }
    }
    say!("{done} package(s) installed, {skipped} skipped");
    write_installed(fs, pubkey)
}

/// `STAGED`: what the updater listed in the `[staged]` section of
/// `updater/state`, nothing else.
fn install_staged(fs: &mut FsClient, allow_downgrade: bool, pubkey: &[u8; 32]) -> u64 {
    let state = match read_state(fs) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let text = lists::section(&state, lists::STAGED).unwrap_or_default();
    let (entries, rejected) = lists::parse_staged(&text);
    let mut result = 0;
    if rejected > 0 {
        result = fault(
            code::STAGE,
            "inst-stage-path",
            &format!("{rejected} staged entry(ies) name another file or are malformed: refused"),
        );
    }
    let (mut done, mut skipped) = (0u64, 0u64);
    for e in &entries {
        let path = upd::main_path(e.file);
        let bytes = match read_all(fs, &path, MAX_PACKAGE) {
            Ok(Some(bytes)) if sha256(&bytes) == e.sha256 => bytes,
            Ok(_) | Err(ReadError::TooLarge) => {
                result = fault(
                    code::STAGE,
                    "inst-stage-hash",
                    &format!("{path} missing or not what the updater staged: refused"),
                );
                skipped += 1;
                continue;
            }
            Err(ReadError::Failed(err)) => {
                result = fault(
                    code::PKG_READ,
                    "inst-pkg-read",
                    &format!("{path} cannot be read ({err:?}): refused"),
                );
                skipped += 1;
                continue;
            }
        };
        match aricode_pkg::verify(&bytes, pubkey) {
            Ok(v) if v.manifest.name_str() == e.name && v.manifest.version == e.version => {}
            _ => {
                result = fault(
                    code::VERIFY,
                    "inst-pkg-verify",
                    &format!("{path} fails verification or differs from the staged list: refused"),
                );
                skipped += 1;
                continue;
            }
        }
        match install_one(fs, &bytes, &path, allow_downgrade, pubkey) {
            Ok(true) => done += 1,
            Ok(false) => skipped += 1,
            Err(code) => {
                result = code;
                break;
            }
        }
    }
    // Consumed: a later run (with or without --allow-downgrade) cannot
    // install the same files again.
    if !write_state_section(fs, lists::STAGED, "") {
        result = fault(
            code::WRITE,
            "inst-stage-clear",
            "cannot empty the staged list",
        );
    }
    say!("{done} package(s) installed, {skipped} skipped");
    let listed = write_installed(fs, pubkey);
    if result != 0 {
        result
    } else {
        listed
    }
}

fn main(arg: u64) -> u64 {
    let (mut root, mut hardware) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            root = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (root, hardware) else {
        return fault(
            code::NO_SPACE,
            "inst-no-space",
            "need the main file space (install capability)",
        );
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return fault(code::NO_CLIENT, "inst-no-client", "no file client");
    };
    let pubkey = aricode_crypto::trust::public_key();
    let allow_downgrade = arg & installer_arg::ALLOW_DOWNGRADE != 0;
    let staged = arg & installer_arg::STAGED != 0;
    let list = arg & installer_arg::LIST != 0;
    let known = installer_arg::ALLOW_DOWNGRADE | installer_arg::STAGED | installer_arg::LIST;
    if arg & !known != 0 || (allow_downgrade && !staged) || (list && arg != installer_arg::LIST) {
        // ADR 0029, decision 5: a downgrade only for what the updater
        // staged from a verified index, never for whatever is in pkgs/.
        return fault(
            code::BAD_FLAGS,
            "inst-flags-refused",
            &format!("argument {arg} refused (--allow-downgrade only applies to staged packages)"),
        );
    }
    if list {
        write_installed(&mut fs, &pubkey)
    } else if staged {
        install_staged(&mut fs, allow_downgrade, &pubkey)
    } else {
        install_pending(&mut fs, &pubkey)
    }
}

aricode_rt::entry!(main);
