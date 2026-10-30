// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Third-party crates (ADR 0063): every crate outside the workspace comes
//! from crates.io at the version `Cargo.lock` pins, under a licence the
//! project accepts. `cargo xtask check` refuses anything else.
//!
//! User programs are built for the Linux target without its C library
//! (ADR 0065), so no crate that calls Linux or libc may reach them: such a
//! crate would compile for the target and then fail to link, or worse,
//! issue Linux system calls on AriCodeOS.

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

/// Crates that bind a program to Linux or its C library, refused in the
/// dependencies of user programs (ADR 0065).
/// Raw Linux system call crates included (audit of ADR 0065, finding 4).
const HOST_OS_CRATES: &[&str] = &[
    "errno",
    "getrandom",
    "libc",
    "linux-raw-sys",
    "linux-syscall",
    "nix",
    "origin",
    "rustix",
    "sc",
    "syscalls",
];

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Resolve,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    license: Option<String>,
    /// `None` for the workspace's own packages.
    source: Option<String>,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<NodeDep>,
}

#[derive(Deserialize)]
struct NodeDep {
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
struct DepKind {
    /// `None` for a normal dependency, `"build"` or `"dev"` otherwise.
    kind: Option<String>,
}

/// Checks every third-party crate of the workspace, and what the user
/// programs `user_roots` link. `--locked`: a `Cargo.lock` that would
/// change is refused, so nothing is picked up that the repository does not
/// pin.
pub fn check(user_roots: &[&str]) -> Result {
    let all = metadata(None, None)?;
    let user = metadata(None, Some(crate::build::USER_TARGET))?;
    judge(&all, &user, user_roots)
}

/// The same checks for an app built out of tree (`dir/Cargo.toml`, its own
/// workspace): every package of that workspace, the SDK's by path
/// included, is linked into the user program.
pub fn check_external(dir: &std::path::Path) -> Result {
    let manifest = dir.join("Cargo.toml");
    let all = metadata(Some(&manifest), None)?;
    let user = metadata(Some(&manifest), Some(crate::build::USER_TARGET))?;
    let roots: Vec<String> = all
        .packages
        .iter()
        .filter(|p| p.source.is_none())
        .map(|p| p.name.clone())
        .collect();
    let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
    judge(&all, &user, &roots)
}

/// `cargo metadata --locked` of the repository, or of `manifest`; with
/// `platform`, the dependency graph keeps only what that target builds
/// (`cfg(unix)` dependencies in, `cfg(target_arch = "aarch64")` ones out).
fn metadata(manifest: Option<&std::path::Path>, platform: Option<&str>) -> Result<Metadata> {
    let mut c = std::process::Command::new(env!("CARGO"));
    c.current_dir(crate::root())
        .args(["metadata", "--format-version", "1", "--locked"]);
    if let Some(m) = manifest {
        c.arg("--manifest-path").arg(m);
    }
    if let Some(p) = platform {
        c.args(["--filter-platform", p]);
    }
    let out = c.output().map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata --locked failed (is Cargo.lock up to date?):\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))
}

