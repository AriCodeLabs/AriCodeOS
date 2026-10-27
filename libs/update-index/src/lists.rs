// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! What the updater and the installer exchange, as sections of one file
//! (`updater/state`, ADR 0029, decisions 4, 5 and 8):
//!
//! ```text
//! [installed]                              written by the installer (LIST)
//! hello_ext 2
//! [plan]                                   written by `update`, read by `upgrade`
//! system 3
//! app hello_ext 2
//!
//! [plan]                                   `update --channel dev` (ADR 0045)
//! channel dev
//! system dev 7
//! [staged]                                 written by `upgrade`, read by the installer
//! pkg-1.aripkg hello_ext 2 <sha256>
//! [floor]                                  the newest stable release this machine ran
//! stable 12                                (ADR 0045; written by the updater and usb-image)
//! ```
//!
//! Parsers are strict: a damaged line is counted and never acted upon.

use crate::index::valid_app_name;
use crate::{staged_package_file, MAX_STAGED};
use alloc::string::String;
use alloc::vec::Vec;
use aricode_ab::Channel;
use aricode_crypto::unhex;
use core::fmt::Write;

pub const INSTALLED: &str = "installed";
pub const PLAN: &str = "plan";
pub const STAGED: &str = "staged";
pub const FLOOR: &str = "floor";

const SECTIONS: [&str; 4] = [INSTALLED, PLAN, STAGED, FLOOR];

/// The state file parsed line by line. A line `[name]` with a known name
/// starts that section; every other non-blank line belongs to the section
/// above it. Line endings (`\n` or `\r\n`), a missing final newline and
/// blank lines are formatting, not damage.
///
/// Structural damage is recovered without guessing (fix
/// update-state-sections, incident 0006). The order of copies in a damaged
/// file says nothing reliable about which one was written last, so:
///
/// - a repeated `[plan]` or `[staged]` makes that section empty: both are
///   recomputed (`update` writes the plan again, `upgrade` stages again),
///   and acting on a possibly stale copy could install something the owner
///   was never shown;
/// - a repeated `[installed]` keeps its last copy, which is only advisory
///   (the installer rewrites it before every updater run), and the file is
///   flagged as damaged like any other repair;
/// - lines under an unknown header or before the first header are dropped.
///
/// Any of these sets `damaged`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct State {
    bodies: [Option<String>; 4],
    damaged: bool,
}

fn header(line: &str) -> Option<&str> {
    line.strip_prefix('[')?.strip_suffix(']')
}

impl State {
    fn parse(text: &str) -> Self {
        let mut state = Self::default();
        let mut repeated = [false; 4];
        let mut current: Option<usize> = None;
        for line in text.lines().map(|l| l.trim_end_matches('\r')) {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(name) = header(line) {
                current = SECTIONS.iter().position(|n| *n == name);
                match current {
                    Some(i) => {
                        if state.bodies[i].is_some() {
                            repeated[i] = true;
                            state.damaged = true;
                        }
                        state.bodies[i] = Some(String::new());
                    }
                    None => state.damaged = true,
                }
                continue;
            }
            match current.and_then(|i| state.bodies[i].as_mut()) {
                Some(body) => {
                    body.push_str(line);
                    body.push('\n');
                }
                None => state.damaged = true,
            }
        }
        for name in [PLAN, STAGED] {
            let i = SECTIONS.iter().position(|n| *n == name).unwrap_or(0);
            if repeated[i] {
                state.bodies[i] = Some(String::new());
            }
        }
        state
    }

    fn encode(&self) -> String {
        let mut out = String::new();
        for (name, body) in SECTIONS.iter().zip(&self.bodies) {
            if let Some(body) = body {
                let _ = writeln!(out, "[{name}]");
                out.push_str(body);
            }
        }
        out
    }
}

