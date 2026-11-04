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
//! Packages are never held whole (ADR 0072, phase B): each is verified
//! piece by piece (`aricode_pkg::verify_streaming`), and installed by
//! copying it 4 KiB at a time into a chunked replace of `apps/NAME.aripkg`
//! that is committed only at the end, and only if a staged package's bytes
//! hash to what the updater listed. A volume still in AriFS v1 replaces at
//! most 32 KiB, and says so.
//!
//! Memory: the DMA quota (64 KiB, manifest) only holds the file client's
//! 4 KiB buffer; the 256 KiB static heap holds a 4 KiB copy buffer, the
//! history (up to 4 x 2 KiB while it is trimmed) and a list (under 16 KiB).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::sha256::Sha256;
use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, kind, permissions};
/// The main file space as `files::Files`.
use aricode_update_index::client::FsFiles as Main;
use aricode_update_index::files::{self, Files, ReadError};
use aricode_update_index::run::{self, Outcome};
use aricode_update_index::{self as upd, compare, installer_arg, lists, Change};

/// The package limit the whole system shares (incident 0026, ADR 0072).
const MAX_PACKAGE: usize = aricode_pkg::PACKAGE_MAX;
/// Bytes copied per chunk of an install: the file client's buffer.
const PIECE: usize = 4096;
const MAX_PENDING: usize = 8;
/// Largest `updater/state` read (the three sections stay well under it).
const MAX_STATE: usize = 16 * 1024;

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
    /// The package needs a newer application interface than this system
    /// offers (ADR 0074).
    pub const REQUIRES: u64 = 17;
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
    let log = match upd::history::appended(&mut Main(fs), &path, line, upd::HISTORY_MAX) {
        Ok(log) => log,
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
    if !write_file(fs, &path, &log) {
        fault(
            code::WRITE,
            "inst-history-write",
            "cannot write the install history",
        );
    }
}

/// What a package file holds, checked piece by piece.
enum Installed {
    Missing,
    /// Verified; its size.
    Valid(aricode_pkg::PkgManifest, u64),
    /// Present but fails verification: the broker refuses to launch it.
    Invalid,
    /// Present but larger than `MAX_PACKAGE`, so not a package this system
    /// installs, launches or upgrades (incident 0026).
    TooLarge,
    /// Present or not, nobody knows: the request failed (incident 0008).
    Unknown(FsError),
}

/// Verifies the package at `path` without holding it (ADR 0072, phase B).
fn installed(fs: &mut FsClient, path: &str, pubkey: &[u8; 32]) -> Installed {
    let mut main = Main(fs);
    let size = match files::size_of(&mut main, path) {
        Ok(None) => return Installed::Missing,
        Ok(Some(size)) => size,
        Err(e) => return Installed::Unknown(size_error(e)),
    };
    if size > MAX_PACKAGE as u64 {
        return Installed::TooLarge;
    }
    let fd = match main.open(path) {
        Ok(Some(fd)) => fd,
        Ok(None) => return Installed::Missing,
        Err(e) => return Installed::Unknown(e),
    };
    let mut failed = None;
    let verified = aricode_pkg::verify_streaming(
        size as usize,
        |offset, buf| match files::read_exact(&mut main, &fd, offset as u32, buf) {
            Ok(true) => Ok(()),
            Ok(false) => Err(()),
            Err(e) => {
                failed = Some(e);
                Err(())
            }
        },
        pubkey,
    );
    main.close(fd);
    match (verified, failed) {
        (_, Some(e)) => Installed::Unknown(e),
        (Ok(v), None) => Installed::Valid(v.manifest, size),
        (Err(_), None) => Installed::Invalid,
    }
}

/// A size that could not be known, as the file client's error: a listing
/// that kept missing a file that opens is a service too busy changing to
/// answer (it never means "absent", audit of ADR 0072 phase B, M1).
fn size_error(e: files::SizeError<FsError>) -> FsError {
    match e {
        files::SizeError::Failed(e) => e,
        files::SizeError::Unlisted => FsError::Status(Status::Busy),
    }
}

