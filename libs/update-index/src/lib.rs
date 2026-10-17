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
//! This crate is the policy shared by the updater, the installer, the shell
//! and xtask: index format and verification ([`index`]), version and
//! downgrade rules ([`policy`]), the source list ([`sources`]), the stored
//! copy of a verified index ([`stored`]) and the install history
//! ([`history`]).

#![no_std]

extern crate alloc;

pub mod history;
pub mod index;
pub mod policy;
pub mod sources;
pub mod stored;

pub use index::{verify, AppEntry, Index, IndexError, SystemEntry};
pub use policy::{best_app, best_system, check_fresh, compare, Change};
pub use sources::{normalize_source, SourceError, Sources};

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

/// The source list in AriFS (the updater's private directory; the shell
/// edits it with `update add/remove`).
pub const SOURCES_FILE: &str = "updater/sources";
/// The single channel URL of ADR 0027; migrated into [`SOURCES_FILE`].
pub const LEGACY_URL_FILE: &str = "updater/update_url";
/// Install history shared by the updater and the installer (AriFS root).
pub const HISTORY_FILE: &str = "updates.log";
/// The history keeps its most recent lines within this size.
pub const HISTORY_MAX: usize = 2048;
/// Output of the updater's last run (shown to remote sessions).
pub const UPDATER_LOG_FILE: &str = "updater/last.log";

/// Stored, verified index of the source in position `slot` (0-based) of
/// the list (`updater/index-1` ...).
pub fn stored_index_file(slot: usize) -> Option<&'static str> {
    const FILES: [&str; MAX_SOURCES] = [
        "updater/index-1",
        "updater/index-2",
        "updater/index-3",
        "updater/index-4",
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
    /// Flag for `UPGRADE`: older versions may replace newer ones.
    pub const ALLOW_DOWNGRADE: u64 = 1 << 8;
    /// Exit code of `UPGRADE` when it left verified packages in `pkgs/` for
    /// the installer (0 means nothing was staged).
    pub const EXIT_APPS_STAGED: u64 = 100;
}

/// Arguments of the `installer` program (flags).
pub mod installer_arg {
    /// A package older than the installed one may replace it.
    pub const ALLOW_DOWNGRADE: u64 = 1;
}
