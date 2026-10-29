// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Builds the system crates and runs static checks.

use crate::{root, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::string::String;

pub const UEFI_TARGET: &str = "x86_64-unknown-uefi";
pub const KERNEL_TARGET: &str = "x86_64-unknown-none";
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Workspace packages and the target they are built/linted with.
/// Single source of truth for `check` and for the host unit tests.
pub const PACKAGES: &[(&str, Option<&str>)] = &[
    ("boot-protocol", None),
    ("aricode-elf", None),
    ("aricode-x86-paging", None),
    ("aricode-cmdline", None),
    ("aricode-fb-console", None),
    ("aricode-x86-cpu", None),
    ("aricode-pmm", None),
    ("aricode-heap", None),
    ("aricode-sync", None),
    ("aricode-sched", None),
    ("aricode-abi", None),
    ("aricode-handles", None),
    ("aricode-vranges", None),
    ("aricode-initrd", None),
    ("aricode-acpi", None),
    ("aricode-pci", None),
    ("aricode-virtio", None),
    ("aricode-gpt", None),
    ("aricode-arifs", None),
    ("aricode-net", None),
    ("aricode-faults", None),
    ("aricode-http", None),
    ("aricode-input", None),
    ("aricode-gfx", None),
    ("aricode-manifest", None),
    ("aricode-pkg", None),
    ("aricode-update-index", None),
    ("aricode-wasm", None),
    ("aricode-crypto", None),
    ("aricode-ab", None),
    ("aricode-proto", None),
    ("aricode-ehci", None),
    ("aricode-sys", None),
    ("aricode-build", None),
    ("aricode-rt", Some(KERNEL_TARGET)),
    ("ari-app-hello", Some(KERNEL_TARGET)),
    ("ari-app-perms-check", Some(KERNEL_TARGET)),
    ("ari-app-updater", Some(KERNEL_TARGET)),
    ("ari-app-fs-probe", Some(KERNEL_TARGET)),
    ("ari-app-win-probe", Some(KERNEL_TARGET)),
    ("ari-app-wasm-runner", Some(KERNEL_TARGET)),
    ("ari-app-notes-reader", Some(KERNEL_TARGET)),
    ("ari-app-installer", Some(KERNEL_TARGET)),
    ("ari-app-bench", Some(KERNEL_TARGET)),
    ("wasm-demo", Some(WASM_TARGET)),
    ("user-testprogs", Some(KERNEL_TARGET)),
    ("ariinit", Some(KERNEL_TARGET)),
    ("ari-virtio-blk", Some(KERNEL_TARGET)),
    ("ari-virtio-net", Some(KERNEL_TARGET)),
    ("ari-virtio-input", Some(KERNEL_TARGET)),
    ("ari-usb-input", Some(KERNEL_TARGET)),
    ("ari-usb-ehci", Some(KERNEL_TARGET)),
    ("ari-console-bridge", Some(KERNEL_TARGET)),
    ("ari-compositor", Some(KERNEL_TARGET)),
    ("ari-fsd", Some(KERNEL_TARGET)),
    ("ari-shell", Some(KERNEL_TARGET)),
    ("ari-net-core", Some(KERNEL_TARGET)),
    ("ari-rtl-net", Some(KERNEL_TARGET)),
    ("ari-sshd", Some(KERNEL_TARGET)),
    ("aricode-https", Some(KERNEL_TARGET)),
    ("aricode-fat", None),
    ("aricode-iommu", None),
    ("aricode-lineedit", None),
    ("aricode-rtl8169", None),
    ("aricode-ssh", None),
    ("aricode-supervisor", None),
    ("aricode-tls", None),
    ("aricode-usb-storage", None),
    ("aricode-x509", None),
    ("aricode-xhci", None),
    ("ariboot", Some(UEFI_TARGET)),
    ("arikernel", Some(KERNEL_TARGET)),
    ("xtask", None),
];

pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    // The verifiers embed the selected public key (`aricode_crypto::trust`);
    // with the development key the variable is removed so a stray value in
    // the environment cannot change what a build trusts. The development
    // channel's public key is always set by xtask, never inherited (ADR
    // 0045); AriBoot's build removes it unless the machine opted in.
    match crate::keys::signing_public_hex() {
        Some(key) => c.env(crate::keys::SIGNING_PUB_ENV, key),
        None => c.env_remove(crate::keys::SIGNING_PUB_ENV),
    };
    match crate::keys::dev_channel_public_hex() {
        Ok(key) => c.env(crate::keys::DEV_CHANNEL_PUB_ENV, key),
        Err(_) => c.env_remove(crate::keys::DEV_CHANNEL_PUB_ENV),
    };
    c
}