/// SHA-256 of the first `size` bytes of `path`, read piece by piece;
/// `Ok(None)` if it is missing or shorter.
fn file_sha256(fs: &mut FsClient, path: &str, size: u64) -> Result<Option<[u8; 32]>, FsError> {
    let mut main = Main(fs);
    let Some(fd) = main.open(path)? else {
        return Ok(None);
    };
    let mut h = Sha256::new();
    let mut piece = alloc::vec![0u8; PIECE];
    let mut done = 0u64;
    let mut result = Ok(true);
    while done < size {
        let n = (size - done).min(PIECE as u64) as usize;
        result = files::read_exact(&mut main, &fd, done as u32, &mut piece[..n]);
        if result != Ok(true) {
            break;
        }
        h.update(&piece[..n]);
        done += n as u64;
    }
    main.close(fd);
    match result {
        Ok(true) => Ok(Some(h.finish())),
        Ok(false) => Ok(None),
        Err(e) => Err(e),
    }
}

/// How a copy into an installed package ended.
#[derive(PartialEq)]
enum Copied {
    Installed,
    /// The bytes did not hash to what the updater staged: nothing changed.
    Mismatch,
    /// The volume is still AriFS v1, whose replace holds 32 KiB.
    V1Limit,
    /// `source` is gone since it was verified.
    Gone,
    /// `source` or `target` could not be opened (or created).
    OpenFailed,
    Failed,
}

