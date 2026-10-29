// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Third-party crates (ADR 0063): every crate outside the workspace comes
//! from crates.io at the version `Cargo.lock` pins, under a licence the
//! project accepts. `cargo xtask check` refuses anything else.

use crate::Result;
use serde::Deserialize;

/// Licences a third-party crate may use (SPDX identifiers): permissive
/// ones, and MPL-2.0, whose copyleft stays inside the files it covers.
/// GPL, LGPL and AGPL are not accepted (owner, 2026-10-29).
const ACCEPTED: &[&str] = &[
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "ISC",
    "MIT",
    "MPL-2.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Unlicense",
    "Zlib",
];

/// The only registry crates may come from.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    license: Option<String>,
    /// `None` for the workspace's own packages.
    source: Option<String>,
}

/// Checks every third-party crate of the workspace. `--locked`: a
/// `Cargo.lock` that would change is refused, so nothing is picked up
/// that the repository does not pin.
pub fn check() -> Result {
    let out = std::process::Command::new(env!("CARGO"))
        .current_dir(crate::root())
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata --locked failed (is Cargo.lock up to date?):\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let metadata: Metadata =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let refused: Vec<String> = metadata
        .packages
        .iter()
        .filter_map(|p| {
            let source = p.source.as_deref()?;
            refusal(source, p.license.as_deref())
                .map(|why| format!("{} {}: {why}", p.name, p.version))
        })
        .collect();
    let third_party = metadata
        .packages
        .iter()
        .filter(|p| p.source.is_some())
        .count();
    if refused.is_empty() {
        println!("[xtask] {third_party} third-party crate(s): crates.io, accepted licences");
        Ok(())
    } else {
        Err(format!(
            "third-party crates refused (ADR 0063):\n  {}",
            refused.join("\n  ")
        ))
    }
}

/// Why a third-party crate is refused, `None` when it is accepted.
fn refusal(source: &str, license: Option<&str>) -> Option<String> {
    if source != CRATES_IO {
        return Some(format!("comes from {source}, not crates.io"));
    }
    match license {
        None => Some(String::from("no licence declared")),
        Some(l) if accepted(l) => None,
        Some(l) => Some(format!("licence '{l}' not accepted")),
    }
}

/// Whether an SPDX licence expression lets the project use the crate:
/// `A OR B` needs one side accepted, `A AND B` both, `A WITH exception`
/// needs `A`. The old `A/B` spelling means `A OR B`. Anything that does
/// not parse is refused.
fn accepted(expr: &str) -> bool {
    let spaced = expr
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace('/', " OR ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut pos = 0;
    let value = or_expr(&tokens, &mut pos);
    value == Some(true) && pos == tokens.len()
}

fn or_expr(t: &[&str], pos: &mut usize) -> Option<bool> {
    let mut value = and_expr(t, pos)?;
    while t.get(*pos) == Some(&"OR") {
        *pos += 1;
        value |= and_expr(t, pos)?;
    }
    Some(value)
}

fn and_expr(t: &[&str], pos: &mut usize) -> Option<bool> {
    let mut value = term(t, pos)?;
    while t.get(*pos) == Some(&"AND") {
        *pos += 1;
        value &= term(t, pos)?;
    }
    Some(value)
}

fn term(t: &[&str], pos: &mut usize) -> Option<bool> {
    let token = *t.get(*pos)?;
    *pos += 1;
    let value = if token == "(" {
        let inner = or_expr(t, pos)?;
        (t.get(*pos) == Some(&")")).then_some(())?;
        *pos += 1;
        inner
    } else if ["OR", "AND", "WITH", ")"].contains(&token) {
        return None;
    } else {
        ACCEPTED.contains(&token)
    };
    if t.get(*pos) == Some(&"WITH") {
        // An exception only adds permissions to the licence it follows.
        *pos += 1;
        t.get(*pos)
            .filter(|e| !["(", ")", "OR", "AND", "WITH"].contains(e))?;
        *pos += 1;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn licence_expressions() {
        for ok in [
            "MIT",
            "MIT OR Apache-2.0",
            "Apache-2.0/MIT",
            "Unlicense OR MIT",
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            "Apache-2.0 WITH LLVM-exception OR MIT",
            "MPL-2.0",
            "GPL-3.0 OR MIT",
        ] {
            assert!(accepted(ok), "{ok}");
        }
        for bad in [
            "GPL-2.0",
            "LGPL-2.1-or-later",
            "MIT AND GPL-3.0",
            "AGPL-3.0 WITH Classpath-exception-2.0",
            "",
            "MIT OR",
            "(MIT",
            "MIT)",
            "MIT Apache-2.0",
            "WITH MIT",
            "Proprietary",
        ] {
            assert!(!accepted(bad), "{bad}");
        }
    }

    #[test]
    fn only_crates_io_with_a_licence() {
        assert_eq!(refusal(CRATES_IO, Some("MIT")), None);
        assert!(refusal(CRATES_IO, None).is_some());
        assert!(refusal("git+https://example.org/x", Some("MIT")).is_some());
        assert!(refusal(CRATES_IO, Some("GPL-2.0-only")).is_some());
    }
}