pub fn run(mut c: Command) -> Result {
    let st = c
        .status()
        .map_err(|e| format!("{:?}: {e}", c.get_program()))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("command failed: {c:?}"))
    }
}

fn profile_dir(release: bool) -> &'static str {
    if release {
        "release"
    } else {
        "debug"
    }
}

pub struct Artifacts {
    pub ariboot: PathBuf,
    pub kernel: PathBuf,
    /// Directory with `init` and the user programs.
    pub user_bin: PathBuf,
}

/// Programs that go into the initrd: `init`, the services, the applications
/// and the `user/testprogs` binaries.
pub fn initrd_programs() -> Result<Vec<String>> {
    let mut names = vec![
        String::from("init"),
        String::from("virtio_blk"),
        String::from("virtio_net"),
        String::from("rtl_net"),
        String::from("sshd"),
        String::from("virtio_input"),
        String::from("usb_input"),
        String::from("usb_ehci"),
        String::from("console_bridge"),
        String::from("compositor"),
        String::from("fsd"),
        String::from("hello_app"),
        String::from("perms_check"),
        String::from("updater"),
        String::from("fs_victim"),
        String::from("fs_probe"),
        String::from("win_probe"),
        String::from("wasm_runner"),
        String::from("notes_reader"),
        String::from("installer"),
        String::from("bench"),
        String::from("shell"),
    ];
    let bins = root().join("user/testprogs/src/bin");
    for entry in std::fs::read_dir(&bins).map_err(|e| format!("{}: {e}", bins.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|x| x == "rs") {
            names.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// `NAME.manifest` files of `dir`, by file name.
fn manifests_in(dir: &std::path::Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for f in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = f.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|x| x == "manifest") {
            out.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                path,
            ));
        }
    }
    Ok(out)
}

/// Manifests of the test programs the shell launches through the broker
/// with capabilities (`user/testprogs/manifests/NAME.manifest`). They go
/// into the scenario initrds only: a stick for a real machine carries no
/// test data (ADR 0032), and without its manifest a test program gets no
/// capability there (`write_usb_image` removes them).
pub fn test_program_manifests() -> Result<Vec<(String, PathBuf)>> {
    manifests_in(&root().join("user/testprogs/manifests"))
}

/// Application manifests (`apps/*/NAME.manifest`) and those of the test
/// programs: they go into the initrd under their file name, next to the
/// program they describe.
pub fn initrd_manifests() -> Result<Vec<(String, PathBuf)>> {
    let apps = root().join("apps");
    let mut out = test_program_manifests()?;
    for entry in std::fs::read_dir(&apps).map_err(|e| format!("{}: {e}", apps.display()))? {
        let dir = entry.map_err(|e| e.to_string())?.path();
        if dir.is_dir() {
            out.extend(manifests_in(&dir)?);
        }
    }
    out.sort();
    Ok(out)
}

/// Packages of the initrd programs (`initrd_programs`).
pub const USER_PACKAGES: &[&str] = &[
    "user-testprogs",
    "ariinit",
    "ari-virtio-blk",
    "ari-virtio-net",
    "ari-rtl-net",
    "ari-sshd",
    "ari-virtio-input",
    "ari-usb-input",
    "ari-usb-ehci",
    "ari-console-bridge",
    "ari-compositor",
    "ari-fsd",
    "ari-app-hello",
    "ari-app-perms-check",
    "ari-app-updater",
    "ari-app-fs-probe",
    "ari-app-win-probe",
    "ari-app-wasm-runner",
    "ari-app-notes-reader",
    "ari-app-installer",
    "ari-app-bench",
    "ari-shell",
];

