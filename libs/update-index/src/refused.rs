// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Packages this system already downloaded and refused for needing a newer
//! application interface (`updater/refused`, ADR 0074): one line per
//! package, keyed by the SHA-256 the signed index gave for it.
//!
//! ```text
//! editor 4 <sha256> 2
//! ```
//!
//! The index does not carry `requires`, so without this list every
//! `update` would plan such a package again and every `upgrade` would
//! download it only to refuse it (audit of ADR 0074 phase A, L4). A
//! package with other bytes (another SHA-256) is not this one and is
//! downloaded; a line whose level this system now offers no longer holds
//! anything back.

use crate::index::{valid_app_name, AppEntry};
use crate::MAX_APPS;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::unhex;
use core::fmt::Write;

/// A refused package: its name, version and SHA-256 as the signed index
/// gave them, and the level its signed manifest required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub name: String,
    pub version: u32,
    pub sha256: [u8; 32],
    pub requires: u32,
}

/// The list, at most `MAX_APPS` entries (one per app name).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefusedList {
    list: Vec<Refused>,
}

impl RefusedList {
    /// Parses the file; returns the list and how many lines were refused
    /// (malformed, a repeated name, past `MAX_APPS`), never acted upon.
    pub fn parse(text: &str) -> (Self, usize) {
        let mut out = Self::default();
        let mut rejected = 0;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match parse_line(line) {
                Some(r)
                    if !out.list.iter().any(|e| e.name == r.name) && out.list.len() < MAX_APPS =>
                {
                    out.list.push(r)
                }
                _ => rejected += 1,
            }
        }
        (out, rejected)
    }

    /// The level `entry` is known to need, if this exact package (name,
    /// version and SHA-256) was refused for a level above `level`.
    pub fn holds_back(&self, entry: &AppEntry, level: u32) -> Option<u32> {
        self.list
            .iter()
            .find(|r| {
                r.name == entry.name && r.version == entry.version && r.sha256 == entry.sha256
            })
            .map(|r| r.requires)
            .filter(|&requires| requires > level)
    }

    /// Records a refusal, replacing any line for the same app.
    pub fn record(&mut self, refused: Refused) {
        self.list.retain(|r| r.name != refused.name);
        if self.list.len() < MAX_APPS {
            self.list.push(refused);
        }
    }

    /// Drops the lines `level` satisfies: they hold nothing back any more.
    pub fn prune(&mut self, level: u32) {
        self.list.retain(|r| r.requires > level);
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut hex = [0u8; 64];
        for r in &self.list {
            let _ = writeln!(
                out,
                "{} {} {} {}",
                r.name,
                r.version,
                aricode_crypto::hex(&r.sha256, &mut hex),
                r.requires
            );
        }
        out
    }
}

fn parse_line(line: &str) -> Option<Refused> {
    let mut fields = line.split_whitespace();
    let name = fields.next().filter(|n| valid_app_name(n))?;
    let version = fields.next()?.parse().ok()?;
    let mut sha256 = [0u8; 32];
    unhex(fields.next()?, &mut sha256)?;
    let requires = fields.next()?.parse().ok().filter(|&r| r > 0)?;
    if fields.next().is_some() {
        return None;
    }
    Some(Refused {
        name: String::from(name),
        version,
        sha256,
        requires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &'static str, version: u32, sha: u8) -> AppEntry<'static> {
        AppEntry {
            name,
            version,
            file: "x.aripkg",
            size: 1,
            sha256: [sha; 32],
        }
    }

    fn refused(name: &str, version: u32, sha: u8, requires: u32) -> Refused {
        Refused {
            name: String::from(name),
            version,
            sha256: [sha; 32],
            requires,
        }
    }

    #[test]
    fn holds_back_only_the_same_package_above_the_level() {
        let mut list = RefusedList::default();
        list.record(refused("editor", 4, 0xAA, 2));
        assert_eq!(list.holds_back(&entry("editor", 4, 0xAA), 1), Some(2));
        // The level now offered, another version or other bytes: fetched.
        assert_eq!(list.holds_back(&entry("editor", 4, 0xAA), 2), None);
        assert_eq!(list.holds_back(&entry("editor", 5, 0xAA), 1), None);
        assert_eq!(list.holds_back(&entry("editor", 4, 0xBB), 1), None);
        assert_eq!(list.holds_back(&entry("files", 4, 0xAA), 1), None);
        // One line per app; pruning drops what the level satisfies.
        list.record(refused("editor", 5, 0xCC, 3));
        list.record(refused("files", 1, 0x11, 2));
        assert_eq!(list.holds_back(&entry("editor", 4, 0xAA), 1), None);
        assert_eq!(list.holds_back(&entry("editor", 5, 0xCC), 1), Some(3));
        list.prune(2);
        assert_eq!(RefusedList::parse(&list.to_text()), (list.clone(), 0));
        assert_eq!(list.to_text().lines().count(), 1);
        list.prune(3);
        assert!(list.is_empty());
    }

    #[test]
    fn damaged_lines_are_counted_and_dropped() {
        let sha = "ab".repeat(32);
        let text = alloc::format!(
            "editor 4 {sha} 2\n\neditor 5 {sha} 2\nbad/name 1 {sha} 2\nfiles 1 {sha} 0\nfiles 1 xyz 2\nfiles 1 {sha} 2 extra\nfiles 1 {sha} 2\n"
        );
        let (list, rejected) = RefusedList::parse(&text);
        assert_eq!(rejected, 5);
        assert_eq!(list.to_text().lines().count(), 2);
    }
}