/// Normalizes a section body: whole lines, no header lines (a body can
/// never forge another section).
fn clean_body(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines().filter(|l| header(l).is_none()) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The body of section `name`; `None` when the section is absent.
pub fn section(text: &str, name: &str) -> Option<String> {
    let i = SECTIONS.iter().position(|n| *n == name)?;
    State::parse(text).bodies[i].take()
}

/// `text` rewritten canonically with section `name` replaced by `body`:
/// every section appears once, in the order installed, plan, staged, with
/// damage handled as [`State`] describes.
pub fn with_section(text: &str, name: &str, body: &str) -> String {
    let mut state = State::parse(text);
    if let Some(i) = SECTIONS.iter().position(|n| *n == name) {
        state.bodies[i] = Some(clean_body(body));
    }
    state.encode()
}

/// Whether `text` has structural damage (a repeated or unknown header,
/// lines outside a section). Formatting (line endings, blank lines, the
/// final newline) is not damage.
pub fn needs_repair(text: &str) -> bool {
    State::parse(text).damaged
}

/// The canonical form of `text` (what the next write would store), with
/// damage handled as [`State`] describes: callers store it as soon as they
/// find damage.
pub fn repaired(text: &str) -> String {
    State::parse(text).encode()
}

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

/// One item `update` reported as installable: a system image of a channel
/// (its version, or build number for `Dev`), or an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanItem<'a> {
    System(Channel, u32),
    App(&'a str, u32),
}

/// What the last `update` reported, for the channel it ran for (ADR 0045):
/// `upgrade --channel C` follows only a plan made for `C`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan<'a> {
    pub channel: Channel,
    pub items: Vec<PlanItem<'a>>,
}

/// A stable plan is written as before ADR 0045 (no `channel` line).
pub fn encode_plan(channel: Channel, items: &[PlanItem]) -> String {
    let mut out = String::new();
    if channel == Channel::Dev {
        let _ = writeln!(out, "channel dev");
    }
    for item in items {
        let _ = match item {
            PlanItem::System(Channel::Stable, v) => writeln!(out, "system {v}"),
            PlanItem::System(Channel::Dev, v) => writeln!(out, "system dev {v}"),
            PlanItem::App(name, v) => writeln!(out, "app {name} {v}"),
        };
    }
    out
}

/// The plan; rejected lines are counted. A plan without a `channel` line is
/// a stable one; a second `channel` line is rejected (the first stands).
pub fn parse_plan(text: &str) -> (Plan<'_>, usize) {
    let mut plan = Plan::default();
    let mut channel_seen = false;
    let mut rejected = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let item = match f.as_slice() {
            ["channel", "dev"] if !channel_seen => {
                channel_seen = true;
                plan.channel = Channel::Dev;
                continue;
            }
            ["system", v] => v.parse().ok().map(|v| PlanItem::System(Channel::Stable, v)),
            ["system", "dev", v] => v.parse().ok().map(|v| PlanItem::System(Channel::Dev, v)),
            ["app", name, v] if valid_app_name(name) => {
                v.parse().ok().map(|v| PlanItem::App(name, v))
            }
            _ => None,
        };
        match item {
            Some(i) => plan.items.push(i),
            None => rejected += 1,
        }
    }
    (plan, rejected)
}

/// The `[floor]` body: the newest stable release this machine ran (audit
/// of ADR 0045, M1).
pub fn encode_floor(version: u32) -> String {
    alloc::format!("stable {version}\n")
}

/// A `[floor]` body that is not exactly one `stable N` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamagedFloor;

