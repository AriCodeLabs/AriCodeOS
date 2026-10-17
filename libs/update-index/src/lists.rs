// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The small text files the updater and the installer exchange in the
//! updater's directory (ADR 0029, decisions 4, 5 and 8):
//!
//! ```text
//! installed   hello_ext 2                 written by the installer (LIST)
//! plan        system 3 / app hello_ext 2  written by `update`, read by `upgrade`
//! staged      pkg-1.aripkg hello_ext 2 <sha256>   written by `upgrade`, read by the installer
//! ```
//!
//! Parsers are strict: a damaged line is counted and never acted upon.

use crate::index::valid_app_name;
use crate::{staged_package_file, MAX_STAGED};
use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::unhex;
use core::fmt::Write;

/// `name version` per line.
pub fn encode_installed(apps: &[(String, u32)]) -> String {
    let mut out = String::new();
    for (name, version) in apps {
        let _ = writeln!(out, "{name} {version}");
    }
    out
}

/// The installed list; rejected lines are counted.
pub fn parse_installed(text: &str) -> (Vec<(&str, u32)>, usize) {
    let mut apps: Vec<(&str, u32)> = Vec::new();
    let mut rejected = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut f = line.split_whitespace();
        match (f.next(), f.next().and_then(|v| v.parse().ok()), f.next()) {
            (Some(name), Some(version), None)
                if valid_app_name(name) && !apps.iter().any(|(n, _)| *n == name) =>
            {
                apps.push((name, version))
            }
            _ => rejected += 1,
        }
    }
    (apps, rejected)
}

/// One item `update` reported as installable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanItem<'a> {
    System(u32),
    App(&'a str, u32),
}

pub fn encode_plan(items: &[PlanItem]) -> String {
    let mut out = String::new();
    for item in items {
        let _ = match item {
            PlanItem::System(v) => writeln!(out, "system {v}"),
            PlanItem::App(name, v) => writeln!(out, "app {name} {v}"),
        };
    }
    out
}

/// The plan; rejected lines are counted.
pub fn parse_plan(text: &str) -> (Vec<PlanItem<'_>>, usize) {
    let mut items = Vec::new();
    let mut rejected = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let item = match f.as_slice() {
            ["system", v] => v.parse().ok().map(PlanItem::System),
            ["app", name, v] if valid_app_name(name) => {
                v.parse().ok().map(|v| PlanItem::App(name, v))
            }
            _ => None,
        };
        match item {
            Some(i) => items.push(i),
            None => rejected += 1,
        }
    }
    (items, rejected)
}

/// One package staged by the updater.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedEntry<'a> {
    /// One of [`staged_package_file`]: never a path.
    pub file: &'a str,
    pub name: &'a str,
    pub version: u32,
    pub sha256: [u8; 32],
}

pub fn encode_staged(entries: &[StagedEntry]) -> String {
    let mut out = String::new();
    let mut hex = [0u8; 64];
    for e in entries {
        let _ = writeln!(
            out,
            "{} {} {} {}",
            e.file,
            e.name,
            e.version,
            aricode_crypto::hex(&e.sha256, &mut hex)
        );
    }
    out
}

fn is_stage_file(name: &str) -> bool {
    (0..MAX_STAGED).any(|i| staged_package_file(i) == Some(name))
}

/// The staged list. A line naming any file other than the fixed stage
/// files, or repeating one, is rejected and counted: the installer acts on
/// nothing else (ADR 0029, decision 5).
pub fn parse_staged(text: &str) -> (Vec<StagedEntry<'_>>, usize) {
    let mut entries: Vec<StagedEntry> = Vec::new();
    let mut rejected = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let entry = match f.as_slice() {
            [file, name, version, sha] if is_stage_file(file) && valid_app_name(name) => {
                let mut sha256 = [0u8; 32];
                match (version.parse(), unhex(sha, &mut sha256)) {
                    (Ok(version), Some(())) => Some(StagedEntry {
                        file,
                        name,
                        version,
                        sha256,
                    }),
                    _ => None,
                }
            }
            _ => None,
        };
        match entry {
            Some(e) if !entries.iter().any(|x| x.file == e.file || x.name == e.name) => {
                entries.push(e)
            }
            _ => rejected += 1,
        }
    }
    (entries, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn installed_round_trip_and_rejections() {
        let apps = vec![(String::from("hello_ext"), 2), (String::from("b"), 7)];
        let text = encode_installed(&apps);
        assert_eq!(
            parse_installed(&text),
            (vec![("hello_ext", 2), ("b", 7)], 0)
        );
        let (list, rejected) = parse_installed("a 1\na 2\nbad/name 1\nc x\nd 1 extra\n");
        assert_eq!((list, rejected), (vec![("a", 1)], 4));
    }

    #[test]
    fn plan_round_trip_and_rejections() {
        let items = [PlanItem::System(3), PlanItem::App("hello_ext", 2)];
        let text = encode_plan(&items);
        assert_eq!(parse_plan(&text), (items.to_vec(), 0));
        assert_eq!(parse_plan("system x\napp ../x 1\nother 1\n").1, 3);
    }

    #[test]
    fn staged_accepts_only_the_stage_files() {
        let e = StagedEntry {
            file: "pkg-1.aripkg",
            name: "hello_ext",
            version: 2,
            sha256: [7; 32],
        };
        let text = encode_staged(&[e]);
        assert_eq!(parse_staged(&text), (vec![e], 0));
        let sha = "00".repeat(32);
        for bad in [
            alloc::format!("../apps/x.aripkg x 1 {sha}"),
            alloc::format!("pkgs/x.aripkg x 1 {sha}"),
            alloc::format!("pkg-9.aripkg x 1 {sha}"),
            alloc::format!("pkg-1.aripkg x one {sha}"),
            String::from("pkg-1.aripkg x 1 abcd"),
        ] {
            assert_eq!(parse_staged(&bad).1, 1, "{bad}");
        }
        // The same file or app twice: only the first counts.
        let twice = alloc::format!("{text}{text}pkg-2.aripkg hello_ext 3 {sha}\n");
        assert_eq!(parse_staged(&twice), (vec![e], 2));
    }
}
