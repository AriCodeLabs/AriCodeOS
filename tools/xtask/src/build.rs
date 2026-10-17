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
    ("aricode-initrd", None),
    ("aricode-acpi", None),
    ("aricode-pci", None),
    ("aricode-virtio", None),
    ("aricode-gpt", None),
    ("aricode-arifs", None),
    ("aricode-net", None),
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
    ("ari-console-bridge", Some(KERNEL_TARGET)),
    ("ari-compositor", Some(KERNEL_TARGET)),
    ("ari-fsd", Some(KERNEL_TARGET)),
    ("ari-shell", Some(KERNEL_TARGET)),
    ("ariboot", Some(UEFI_TARGET)),
    ("arikernel", Some(KERNEL_TARGET)),
    ("xtask", None),
];

pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    // The verifiers embed the selected public key (`aricode_crypto::trust`);
    // with the development key the variable is removed so a stray value in
    // the environment cannot change what a build trusts.
    match crate::keys::signing_public_hex() {
        Some(key) => c.env(crate::keys::SIGNING_PUB_ENV, key),
        None => c.env_remove(crate::keys::SIGNING_PUB_ENV),
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

/// Application manifests (`apps/*/NAME.manifest`): they go into the initrd
/// under their file name, next to the program they describe.
pub fn initrd_manifests() -> Result<Vec<(String, PathBuf)>> {
    let apps = root().join("apps");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&apps).map_err(|e| format!("{}: {e}", apps.display()))? {
        let dir = entry.map_err(|e| e.to_string())?.path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = f.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|x| x == "manifest") {
                out.push((
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    path,
                ));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Builds `init` and the user programs (static ELFs at 0x400000) and
/// returns the directory with the binaries.
pub fn build_user_programs(release: bool) -> Result<PathBuf> {
    for pkg in [
        "user-testprogs",
        "ariinit",
        "ari-virtio-blk",
        "ari-virtio-net",
        "ari-rtl-net",
        "ari-sshd",
        "ari-virtio-input",
        "ari-usb-input",
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
    ] {
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
pub fn build_external(
    dir: &Path,
    version: u32,
    release: bool,
    corrupt: bool,
) -> Result<ExternalApp> {
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        root().join(dir)
    };
    let mut manifests = std::fs::read_dir(&dir)
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
    Ok(ExternalApp {
        name,
        version,
        package,
    })
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
pub fn build_wasm_programs(release: bool) -> Result {
    println!("[xtask] build wasm-demo ({WASM_TARGET})");
    let mut c = cargo();
    c.args(["build", "-p", "wasm-demo", "--target", WASM_TARGET]);
    if release {
        c.arg("--release");
    }
    run(c)?;
    let src = root()
        .join("target")
        .join(WASM_TARGET)
        .join(profile_dir(release))
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
    build_wasm_programs(release)?;
    for (pkg, target) in [("ariboot", UEFI_TARGET), ("arikernel", KERNEL_TARGET)] {
        println!("[xtask] build {pkg} ({target})");
        let mut c = cargo();
        c.args(["build", "-p", pkg, "--target", target]);
        if release {
            c.arg("--release");
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
