// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Third-party notices of what the images ship: every crates.io crate
//! compiled into AriBoot, the kernel or a user program, with its licence,
//! authors, repository and the licence files of its package (MIT, BSD and
//! Apache-2.0 ask for them with binary distributions). Build scripts and
//! procedural macros run on the build host and are not shipped, so they
//! are not listed. `cargo xtask notices` writes the file, `usb-image`
//! puts it on the ESP, and `check` fails when a shipped crate has no
//! licence file.

use crate::Result;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where `cargo xtask notices` writes the notices.
pub const FILE: &str = "THIRD-PARTY-NOTICES.txt";

/// Crates whose package carries no licence file, and the directory of
/// `tools/xtask/licences/` holding the copies from their repository's
/// tag (its README names it). A new version of one is refused until its
/// copies are refreshed.
const FROM_REPOSITORY: &[(&str, &str, &str)] = &[
    ("wasmi", "2.0.0", "wasmi"),
    ("wasmi_collections", "2.0.0", "wasmi"),
    ("wasmi_core", "2.0.0", "wasmi"),
    ("wasmi_ir", "2.0.0", "wasmi"),
    ("wasmparser", "0.228.0", "wasm-tools"),
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
    license_file: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    repository: Option<String>,
    source: Option<String>,
    manifest_path: PathBuf,
}

/// One shipped third-party crate and its licence files.
struct Shipped<'a> {
    package: &'a Package,
    licences: Vec<(String, String)>,
}

/// The notices text, or the crates whose licence files were not found.
pub fn generate() -> Result<String> {
    let out = crate::build::cargo()
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata --locked failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Metadata =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let mut shipped: BTreeMap<(String, String), Vec<&'static str>> = BTreeMap::new();
    for (what, crates) in [
        (
            "AriBoot",
            tree(
                crate::build::cargo(),
                &["ariboot"],
                crate::build::UEFI_TARGET,
            )?,
        ),
        (
            "kernel",
            tree(
                crate::build::cargo(),
                &["arikernel"],
                crate::build::KERNEL_TARGET,
            )?,
        ),
        (
            "user programs",
            tree(
                crate::build::user_cargo(),
                crate::build::USER_PACKAGES,
                crate::build::USER_TARGET,
            )?,
        ),
    ] {
        for c in crates {
            let places = shipped.entry(c).or_default();
            if !places.contains(&what) {
                places.push(what);
            }
        }
    }
    let mut list = Vec::new();
    let mut missing = Vec::new();
    for (name, version) in shipped.keys() {
        let Some(p) = meta
            .packages
            .iter()
            .find(|p| &p.name == name && &p.version == version && p.source.is_some())
        else {
            continue; // a workspace crate
        };
        let licences = licence_files(p)?;
        if licences.is_empty() {
            missing.push(format!("{name} {version}"));
        }
        list.push(Shipped {
            package: p,
            licences,
        });
    }
    if !missing.is_empty() {
        return Err(format!(
            "shipped crates without a licence file in their package (add their text by hand to {FILE}'s generator): {}",
            missing.join(", ")
        ));
    }
    Ok(render(&list, &shipped))
}

/// `cargo tree` of `packages` on `target`: normal dependencies only, no
/// procedural macros, as `(name, version)`.
fn tree(mut c: Command, packages: &[&str], target: &str) -> Result<Vec<(String, String)>> {
    c.args([
        "tree", "--locked", "--target", target, "-e", "normal", "--prefix", "none", "--format",
        "{p}",
    ]);
    for p in packages {
        c.args(["-p", p]);
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

/// `name vX.Y.Z [(path)] [(proc-macro)] [(*)]` lines into unique
/// `(name, version)`, procedural macros left out.
fn parse_tree(text: &str) -> Vec<(String, String)> {
    let mut crates: Vec<(String, String)> = text
        .lines()
        .filter(|l| !l.contains("(proc-macro)"))
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

/// The package's licence files: `license-file`, and every top-level file
/// whose name starts with LICENSE, LICENCE, COPYING or NOTICE; for the
/// crates of [`FROM_REPOSITORY`], the copies kept in the repository.
fn licence_files(p: &Package) -> Result<Vec<(String, String)>> {
    let kept = FROM_REPOSITORY
        .iter()
        .find(|(n, v, _)| *n == p.name && *v == p.version)
        .map(|(_, _, d)| crate::root().join("tools/xtask/licences").join(d));
    let dir = match &kept {
        Some(d) => d.as_path(),
        None => p.manifest_path.parent().unwrap_or(Path::new(".")),
    };
    let mut names: Vec<String> = Vec::new();
    if let Some(f) = &p.license_file {
        names.push(f.clone());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let upper = name.to_ascii_uppercase();
        if e.path().is_file()
            && name != "README.md"
            && ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
                .iter()
                .any(|k| upper.starts_with(k))
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let text = std::fs::read_to_string(dir.join(&n))
            .map_err(|e| format!("{}: {e}", dir.join(&n).display()))?;
        out.push((n, text));
    }
    Ok(out)
}

fn render(list: &[Shipped], shipped: &BTreeMap<(String, String), Vec<&str>>) -> String {
    let mut s = String::new();
    s.push_str("AriCodeOS third-party notices\n=============================\n\n");
    s.push_str(
        "The AriCodeOS images (AriBoot, the kernel and the programs of its initrd)\n\
         include the following crates from crates.io, each under the licence\n\
         named, whose texts and notices follow. Generated by `cargo xtask\n\
         notices` from Cargo.lock. See NOTICE for AriCodeOS itself and the\n\
         third-party data it carries.\n\n",
    );
    for c in list {
        let p = c.package;
        let used = shipped
            .get(&(p.name.clone(), p.version.clone()))
            .map(|v| v.join(", "))
            .unwrap_or_default();
        s.push_str(&format!(
            "  {} {}  {}  ({used})\n",
            p.name,
            p.version,
            p.license.as_deref().unwrap_or("see licence file")
        ));
    }
    for c in list {
        let p = c.package;
        s.push_str(&format!("\n{}\n{} {}\n", "=".repeat(72), p.name, p.version));
        s.push_str(&format!(
            "Licence: {}\n",
            p.license.as_deref().unwrap_or("see below")
        ));
        if !p.authors.is_empty() {
            s.push_str(&format!("Authors: {}\n", p.authors.join(", ")));
        }
        if let Some(r) = &p.repository {
            s.push_str(&format!("Repository: {r}\n"));
        }
        for (name, text) in &c.licences {
            s.push_str(&format!("\n--- {name} ---\n\n{}\n", text.trim_end()));
        }
    }
    s
}

/// `cargo xtask notices`: writes `build/THIRD-PARTY-NOTICES.txt`.
pub fn write() -> Result<PathBuf> {
    let text = generate()?;
    let path = crate::build_dir().join(FILE);
    std::fs::create_dir_all(crate::build_dir()).map_err(|e| e.to_string())?;
    std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procedural_macros_and_duplicates_are_left_out() {
        let text = "\
ariboot v0.16.0 (/x/boot)
uefi v0.33.0
uefi-macros v0.16.0 (proc-macro)
uefi v0.33.0 (*)
wasmi v2.0.0
";
        assert_eq!(
            parse_tree(text),
            [
                ("ariboot".to_string(), "0.16.0".to_string()),
                ("uefi".to_string(), "0.33.0".to_string()),
                ("wasmi".to_string(), "2.0.0".to_string()),
            ]
        );
    }
}