/// Copies `size` bytes of `source` into `target` with one chunked replace,
/// committed only once every byte is there and, with `expected`, hashes to
/// it: after a power cut `target` holds the old package or the new one.
fn copy_package(
    fs: &mut FsClient,
    source: &str,
    target: &str,
    size: u64,
    expected: Option<[u8; 32]>,
) -> Copied {
    let src = match fs.open(source) {
        Ok(fd) => fd,
        Err(FsError::Status(Status::NotFound)) => return Copied::Gone,
        Err(_) => return Copied::OpenFailed,
    };
    let Ok(dst) = fs.open(target).or_else(|_| fs.create(target)) else {
        let _ = fs.close(src);
        return Copied::OpenFailed;
    };
    let mut h = Sha256::new();
    let mut piece = alloc::vec![0u8; PIECE];
    let mut done = 0u64;
    let mut copied = Copied::Installed;
    while done < size {
        let n = (size - done).min(PIECE as u64) as usize;
        if files::read_exact(&mut Main(fs), &src, done as u32, &mut piece[..n]) != Ok(true) {
            copied = Copied::Failed;
            break;
        }
        h.update(&piece[..n]);
        match fs.replace_chunk(dst, done as u32, &piece[..n]) {
            Ok(()) => {}
            Err(FsError::Status(Status::OutOfRange)) => {
                copied = Copied::V1Limit;
                break;
            }
            Err(_) => {
                copied = Copied::Failed;
                break;
            }
        }
        done += n as u64;
    }
    if copied == Copied::Installed && expected.is_some_and(|e| e != h.finish()) {
        copied = Copied::Mismatch;
    }
    if copied == Copied::Installed && fs.replace_commit(dst, size as u32) != Ok(size as usize) {
        copied = Copied::Failed;
    }
    // Closing a descriptor with an uncommitted replace abandons it.
    let _ = fs.close(dst);
    let _ = fs.close(src);
    copied
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
            Installed::Valid(m, _) => apps.push((String::from(m.name_str()), m.version)),
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
            Installed::TooLarge => {
                fault(
                    code::INSTALLED_INVALID,
                    "inst-installed-size",
                    &format!(
                        "{path} is larger than the {MAX_PACKAGE} bytes a package holds (it cannot be launched or upgraded); not listed"
                    ),
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

/// Replaces one section of the exchange file, whose text `state` was read
/// once at the start of the run (`read_state`): the other sections are
/// never replaced by what a failed read did not return. On success `state`
/// becomes the text written, so a later section write builds on it.
fn write_state_section(fs: &mut FsClient, state: &mut String, name: &str, body: &str) -> bool {
    let text = lists::with_section(state, name, body);
    let ok = write_file(fs, &upd::main_path(upd::STATE_FILE), text.as_bytes());
    if ok {
        *state = text;
    }
    ok
}

/// `LIST`, and after every install: the installed apps for the updater.
fn write_installed(fs: &mut FsClient, state: &mut String, pubkey: &[u8; 32]) -> u64 {
    let apps = match installed_apps(fs, pubkey) {
        Ok(apps) => apps,
        Err(code) => return code,
    };
    if write_state_section(fs, state, lists::INSTALLED, &lists::encode_installed(&apps)) {
        0
    } else {
        fault(
            code::LIST,
            "inst-list-write",
            "cannot write the installed list",
        )
    }
}

/// Installs the package at `source`, of `size` bytes and verified as
/// `manifest`, unless the version rule refuses it; with `expected`, only if
/// its bytes still hash to it.
fn install_one(
    fs: &mut FsClient,
    source: &str,
    manifest: &aricode_pkg::PkgManifest,
    size: u64,
    expected: Option<[u8; 32]>,
    allow_downgrade: bool,
    pubkey: &[u8; 32],
) -> Result<Done, u64> {
    let app = manifest.name_str();
    let version = manifest.version;
    // ADR 0029, decision 2: a name over MAX_APP_NAME bytes makes a path the
    // file client refuses before asking the service, which could only be
    // reported as a failure of the service: refused here, as a package.
    let Some(target) = upd::installed_path(app) else {
        fault(
            code::VERIFY,
            "inst-pkg-name",
            &format!(
                "{source}: package name {app:?} is not a valid app name (at most {} bytes, [A-Za-z0-9_-]): refused",
                upd::index::MAX_APP_NAME
            ),
        );
        return Ok(Done::Refused(0));
    };
    // ADR 0074: a package for a newer system never replaces one that runs
    // here; the owner upgrades the system first.
    if !manifest.runs_on(aricode_ab::APPS_LEVEL) {
        return Ok(Done::Refused(fault(
            code::REQUIRES,
            "inst-requires-newer",
            &format!(
                "{app} version {version} needs application interface {} and this system offers {}: refused (upgrade the system first)",
                manifest.requires,
                aricode_ab::APPS_LEVEL
            ),
        )));
    }
    let current = match installed(fs, &target, pubkey) {
        Installed::Missing => None,
        Installed::Valid(m, _) => Some(m.version),
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
        Installed::TooLarge => {
            fault(
                code::INSTALLED_INVALID,
                "inst-installed-size",
                &format!(
                    "{target} is larger than the {MAX_PACKAGE} bytes a package holds; replacing it with version {version}"
                ),
            );
            None
        }
        Installed::Unknown(e) => {
            // ADR 0029, decision 5: what cannot be identified is not
            // replaced. A failed request is not a missing file (incident
            // 0008): this is a failure of the run (nonzero exit), not a
            // refusal, and the package can be installed again.
            return Err(fault(
                code::INSTALLED_UNKNOWN,
                "inst-installed-unknown",
                &format!("{target} cannot be read ({e:?}): {app} version {version} refused (installed version unknown)"),
            ));
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
        return Ok(Done::Refused(0));
    }
    say!("installing {app} version {version} (from {source})");
    // A single atomic replace: after a power cut there is either the old
    // version or the new one, never a mix.
    match copy_package(fs, source, &target, size, expected) {
        Copied::Installed => {}
        // Refusals with their codes, as when found before the copy (audit
        // of ADR 0072 phase B, L1).
        Copied::Mismatch => {
            return Ok(Done::Refused(fault(
                code::STAGE,
                "inst-stage-hash",
                &format!("{source} changed while it was installed: refused"),
            )));
        }
        Copied::V1Limit => {
            return Ok(Done::Refused(fault(
                code::WRITE,
                "inst-v1-limit",
                &format!(
                    "{source} ({size} bytes) did not fit the replace of {target}: ARI0 is still AriFS v1, whose files hold 32768 (a stick written from 0.18 on is v2): refused"
                ),
            )));
        }
        Copied::Gone => {
            say!("{source} is gone: skipped");
            return Ok(Done::Refused(0));
        }
        Copied::OpenFailed => {
            return Err(fault(
                code::OPEN,
                "inst-open",
                "could not open the package or open or create the installed one",
            ));
        }
        Copied::Failed => {
            return Err(fault(
                code::WRITE,
                "inst-write",
                "error writing the installed package",
            ));
        }
    }
    let from = current.map_or(String::from("none"), |v| format!("{v}"));
    history(
        fs,
        &format!("app {app} {from} -> {version} installed ({target})"),
    );
    say!("{app} {from} -> {version} installed ({target})");
    Ok(Done::Installed)
}

/// What `install_one` did.
enum Done {
    Installed,
    /// Refused, with the fault's code (0: not a fault, as the same version).
    Refused(u64),
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
        let (manifest, size) = match installed(fs, path, pubkey) {
            Installed::Valid(manifest, size) => (manifest, size),
            Installed::Missing => {
                say!("{path} is gone: skipped");
                skipped += 1;
                continue;
            }
            Installed::Invalid => {
                fault(
                    code::VERIFY,
                    "inst-pkg-verify",
                    &format!("{path} fails verification: skipped"),
                );
                skipped += 1;
                continue;
            }
            Installed::TooLarge => {
                fault(
                    code::VERIFY,
                    "inst-pkg-size",
                    &format!(
                        "{path} is larger than the {MAX_PACKAGE} bytes a package holds: skipped"
                    ),
                );
                skipped += 1;
                continue;
            }
            Installed::Unknown(e) => {
                fault(
                    code::PKG_READ,
                    "inst-pkg-read",
                    &format!("{path} cannot be read ({e:?}): skipped"),
                );
                skipped += 1;
                continue;
            }
        };
        // The bytes committed are the bytes hashed here, right after the
        // verification (audit of ADR 0072 phase B, I2).
        let expected = match file_sha256(fs, path, size) {
            Ok(Some(h)) => h,
            Ok(None) => {
                say!("{path} is gone: skipped");
                skipped += 1;
                continue;
            }
            Err(e) => {
                fault(
                    code::PKG_READ,
                    "inst-pkg-read",
                    &format!("{path} cannot be read ({e:?}): skipped"),
                );
                skipped += 1;
                continue;
            }
        };
        match install_one(fs, path, &manifest, size, Some(expected), false, pubkey) {
            Ok(Done::Installed) => done += 1,
            Ok(Done::Refused(_)) => skipped += 1,
            Err(code) => return code,
        }
    }
    say!("{done} package(s) installed, {skipped} skipped");
    match read_state(fs) {
        Ok(mut state) => write_installed(fs, &mut state, pubkey),
        Err(code) => code,
    }
}

/// `STAGED`: what the updater listed in the `[staged]` section of
/// `updater/state`, nothing else.
fn install_staged(fs: &mut FsClient, allow_downgrade: bool, pubkey: &[u8; 32]) -> u64 {
    let mut state = match read_state(fs) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let text = lists::section(&state, lists::STAGED).unwrap_or_default();
    let (entries, rejected) = lists::parse_staged(&text);
    // What happens to the list and the exit code is `run::settle_staged`
    // (host-tested): refusals consume the list, a failure keeps it.
    let mut prior = 0;
    if rejected > 0 {
        prior = fault(
            code::STAGE,
            "inst-stage-path",
            &format!("{rejected} staged entry(ies) name another file or are malformed: refused"),
        );
    }
    let mut outcomes: Vec<Outcome> = Vec::new();
    for e in &entries {
        let path = upd::main_path(e.file);
        let hashed = match files::size_of(&mut Main(fs), &path) {
            Ok(Some(size)) if size > MAX_PACKAGE as u64 => Err(ReadError::TooLarge),
            Ok(Some(size)) => file_sha256(fs, &path, size)
                .map(|h| h.filter(|h| *h == e.sha256))
                .map_err(ReadError::Failed),
            Ok(None) => Ok(None),
            Err(err) => Err(ReadError::Failed(size_error(err))),
        };
        match hashed {
            Ok(Some(_)) => {}
            Err(ReadError::TooLarge) => {
                outcomes.push(Outcome::Refused(fault(
                    code::STAGE,
                    "inst-stage-size",
                    &format!(
                        "{path} is larger than the {MAX_PACKAGE} bytes a package holds: refused"
                    ),
                )));
                continue;
            }
            Ok(_) => {
                outcomes.push(Outcome::Refused(fault(
                    code::STAGE,
                    "inst-stage-hash",
                    &format!("{path} missing or not what the updater staged: refused"),
                )));
                continue;
            }
            Err(ReadError::Failed(err)) => {
                outcomes.push(Outcome::Failed(fault(
                    code::PKG_READ,
                    "inst-pkg-read",
                    &format!("{path} cannot be read ({err:?}): refused"),
                )));
                continue;
            }
        };
        let (manifest, size) = match installed(fs, &path, pubkey) {
            Installed::Valid(m, size) if m.name_str() == e.name && m.version == e.version => {
                (m, size)
            }
            _ => {
                outcomes.push(Outcome::Refused(fault(
                    code::VERIFY,
                    "inst-pkg-verify",
                    &format!("{path} fails verification or differs from the staged list: refused"),
                )));
                continue;
            }
        };
        match install_one(
            fs,
            &path,
            &manifest,
            size,
            Some(e.sha256),
            allow_downgrade,
            pubkey,
        ) {
            Ok(Done::Installed) => outcomes.push(Outcome::Installed),
            // Refused by `install_one`, with its fault's code (the same
            // version is no fault: 0).
            Ok(Done::Refused(code)) => outcomes.push(Outcome::Refused(code)),
            Err(code) => {
                outcomes.push(Outcome::Failed(code));
                break;
            }
        }
    }
    let end = run::settle_staged(prior, &outcomes);
    let mut result = end.code;
    if !end.consume {
        say!("staged list kept for inspection in updater/state (a failure, not a refusal); upgrade stages again");
    } else if write_state_section(fs, &mut state, lists::STAGED, "") {
        // Consumed: the stage files are scratch now. Emptied so they do not
        // keep up to MAX_STAGED packages of PACKAGE_MAX bytes on the
        // volume (audit of ADR 0074 phase A, L3); a failure only leaves
        // the bytes until the next upgrade overwrites them.
        for e in &entries {
            let path = upd::main_path(e.file);
            if let Ok(fd) = fs.open(&path) {
                let _ = fs.replace(fd, &[]);
                let _ = fs.close(fd);
            }
        }
    } else {
        // The list stays: a later run (with or without --allow-downgrade)
        // could install the same files again, so this is a failure.
        result = fault(
            code::WRITE,
            "inst-stage-clear",
            "cannot empty the staged list",
        );
        // `state` only follows an acknowledged write. The clear's reply
        // may have been lost after the replace landed: writing
        // `[installed]` from the old text would bring the list back, so
        // the state is read again, and nothing more is written when that
        // fails too (one more fault, after a failed write, is not noise).
        match read_state(fs) {
            Ok(fresh) => state = fresh,
            Err(_) => return result,
        }
    }
    say!(
        "{} package(s) installed, {} skipped",
        end.installed,
        end.skipped
    );
    let listed = write_installed(fs, &mut state, pubkey);
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
        match read_state(&mut fs) {
            Ok(mut state) => write_installed(&mut fs, &mut state, &pubkey),
            Err(code) => code,
        }
    } else if staged {
        install_staged(&mut fs, allow_downgrade, &pubkey)
    } else {
        install_pending(&mut fs, &pubkey)
    }
}

aricode_rt::entry!(main);
