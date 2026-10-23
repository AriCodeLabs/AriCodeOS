// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Update sources and the signed update index (ADR 0029).
//!
//! A *source* is a directory served over HTTP(S) that the owner registered
//! with `update add`. Each source publishes two files:
//!
//! ```text
//! index       what the source offers (text, at most MAX_INDEX bytes)
//! index.sig   Ed25519 signature of the exact bytes of `index`, in hex
//! ```
//!
//! ```text
//! # AriCodeOS update index (ADR 0029)
//! format = 1
//! serial = 1760659200
//! system = 3 aricodeos-3.img 2412544 <sha256 hex>
//! app = hello_ext 2 hello_ext-2.aripkg 10240 <sha256 hex>
//! ```
//!
//! `system` (at most one) and `app` (at most [`MAX_APPS`], one per name)
//! read `[name] version file size sha256`. `serial` grows with every
//! publication; an index older than the one already stored for the same
//! source is refused (rollback protection).
//!
//! Sources and indexes belong to a channel (ADR 0045): `stable` (releases,
//! signed with the release key) or `dev` (development builds, signed with
//! the development channel's key, opt-in per machine).
//!
//! This crate is the policy shared by the updater, the installer, the shell
//! and xtask: index format and verification ([`index`]), version and
//! downgrade rules ([`policy`]), the source list ([`sources`]), the stored
//! copy of a verified index ([`stored`]), optional-file reads that never
//! mistake a failure for absence ([`files`]), the lists the updater and the
//! installer exchange ([`lists`]), the end of a staged install ([`run`])
//! and the install history ([`history`]).

#![no_std]

extern crate alloc;

pub mod files;
pub mod history;
pub mod index;
pub mod lists;
pub mod policy;
pub mod run;
pub mod sources;
pub mod stored;
pub mod trust;

pub use aricode_ab::{Channel, Keys};
pub use index::{verify, verify_channel, AppEntry, Index, IndexError, SystemEntry};
pub use policy::{
    best_app, best_system, check_fresh, compare, compare_system, decide_system, raise_floor,
    Change, Refusal, SystemChange, SystemDecision, SystemId, SystemRules,
};
pub use sources::{normalize_source, Migration, Source, SourceError, Sources};
pub use trust::{image_matches_offer, trusted_keys};

/// Name of the index file inside a source.
pub const INDEX_FILE: &str = "index";
/// Name of the index signature file inside a source.
pub const SIGNATURE_FILE: &str = "index.sig";
/// Largest index accepted (bytes).
pub const MAX_INDEX: usize = 4096;
/// Most app entries in one index.
pub const MAX_APPS: usize = 16;
/// Most registered sources (each one keeps a stored index in AriFS, whose
/// table holds 32 files in all).
pub const MAX_SOURCES: usize = 4;
/// Longest source URL, normalized.
pub const MAX_SOURCE_LEN: usize = 128;
/// Index format this crate understands.
pub const FORMAT: u32 = 1;

/// The updater's private directory (its `fs` grant). The names below are
/// relative to it: the updater opens them as they are, the shell and the
/// installer (main file space) through [`main_path`].
pub const UPDATER_DIR: &str = "updater";
/// The source list (the shell edits it with `update add/remove`).
pub const SOURCES_FILE: &str = "sources";
/// The single channel URL of ADR 0027; migrated into [`SOURCES_FILE`].
pub const LEGACY_URL_FILE: &str = "update_url";
/// Install history: system installs (updater), app installs (installer),
/// source changes (shell).
pub const HISTORY_FILE: &str = "history.log";
/// The history keeps its most recent lines within this size.
pub const HISTORY_MAX: usize = 2048;
/// Output of the updater's last run, kept for later reading (`cat`, or
/// `fs-cat` on a dump of the disk).
pub const UPDATER_LOG_FILE: &str = "last.log";
/// The exchange between the updater and the installer, one file with three
/// sections ([`lists::section`]) so the AriFS table (32 entries in all) is
/// spared (audit 1, R3): `[installed]` (written by the installer before
/// every updater run), `[plan]` (what the last `update` reported; `upgrade`
/// installs nothing outside it) and `[staged]` (packages the last
/// `upgrade` staged; the installer empties it once consumed).
pub const STATE_FILE: &str = "state";
/// The verified copies of every source's index, one file
/// ([`stored::encode_all`]).
pub const INDEXES_FILE: &str = "indexes";
/// Most packages staged by one `upgrade` (each one is an AriFS file).
pub const MAX_STAGED: usize = 4;

/// `name` inside the updater's directory, as the main file space sees it.
pub fn main_path(name: &str) -> alloc::string::String {
    alloc::format!("{UPDATER_DIR}/{name}")
}