/// The stable floor in a `[floor]` body; [`DamagedFloor`] when damaged,
/// which never means "no floor" (`policy::floor_for_run`, ADR 0060).
pub fn parse_floor(text: &str) -> Result<Option<u32>, DamagedFloor> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(line) = lines.next() else {
        return Ok(None);
    };
    match (
        line.split_whitespace().collect::<Vec<_>>().as_slice(),
        lines.next(),
    ) {
        (["stable", v], None) => v.parse().map(Some).map_err(|_| DamagedFloor),
        _ => Err(DamagedFloor),
    }
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
    fn sections_are_replaced_independently() {
        let t = with_section("", PLAN, "system 3\n");
        let t = with_section(&t, INSTALLED, "hello_ext 2\n");
        assert_eq!(section(&t, PLAN).as_deref(), Some("system 3\n"));
        assert_eq!(section(&t, INSTALLED).as_deref(), Some("hello_ext 2\n"));
        assert_eq!(section(&t, STAGED), None);
        let t = with_section(&t, PLAN, "");
        assert_eq!(section(&t, PLAN).as_deref(), Some(""));
        assert_eq!(section(&t, INSTALLED).as_deref(), Some("hello_ext 2\n"));
        // A body cannot forge another section.
        let t = with_section(&t, STAGED, "pkg-1.aripkg a 1 00\n[installed]\nevil 9\n");
        assert_eq!(section(&t, INSTALLED).as_deref(), Some("hello_ext 2\n"));
        assert_eq!(t.matches("[staged]").count(), 1);
        assert!(!needs_repair(&t));
    }

    /// Fix update-state-sections: on the laptop, two `update` runs and one
    /// `upgrade` left `[plan]` three times. Every rewrite, in any order and
    /// with empty bodies, must leave one header per section and no line
    /// any parser rejects.
    #[test]
    fn rewrites_stay_canonical() {
        let bodies = [
            (INSTALLED, ""),
            (INSTALLED, "hello_ext 2\n"),
            (PLAN, ""),
            (PLAN, "system 3\napp hello_ext 2\n"),
            (STAGED, ""),
            (STAGED, "pkg-1.aripkg hello_ext 2 0000000000000000000000000000000000000000000000000000000000000000\n"),
        ];
        // Every order of six writes drawn from the list, repeated: a cheap
        // deterministic sweep (6^4 sequences).
        for seq in 0..6usize.pow(4) {
            let mut text = String::new();
            let mut n = seq;
            let mut last = [None, None, None, None];
            for _ in 0..4 {
                let (name, body) = bodies[n % 6];
                n /= 6;
                text = with_section(&text, name, body);
                last[SECTIONS.iter().position(|s| *s == name).unwrap()] = Some(body);
                assert!(!needs_repair(&text), "{text:?}");
                for (i, name) in SECTIONS.iter().enumerate() {
                    assert_eq!(
                        text.matches(&alloc::format!("[{name}]")).count(),
                        usize::from(last[i].is_some())
                    );
                    assert_eq!(section(&text, name).as_deref(), last[i]);
                }
                let installed = section(&text, INSTALLED).unwrap_or_default();
                let plan = section(&text, PLAN).unwrap_or_default();
                let staged = section(&text, STAGED).unwrap_or_default();
                assert_eq!(parse_installed(&installed).1, 0, "{text:?}");
                assert_eq!(parse_plan(&plan).1, 0, "{text:?}");
                assert_eq!(parse_staged(&staged).1, 0, "{text:?}");
            }
        }
    }

    #[test]
    fn damaged_files_are_recovered_and_flagged() {
        // The file found on the laptop: [plan] repeated, so the plan is
        // distrusted (empty) and recomputed by the next `update`.
        let laptop = "[installed]\n[plan]\n[plan]\n[plan]\nsystem 3\n[staged]\n";
        assert!(needs_repair(laptop));
        assert_eq!(section(laptop, INSTALLED).as_deref(), Some(""));
        assert_eq!(section(laptop, PLAN).as_deref(), Some(""));
        assert_eq!(section(laptop, STAGED).as_deref(), Some(""));
        assert_eq!(repaired(laptop), "[installed]\n[plan]\n[staged]\n");
        assert!(!needs_repair(&repaired(laptop)));
        let rewritten = with_section(laptop, PLAN, "system 3\n");
        assert_eq!(rewritten, "[installed]\n[plan]\nsystem 3\n[staged]\n");
        assert!(!needs_repair(&rewritten));
        // Out of order: the later copy is not necessarily the newer one
        // (here the plan meant system 3). Neither copy is used.
        let shuffled = "[installed]\n[staged]\n[plan]\nsystem 3\n[staged]\n[plan]\nsystem 4\n";
        assert!(needs_repair(shuffled));
        assert_eq!(section(shuffled, PLAN).as_deref(), Some(""));
        assert_eq!(section(shuffled, STAGED).as_deref(), Some(""));
        // A repeated [staged] is emptied: nothing stale is ever installed.
        let staged = alloc::format!(
            "[staged]\npkg-1.aripkg a 1 {0}\n[staged]\npkg-2.aripkg b 2 {0}\n",
            "00".repeat(32)
        );
        assert!(needs_repair(&staged));
        assert_eq!(section(&staged, STAGED).as_deref(), Some(""));
        // A repeated [installed] keeps its last copy (advisory, rewritten by
        // the installer before every run) and is flagged.
        let installed = "[installed]\na 1\n[installed]\na 2\n";
        assert!(needs_repair(installed));
        assert_eq!(section(installed, INSTALLED).as_deref(), Some("a 2\n"));
        // Unknown sections and stray lines are dropped, never attributed.
        let odd = "stray\n[other]\nx 1\n[installed]\na 1\n";
        assert!(needs_repair(odd));
        assert_eq!(section(odd, INSTALLED).as_deref(), Some("a 1\n"));
        assert_eq!(with_section(odd, PLAN, ""), "[installed]\na 1\n[plan]\n");
    }

    #[test]
    fn formatting_is_not_damage() {
        for text in [
            "",
            "[installed]\na 1\n[plan]\n",
            "[installed]\r\na 1\r\n[plan]\r\nsystem 3\r\n",
            "[installed]\na 1\n[plan]\nsystem 3",
            "\n\n[installed]\n\na 1\n\n[plan]\n",
        ] {
            assert!(!needs_repair(text), "{text:?}");
        }
        let crlf = "[installed]\r\na 1\r\n[plan]\r\nsystem 3\r\n";
        assert_eq!(section(crlf, PLAN).as_deref(), Some("system 3\n"));
        assert_eq!(repaired(crlf), "[installed]\na 1\n[plan]\nsystem 3\n");
    }

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
    fn stable_floor_round_trip() {
        assert_eq!(parse_floor(&encode_floor(12)), Ok(Some(12)));
        assert_eq!(parse_floor(""), Ok(None));
        for bad in [
            "stable x\n",
            "stable 1\nstable 2\n",
            "dev 3\n",
            "stable 1 2\n",
        ] {
            assert_eq!(parse_floor(bad), Err(DamagedFloor), "{bad}");
        }
        // The floor is a section of its own, kept by every other rewrite.
        let t = with_section("", FLOOR, &encode_floor(12));
        let t = with_section(&t, PLAN, "system 3\n");
        let t = with_section(&t, STAGED, "");
        assert_eq!(section(&t, FLOOR).as_deref(), Some("stable 12\n"));
        assert!(!needs_repair(&t));
    }

    #[test]
    fn plan_round_trip_and_rejections() {
        let items = [
            PlanItem::System(Channel::Stable, 3),
            PlanItem::App("hello_ext", 2),
        ];
        let text = encode_plan(Channel::Stable, &items);
        assert_eq!(text, "system 3\napp hello_ext 2\n", "as before ADR 0045");
        let plan = Plan {
            channel: Channel::Stable,
            items: items.to_vec(),
        };
        assert_eq!(parse_plan(&text), (plan, 0));
        assert_eq!(parse_plan("system x\napp ../x 1\nother 1\n").1, 3);
        // A development plan (ADR 0045).
        let dev = [PlanItem::System(Channel::Dev, 7)];
        let text = encode_plan(Channel::Dev, &dev);
        assert_eq!(text, "channel dev\nsystem dev 7\n");
        let plan = Plan {
            channel: Channel::Dev,
            items: dev.to_vec(),
        };
        assert_eq!(parse_plan(&text), (plan, 0));
        let (odd, rejected) =
            parse_plan("channel dev\nchannel stable\nchannel dev\nsystem beta 3\nsystem dev x\n");
        assert_eq!(
            (odd.channel, odd.items.len(), rejected),
            (Channel::Dev, 0, 4)
        );
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