/// Builds `init` and the user programs (static ELFs at 0x400000) and
/// returns the directory with the binaries.
pub fn build_user_programs(release: bool) -> Result<PathBuf> {
    for pkg in USER_PACKAGES {
        println!("[xtask] build {pkg} ({KERNEL_TARGET})");
        let mut c = cargo();
        c.args(["build", "-p", pkg, "--target", KERNEL_TARGET]);
        if release {
            c.arg("--release");
        }
        // Same flags as the kernel (.cargo/config.toml): the `kernel` code
        // model allows signed 32-bit absolute addresses, so 0x400000 is
        // valid, and it matches the precompiled `core` (LTO).
        run(c)?;
    }
    let out = root()
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile_dir(release));
    // A program without its `build.rs` (`aricode_build::user_program`) is
    // linked at the default address and the kernel rejects it at boot (the
    // ELF does not fit in the user region): better to fail here.
    for name in initrd_programs()? {
        let bin = out.join(&name);
        let header = std::fs::read(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
        check_user_elf(&name, &header)?;
    }
    Ok(out)
}

/// Base address of the user programs (`aricode-build`).
pub const USER_BASE: u64 = 0x40_0000;

/// The ELF entry point must be in the user region.
pub fn check_user_elf(name: &str, elf: &[u8]) -> Result {
    if elf.len() < 32 || &elf[..4] != b"\x7fELF" {
        return Err(format!("{name}: not an ELF"));
    }
    let entry = u64::from_le_bytes(elf[24..32].try_into().expect("8 bytes"));
    if entry < USER_BASE {
        return Err(format!(
            "{name}: entry point {entry:#x} outside the user region (missing build.rs with aricode_build::user_program)"
        ));
    }
    Ok(())
}

/// Program built out of tree and packaged (ADR 0017).
#[derive(Clone)]
pub struct ExternalApp {
    pub name: String,
    pub version: u32,
    /// Signed package (`aricode-pkg`).
    pub package: Vec<u8>,
}

impl ExternalApp {
    /// File that goes to AriFS as an installed program.
    pub fn installed(&self) -> (String, Vec<u8>) {
        (
            format!("apps/{}{}", self.name, aricode_pkg::EXTENSION),
            self.package.clone(),
        )
    }
}

/// Builds `dir/Cargo.toml` (its own workspace with the SDK by path) from the
/// repository root, so that `rust-toolchain.toml` and `.cargo/config.toml`
/// apply just as they do to the initrd programs, with `APP_VERSION` set;
/// signs the package with the development key. `dir` holds a single
/// `NAME.manifest`, which gives the binary name. With `corrupt`, the
/// signature does not verify (tests).
///
/// Each (app, version, profile, corrupt) is built once per process and
/// then served from memory: the test runner builds every package its
/// scenarios need before they run concurrently (ADR 0040), so a scenario
/// never starts cargo and its output never interleaves with theirs.
/// Packages that ship as data (scenario installs, update sources) are built
/// in the release profile whatever the OS build, as the WebAssembly demo:
/// a dev build of `hello_ext` (75,408 bytes) is larger than any machine can
/// install (`PACKAGE_MAX`, incident 0026). Only `app-pack --debug` builds
/// one in the dev profile, and is refused if it does not fit.
pub const PACKAGES_RELEASE: bool = true;

pub fn build_external(
    dir: &Path,
    version: u32,
    release: bool,
    corrupt: bool,
) -> Result<ExternalApp> {
    type Key = (PathBuf, u32, bool, bool);
    static BUILT: std::sync::Mutex<Vec<(Key, ExternalApp)>> = std::sync::Mutex::new(Vec::new());
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        root().join(dir)
    };
    let key = (dir.clone(), version, release, corrupt);
    if let Some((_, app)) = BUILT
        .lock()
        .expect("built packages")
        .iter()
        .find(|(k, _)| *k == key)
    {
        return Ok(app.clone());
    }
    let app = build_external_now(&dir, version, release, corrupt)?;
    BUILT
        .lock()
        .expect("built packages")
        .push((key, app.clone()));
    Ok(app)
}