/// Where the installer keeps app `name`: `apps/NAME.aripkg`. `None` when
/// `name` is not a valid app name (decision 2: `[A-Za-z0-9_-]`, at most
/// [`index::MAX_APP_NAME`] bytes): a longer name would make a path the
/// file client refuses before asking the service, which the installer
/// could only report as a failure of the service (incident 0008 audit).
/// The path always fits an AriFS name ([`files::NAME_LEN`]).
pub fn installed_path(name: &str) -> Option<alloc::string::String> {
    if !index::valid_app_name(name) {
        return None;
    }
    let path = alloc::format!("apps/{name}{}", aricode_pkg::EXTENSION);
    debug_assert!(path.len() <= files::NAME_LEN);
    Some(path)
}

/// Staged package number `slot` (0-based): `pkg-1.aripkg` ... The installer
/// accepts no other name (ADR 0029, decision 5).
pub fn staged_package_file(slot: usize) -> Option<&'static str> {
    const FILES: [&str; MAX_STAGED] = [
        "pkg-1.aripkg",
        "pkg-2.aripkg",
        "pkg-3.aripkg",
        "pkg-4.aripkg",
    ];
    FILES.get(slot).copied()
}

/// Arguments of the `updater` program (the shell's `update`, `upgrade`).
pub mod updater_arg {
    /// Installs the image already in the UPDATE partition (Phase 30).
    pub const APPLY: u64 = 0;
    /// Fetches and verifies every source's index and reports; installs nothing.
    pub const REFRESH: u64 = 1;
    /// Installs what the stored indexes offer over the installed versions.
    pub const UPGRADE: u64 = 2;
    /// Schedules the image of the inactive slot to boot next, from the
    /// console, with no network (shell `update rollback`, ADR 0045).
    pub const ROLLBACK: u64 = 3;
    /// Flag for `UPGRADE` and `APPLY`: older versions (or an unknown
    /// installed version) may be replaced.
    pub const ALLOW_DOWNGRADE: u64 = 1 << 8;
    /// Flag for `REFRESH` and `UPGRADE`: act on the development channel's
    /// sources instead of the stable ones (ADR 0045).
    pub const CHANNEL_DEV: u64 = 1 << 9;
    /// Flag for `REFRESH` and `UPGRADE`: the owner named the channel
    /// (`--channel`); only then may `UPGRADE` move the system to the other
    /// channel.
    pub const CHANNEL_EXPLICIT: u64 = 1 << 10;
    /// Every flag bit.
    pub const FLAGS: u64 = ALLOW_DOWNGRADE | CHANNEL_DEV | CHANNEL_EXPLICIT;

    /// The channel `arg` acts on.
    pub fn channel(arg: u64) -> aricode_ab::Channel {
        if arg & CHANNEL_DEV != 0 {
            aricode_ab::Channel::Dev
        } else {
            aricode_ab::Channel::Stable
        }
    }
    /// Bit in the exit code of `UPGRADE`: verified packages were staged in
    /// the updater's directory for the installer. The other bits are the
    /// failure code (0 when everything went well).
    pub const EXIT_APPS_STAGED: u64 = 1 << 8;
}

/// Arguments of the `installer` program (flags). Without flags it installs
/// the packages left in `pkgs/` (ADR 0018).
pub mod installer_arg {
    /// With `STAGED` only: a package older than the installed one may
    /// replace it. Refused in any other mode.
    pub const ALLOW_DOWNGRADE: u64 = 1;
    /// Install only what the updater listed in `[staged]` of `updater/state`.
    pub const STAGED: u64 = 2;
    /// Only write the installed apps to `[installed]` of `updater/state`.
    pub const LIST: u64 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The longest valid app name gives a path of exactly the AriFS name
    /// length; one byte more, or a character the package rule refuses, is
    /// not a path at all (never a file-service failure).
    #[test]
    fn installed_path_fits_arifs_or_is_refused() {
        let longest = "a".repeat(index::MAX_APP_NAME);
        let path = installed_path(&longest).expect("valid");
        assert_eq!(path, alloc::format!("apps/{longest}.aripkg"));
        assert_eq!(path.len(), files::NAME_LEN);
        assert_eq!(
            installed_path("hello_ext").as_deref(),
            Some("apps/hello_ext.aripkg")
        );
        assert_eq!(installed_path(&"a".repeat(index::MAX_APP_NAME + 1)), None);
        assert_eq!(installed_path(&"a".repeat(aricode_pkg::MAX_NAME)), None);
        assert_eq!(installed_path(""), None);
        assert_eq!(installed_path("../etc"), None);
        assert_eq!(installed_path("a.b"), None);
    }
}