/// Licences and sources over every crate (`all`); crates that bind to the
/// host OS over what the user programs link on their target (`user`).
fn judge(all: &Metadata, user: &Metadata, user_roots: &[&str]) -> Result {
    let metadata = all;
    let mut refused: Vec<String> = metadata
        .packages
        .iter()
        .filter_map(|p| {
            let source = p.source.as_deref()?;
            refusal(source, p.license.as_deref())
                .map(|why| format!("{} {}: {why}", p.name, p.version))
        })
        .collect();
    refused.extend(host_os_crates(user, user_roots));
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

/// Refusals for crates of [`HOST_OS_CRATES`] that a user program links:
/// its normal dependencies in a graph already filtered for the user target
/// (a crate behind `cfg(unix)` is built for it). Build dependencies run on
/// the host and are not followed.
fn host_os_crates(metadata: &Metadata, user_roots: &[&str]) -> Vec<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let package = |id: &str| metadata.packages.iter().find(|p| p.id == id);
    let edges: BTreeMap<&str, Vec<&str>> = metadata
        .resolve
        .nodes
        .iter()
        .map(|n| {
            let normal = n
                .deps
                .iter()
                .filter(|d| d.dep_kinds.iter().any(|k| k.kind.is_none()))
                .map(|d| d.pkg.as_str())
                .collect();
            (n.id.as_str(), normal)
        })
        .collect();
    let mut refused = BTreeSet::new();
    for root in user_roots {
        let Some(start) = metadata
            .packages
            .iter()
            .find(|p| p.name == *root && p.source.is_none())
        else {
            continue;
        };
        let mut seen = BTreeSet::new();
        let mut stack = vec![start.id.as_str()];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(p) = package(id) {
                if HOST_OS_CRATES.contains(&p.name.as_str()) {
                    refused.insert(format!(
                        "{} {}: binds to Linux or libc, linked by user program {root} (ADR 0065)",
                        p.name, p.version
                    ));
                }
            }
            stack.extend(edges.get(id).into_iter().flatten());
        }
    }
    refused.into_iter().collect()
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

    /// A user program reaching `libc` through a `cfg(unix)` dependency
    /// is refused; one that only needs it to build (a build script) or
    /// for its tests is not, and neither is a host tool.
    #[test]
    fn user_programs_do_not_link_the_host_os() {
        let json = r#"{
            "packages": [
                {"id": "app", "name": "app", "version": "1", "license": null, "source": null},
                {"id": "lib", "name": "lib", "version": "1", "license": null, "source": null},
                {"id": "tool", "name": "tool", "version": "1", "license": null, "source": null},
                {"id": "libc", "name": "libc", "version": "0.2", "license": "MIT", "source": "x"},
                {"id": "cc", "name": "cc", "version": "1", "license": "MIT", "source": "x"}
            ],
            "resolve": {"nodes": [
                {"id": "app", "deps": [
                    {"pkg": "lib", "dep_kinds": [{"kind": null, "target": null}]},
                    {"pkg": "cc", "dep_kinds": [{"kind": "build", "target": null}]}
                ]},
                {"id": "lib", "deps": [
                    {"pkg": "libc", "dep_kinds": [{"kind": null, "target": "cfg(unix)"}]}
                ]},
                {"id": "cc", "deps": [
                    {"pkg": "libc", "dep_kinds": [{"kind": null, "target": null}]}
                ]},
                {"id": "tool", "deps": [
                    {"pkg": "libc", "dep_kinds": [{"kind": null, "target": null}]}
                ]},
                {"id": "libc", "deps": []}
            ]}
        }"#;
        let metadata: Metadata = serde_json::from_str(json).expect("metadata");
        let refused = host_os_crates(&metadata, &["app"]);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("libc 0.2") && refused[0].contains("app"));
        assert!(host_os_crates(&metadata, &["tool_not_a_user_program"]).is_empty());
        let dev_only = json.replace(
            r#""pkg": "libc", "dep_kinds": [{"kind": null, "target": "cfg(unix)"}]"#,
            r#""pkg": "libc", "dep_kinds": [{"kind": "dev", "target": null}]"#,
        );
        let metadata: Metadata = serde_json::from_str(&dev_only).expect("metadata");
        assert!(host_os_crates(&metadata, &["app"]).is_empty());
    }

    #[test]
    fn only_crates_io_with_a_licence() {
        assert_eq!(refusal(CRATES_IO, Some("MIT")), None);
        assert!(refusal(CRATES_IO, None).is_some());
        assert!(refusal("git+https://example.org/x", Some("MIT")).is_some());
        assert!(refusal(CRATES_IO, Some("GPL-2.0-only")).is_some());
    }
}
