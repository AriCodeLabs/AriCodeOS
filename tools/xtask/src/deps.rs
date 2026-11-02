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
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    license: Option<String>,
    /// `None` for the workspace's own packages.
    source: Option<String>,
}

/// Checks every third-party crate of the workspace, and what the user
/// programs `user_roots` link. `--locked`: a `Cargo.lock` that would
/// change is refused, so nothing is picked up that the repository does not
/// pin.
pub fn check(user_roots: &[&str]) -> Result {
    let all = metadata(None)?;
    let user = user_graph(None, user_roots)?;
    judge(&all, &user)
}

/// The same checks for an app built out of tree (`dir/Cargo.toml`, its own
/// workspace): every package of that workspace, the SDK's by path
/// included, is linked into the user program.
pub fn check_external(dir: &std::path::Path) -> Result {
    let manifest = dir.join("Cargo.toml");
    let all = metadata(Some(&manifest))?;
    let user = user_graph(Some(&manifest), &[])?;
    judge(&all, &user)
}

/// `cargo metadata --locked` of the repository, or of `manifest`.
fn metadata(manifest: Option<&std::path::Path>) -> Result<Metadata> {
    let mut c = std::process::Command::new(env!("CARGO"));
    c.current_dir(crate::root())
        .args(["metadata", "--format-version", "1", "--locked"]);
    if let Some(m) = manifest {
        c.arg("--manifest-path").arg(m);
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

/// What `roots` (user programs; none: the workspace of `manifest`) link on
/// the user target, as `cargo tree` resolves it: the crates actually
/// built, as `(name, version)`. `cargo metadata`'s graph is not used for
/// this: it lists optional dependencies whose feature is off (it shows
/// `rustls-webpki` linking `ring`, which a build never does).
fn user_graph(manifest: Option<&std::path::Path>, roots: &[&str]) -> Result<Vec<(String, String)>> {
    // With the user programs' flags, so `cfg`s such as `crt-static` are
    // what their build sees (audit of ADR 0067, L9).
    let mut c = crate::build::user_cargo();
    c.args([
        "tree",
        "--locked",
        "--target",
        crate::build::USER_TARGET,
        "-e",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ]);
    if let Some(m) = manifest {
        c.arg("--manifest-path").arg(m);
    }
    for r in roots {
        c.args(["-p", r]);
    }
    let out = c.output().map_err(|e| format!("cargo tree: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo tree --locked failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(parse_tree(&String::from_utf8_lossy(&out.stdout)))
}

/// `name vX.Y.Z [(path)] [(*)]` lines into unique `(name, version)`.
fn parse_tree(text: &str) -> Vec<(String, String)> {
    let mut crates: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (name, version) = (w.next()?, w.next()?.strip_prefix('v')?);
            Some((name.to_string(), version.to_string()))
        })
        .collect();
    crates.sort();
    crates.dedup();
    crates
}

/// Licences and sources over every crate (`all`); crates that bind to the
/// host OS over what the user programs link on their target (`user`).
fn judge(all: &Metadata, user: &[(String, String)]) -> Result {
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
    refused.extend(host_os_crates(user));
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

/// Refusals for crates of [`HOST_OS_CRATES`] in the user programs' build
/// graph (normal dependencies on their target: a crate behind `cfg(unix)`
/// is in, build scripts' dependencies run on the host and are not).
fn host_os_crates(user: &[(String, String)]) -> Vec<String> {
    user.iter()
        .filter(|(name, _)| HOST_OS_CRATES.contains(&name.as_str()))
        .map(|(name, version)| {
            format!("{name} {version}: binds to Linux or libc, linked by a user program (ADR 0065)")
        })
        .collect()
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

    /// `cargo tree` output: a crate the user programs build is refused
    /// once, whatever path reaches it; one absent from it is not.
    #[test]
    fn user_programs_do_not_link_the_host_os() {
        let tree = "\
app v0.1.0 (/src/app)
lib v0.1.0 (/src/lib)
libc v0.2.191
rustls v0.23.46
libc v0.2.191 (*)
";
        let graph = parse_tree(tree);
        assert_eq!(graph.len(), 4);
        let refused = host_os_crates(&graph);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].starts_with("libc 0.2.191: "));
        assert!(host_os_crates(&parse_tree("app v0.1.0 (/src/app)\nrustls v0.23.46\n")).is_empty());
    }

    #[test]
    fn only_crates_io_with_a_licence() {
        assert_eq!(refusal(CRATES_IO, Some("MIT")), None);
        assert!(refusal(CRATES_IO, None).is_some());
        assert!(refusal("git+https://example.org/x", Some("MIT")).is_some());
        assert!(refusal(CRATES_IO, Some("GPL-2.0-only")).is_some());
    }
}