fn build_external_now(
    dir: &Path,
    version: u32,
    release: bool,
    corrupt: bool,
) -> Result<ExternalApp> {
    let mut manifests = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "manifest"));
    let (Some(manifest_path), None) = (manifests.next(), manifests.next()) else {
        return Err(format!(
            "{}: exactly one NAME.manifest is required",
            dir.display()
        ));
    };
    let name = manifest_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let target_dir = crate::build_dir()
        .join("external")
        .join(format!("{name}-v{version}"));
    println!(
        "[xtask] external build {name} v{version} ({}, {KERNEL_TARGET})",
        dir.display()
    );
    let mut c = cargo();
    c.env("APP_VERSION", version.to_string())
        .args(["build", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(["--target", KERNEL_TARGET, "--target-dir"])
        .arg(&target_dir);
    if release {
        c.arg("--release");
    }
    run(c)?;
    let elf = target_dir
        .join(KERNEL_TARGET)
        .join(profile_dir(release))
        .join(&name);
    let elf = crate::image::stripped_elf(&elf)?;
    let manifest =
        std::fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let seed = crate::keys::signing_seed()?;
    let package = aricode_pkg::build(&name, version, &elf, &manifest, &seed, corrupt)
        .ok_or_else(|| format!("{name}: invalid package name"))?;
    check_package_size(&name, package.len(), release)?;
    // Post-build check (re-audit, R1): a package meant to be valid must
    // verify with the key the build embeds.
    if !corrupt && aricode_pkg::verify(&package, &crate::keys::expected_public_key()?).is_err() {
        return Err(format!(
            "{name}: the package does not verify with the embedded public key"
        ));
    }
    Ok(ExternalApp {
        name,
        version,
        package,
    })
}

/// A package the machines cannot install is never produced (incident
/// 0026): one AriFS replace writes at most `PACKAGE_MAX` bytes.
fn check_package_size(name: &str, len: usize, release: bool) -> Result {
    let max = aricode_pkg::PACKAGE_MAX;
    if len > max {
        let hint = if release {
            ""
        } else {
            "; dev builds are larger, the release profile may fit"
        };
        return Err(format!(
            "{name}: package of {len} bytes, larger than the {max} a machine can install (PACKAGE_MAX, AriFS atomic replace; incident 0026){hint}"
        ));
    }
    Ok(())
}

/// `cargo xtask app-pack DIR [--version N]`: produces `build/pkgs/NAME-N.aripkg`.
pub fn app_pack(dir: &Path, version: u32, release: bool) -> Result {
    let app = build_external(dir, version, release, false)?;
    let out = crate::build_dir().join("pkgs");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let file = out.join(format!(
        "{}-{}{}",
        app.name,
        app.version,
        aricode_pkg::EXTENSION
    ));
    std::fs::write(&file, &app.package).map_err(|e| format!("{}: {e}", file.display()))?;
    println!(
        "[xtask] package {} v{} ({} bytes) at {}",
        app.name,
        app.version,
        app.package.len(),
        file.display()
    );
    Ok(())
}

/// Builds the sample WebAssembly modules and leaves `demo.wasm` in `build/`,
/// from where `write_test_disk` copies it into the runner's private directory.
/// Always in the release profile, a dev build of the OS included: the module
/// is data for the runner, and its dev build (636,854 bytes against 12,050,
/// found 2026-10-23 while reproducing incident 0018 on a dev build) does not
/// fit the test disk's ARI0, so `cargo xtask test --debug` failed before any
/// scenario.
pub fn build_wasm_programs() -> Result {
    println!("[xtask] build wasm-demo ({WASM_TARGET}, release)");
    let mut c = cargo();
    c.args([
        "build",
        "-p",
        "wasm-demo",
        "--target",
        WASM_TARGET,
        "--release",
    ]);
    run(c)?;
    let src = root()
        .join("target")
        .join(WASM_TARGET)
        .join(profile_dir(true))
        .join("wasm_demo.wasm");
    let dst = crate::build_dir().join("demo.wasm");
    std::fs::create_dir_all(crate::build_dir()).map_err(|e| e.to_string())?;
    std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
    Ok(())
}

/// Builds AriBoot and AriKernel. A single binary of each serves every
/// scenario: test behaviour is controlled through the cmdline.
pub fn build_os(release: bool) -> Result<Artifacts> {
    let user_bin = build_user_programs(release)?;
    build_wasm_programs()?;
    for (pkg, target) in [("ariboot", UEFI_TARGET), ("arikernel", KERNEL_TARGET)] {
        println!("[xtask] build {pkg} ({target})");
        let mut c = cargo();
        c.args(["build", "-p", pkg, "--target", target]);
        if release {
            c.arg("--release");
        }
        if pkg == "ariboot" && !crate::keys::loader_trusts_dev() {
            // Least privilege (ADR 0045): only a machine that opted in has
            // a loader that boots development builds.
            c.env_remove(crate::keys::DEV_CHANNEL_PUB_ENV);
        }
        run(c)?;
    }
    let t = root().join("target");
    Ok(Artifacts {
        ariboot: t
            .join(UEFI_TARGET)
            .join(profile_dir(release))
            .join("ariboot.efi"),
        kernel: t
            .join(KERNEL_TARGET)
            .join(profile_dir(release))
            .join("arikernel"),
        user_bin,
    })
}

/// Host unit tests of every crate that is not bare-metal.
pub fn unit_tests() -> Result {
    let mut c = cargo();
    c.args(["test", "--workspace"]);
    for (pkg, target) in PACKAGES {
        if target.is_some() {
            c.args(["--exclude", pkg]);
        }
    }
    run(c)
}

pub fn check() -> Result {
    let mut c = cargo();
    c.args(["fmt", "--all", "--", "--check"]);
    run(c)?;
    for (pkg, target) in PACKAGES {
        let mut c = cargo();
        c.args(["clippy", "-p", pkg]);
        if let Some(t) = target {
            c.args(["--target", t]);
        }
        c.args(["--", "-D", "warnings"]);
        run(c)?;
    }
    crate::deps::check()?;
    // Every initrd program's stack budget (incident 0018).
    crate::stack::check()
}

#[cfg(test)]
mod tests {
    #[test]
    fn packages_machines_cannot_install_are_not_built() {
        let max = aricode_pkg::PACKAGE_MAX;
        assert!(super::check_package_size("app", max, true).is_ok());
        assert!(super::check_package_size("app", max + 1, true).is_err());
        let dev = super::check_package_size("app", max + 1, false).unwrap_err();
        assert!(dev.contains("release profile"), "{dev}");
    }

    use super::*;

    /// Every workspace package is linted by `check` and, when it runs on the
    /// host, unit-tested: a package missing from `PACKAGES` was silently
    /// never linted (found 2026-10-17: 14 packages, sshd among them).
    #[test]
    fn every_workspace_package_is_listed() {
        let read = |p: &std::path::Path| -> toml::Table {
            std::fs::read_to_string(p)
                .expect("read Cargo.toml")
                .parse()
                .expect("parse Cargo.toml")
        };
        let workspace = read(&root().join("Cargo.toml"));
        let members = workspace["workspace"]["members"]
            .as_array()
            .expect("workspace members");
        let mut missing = Vec::new();
        for member in members {
            let dir = member.as_str().expect("member path");
            let manifest = read(&root().join(dir).join("Cargo.toml"));
            let name = manifest["package"]["name"].as_str().expect("package name");
            if name != "xtask" && !PACKAGES.iter().any(|(p, _)| *p == name) {
                missing.push(name.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "packages missing from PACKAGES: {missing:?}"
        );
    }

    #[test]
    fn user_elf_entry_must_be_in_user_region() {
        let mut elf = [0u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[24..32].copy_from_slice(&0x0020_1d00u64.to_le_bytes());
        assert!(check_user_elf("x", &elf).unwrap_err().contains("build.rs"));
        elf[24..32].copy_from_slice(&0x0040_0000u64.to_le_bytes());
        assert!(check_user_elf("x", &elf).is_ok());
        assert!(check_user_elf("x", b"nope").is_err());
    }
}
