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
//! Memory: the DMA quota (64 KiB, manifest) only holds the file client's
//! 4 KiB buffer; packages live on the 256 KiB static heap of the runtime.
//! The peak is one package (at most 32 KiB) plus the installed copy read to
//! compare versions (at most 32 KiB, freed before the write) plus the
//! history (2 x 2 KiB) and a list (under 4 KiB): under 80 KiB.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::sha256::sha256;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions};
use aricode_update_index::{self as upd, compare, installer_arg, lists, Change};
use core::sync::atomic::{AtomicBool, Ordering};

/// An installed package is replaced in one go: the AriFS journal (32 KiB).
const MAX_PACKAGE: u64 = 32 * 1024;
const MAX_PENDING: usize = 8;

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

/// Size of `name` from the directory listing.
fn file_size(fs: &FsClient, name: &str) -> Option<u64> {
    let mut entry = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, size))) = fs.list(i, &mut entry) {
        if &entry[..n] == name.as_bytes() {
            return Some(size);
        }
        i += 1;
    }
    None
}

/// Reads a whole file (up to `max` bytes) from the main space.
fn read_all(fs: &mut FsClient, name: &str, max: u64) -> Option<Vec<u8>> {
    let size = file_size(fs, name)?;
    if size > max {
        return None;
    }
    let fd = fs.open(name).ok()?;
    let mut data = alloc::vec![0u8; size as usize];
    let mut done = 0;
    while done < data.len() {
        match fs.read(fd, done as u32, &mut data[done..]) {
            Ok(n) if n > 0 => done += n,
            _ => {
                let _ = fs.close(fd);
                return None;
            }
        }
    }
    let _ = fs.close(fd);
    Some(data)
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

/// Appends a line to the install history (ADR 0029, decision 7).
fn history(fs: &mut FsClient, line: &str) {
    let path = upd::main_path(upd::HISTORY_FILE);
    let existing = read_all(fs, &path, upd::HISTORY_MAX as u64).unwrap_or_default();
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
    Valid(u32),
    /// Present but fails verification: the broker refuses to launch it.
    Invalid,
}

fn installed(fs: &mut FsClient, path: &str, pubkey: &[u8; 32]) -> Installed {
    if file_size(fs, path).is_none() {
        return Installed::Missing;
    }
    match read_all(fs, path, MAX_PACKAGE).and_then(|bytes| {
        aricode_pkg::verify(&bytes, pubkey)
            .ok()
            .map(|v| v.manifest.version)
    }) {
        Some(v) => Installed::Valid(v),
        None => Installed::Invalid,
    }
}

/// Installed apps that verify; the others are reported, never listed.
fn installed_apps(fs: &mut FsClient, pubkey: &[u8; 32]) -> Vec<(String, u32)> {
    let mut paths = Vec::new();
    let mut entry = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, _))) = fs.list(i, &mut entry) {
        i += 1;
        if let Ok(name) = core::str::from_utf8(&entry[..n]) {
            if name.starts_with("apps/") && name.ends_with(aricode_pkg::EXTENSION) {
                paths.push(String::from(name));
            }
        }
    }
    let mut apps = Vec::new();
    for path in paths {
        match read_all(fs, &path, MAX_PACKAGE)
            .as_deref()
            .map(|bytes| aricode_pkg::verify(bytes, pubkey).map(|v| v.manifest))
        {
            Some(Ok(m)) => apps.push((String::from(m.name_str()), m.version)),
            _ => {
                fault(
                    code::INSTALLED_INVALID,
                    "inst-installed-invalid",
                    &format!("{path} fails verification (it cannot be launched); not listed"),
                );
            }
        }
    }
    apps
}

/// The exchange file with the updater (`updater/state`), empty if missing.
/// A read error or an oversized file is a fault, and the file is then
/// neither used nor rewritten (final re-audit, L-a).
fn read_state(fs: &mut FsClient) -> Result<String, u64> {
    let path = upd::main_path(upd::STATE_FILE);
    if file_size(fs, &path).is_none() {
        return Ok(String::new());
    }
    let text = read_all(fs, &path, 16 * 1024)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .ok_or_else(|| {
            fault(
                code::STATE,
                "inst-state-unreadable",
                "updater/state cannot be read or is too large: not used, not rewritten",
            )
        })?;
    // A damaged file is read without misattributing lines and rewritten
    // canonically by the next write; recorded once per run (fix
    // update-state-sections).
    if lists::needs_repair(&text) && !STATE_REPAIR_RECORDED.swap(true, Ordering::Relaxed) {
        fault(
            code::STATE_REPAIRED,
            "inst-state-repaired",
            "updater/state was damaged (repeated or unknown sections): recovered and rewritten canonically",
        );
    }
    Ok(text)
}

/// The state repair fault is recorded once per run.
static STATE_REPAIR_RECORDED: AtomicBool = AtomicBool::new(false);

/// Replaces one section of the exchange file; refused when it could not
/// be read.
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
    let apps = installed_apps(fs, pubkey);
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
        Installed::Valid(v) => Some(v),
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
    let mut pending: Vec<String> = Vec::new();
    let mut entry = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, _))) = fs.list(i, &mut entry) {
        i += 1;
        let name = &entry[..n];
        if name.starts_with(b"pkgs/") && name.ends_with(aricode_pkg::EXTENSION.as_bytes()) {
            if pending.len() == MAX_PENDING {
                say!("too many pending packages; handling the first ones");
                break;
            }
            if let Ok(name) = core::str::from_utf8(name) {
                pending.push(String::from(name));
            }
        }
    }
    if pending.is_empty() {
        say!("nothing to install in pkgs/");
        return 0;
    }
    let (mut done, mut skipped) = (0u64, 0u64);
    for path in &pending {
        let Some(bytes) = read_all(fs, path, MAX_PACKAGE) else {
            say!("{path} cannot be read or is too large: skipped");
            skipped += 1;
            continue;
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
        let checked = read_all(fs, &path, MAX_PACKAGE).filter(|b| sha256(b) == e.sha256);
        let Some(bytes) = checked else {
            result = fault(
                code::STAGE,
                "inst-stage-hash",
                &format!("{path} missing or not what the updater staged: refused"),
            );
            skipped += 1;
            continue;
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
