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
/// User programs (ADR 0065): built for this target as freestanding static
/// binaries, without its C library or anything of Linux, so they can use
/// SSE and AVX. [`user_cargo`] sets the flags that make them so.
pub const USER_TARGET: &str = "x86_64-unknown-linux-gnu";

/// Code generation and link flags of user programs. Passed through the
/// environment, not `.cargo/config.toml`: the host's own builds (xtask,
/// unit tests, build scripts) share the triple and must not get them.
/// Cargo gives these flags to target crates only when `--target` is
/// passed, never to build scripts.
/// - `relocation-model=static`: a static ELF at its linked address
///   (`sdk/rust/user.ld`); no loader relocates it.
/// - `crt-static`: linked `-static`, never against a dynamic loader.
/// - `rust-lld`: the toolchain's own linker, so no host C toolchain or C
///   library takes part; an unresolved libc symbol fails the link.
pub const USER_RUSTFLAGS: &[&str] = &[
    "-Crelocation-model=static",
    "-Ctarget-feature=+crt-static",
    "-Clinker=rust-lld",
    "-Clinker-flavor=ld.lld",
];

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
    ("aricode-rt", Some(USER_TARGET)),
    ("ari-app-hello", Some(USER_TARGET)),
    ("ari-app-perms-check", Some(USER_TARGET)),
    ("ari-app-updater", Some(USER_TARGET)),
    ("ari-app-fs-probe", Some(USER_TARGET)),
    ("ari-app-win-probe", Some(USER_TARGET)),
    ("ari-app-wasm-runner", Some(USER_TARGET)),
    ("ari-app-notes-reader", Some(USER_TARGET)),
    ("ari-app-installer", Some(USER_TARGET)),
    ("ari-app-bench", Some(USER_TARGET)),
    ("wasm-demo", Some(WASM_TARGET)),
    ("user-testprogs", Some(USER_TARGET)),
    ("ariinit", Some(USER_TARGET)),
    ("ari-virtio-blk", Some(USER_TARGET)),
    ("ari-virtio-net", Some(USER_TARGET)),
    ("ari-virtio-input", Some(USER_TARGET)),
    ("ari-usb-input", Some(USER_TARGET)),
    ("ari-usb-ehci", Some(USER_TARGET)),
    ("ari-console-bridge", Some(USER_TARGET)),
    ("ari-compositor", Some(USER_TARGET)),
    ("ari-fsd", Some(USER_TARGET)),
    ("ari-shell", Some(USER_TARGET)),
    ("ari-net-core", Some(USER_TARGET)),
    ("ari-rtl-net", Some(USER_TARGET)),
    ("ari-sshd", Some(USER_TARGET)),
    ("aricode-https", Some(USER_TARGET)),
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

/// [`cargo`] for a command that builds or lints user programs for
/// [`USER_TARGET`]; the caller passes `--target USER_TARGET`.
pub fn user_cargo() -> Command {
    let mut c = cargo();
    c.env("CARGO_ENCODED_RUSTFLAGS", USER_RUSTFLAGS.join("\x1f"));
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
        println!("[xtask] build {pkg} ({USER_TARGET})");
        let mut c = user_cargo();
        c.args(["build", "-p", pkg, "--target", USER_TARGET]);
        if release {
            c.arg("--release");
        }
        run(c)?;
    }
    let out = root()
        .join("target")
        .join(USER_TARGET)
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

/// A user program is a static executable whose entry point is in the user
/// region. Built for [`USER_TARGET`] (ADR 0065), it must also not ask for
/// a dynamic loader, thread-local storage or relocations at load time
/// (a statically linked C library brings `IRELATIVE` ones): nothing in
/// AriCodeOS would process them.
pub fn check_user_elf(name: &str, elf: &[u8]) -> Result {
    const ET_EXEC: u16 = 2;
    const PT_DYNAMIC: u32 = 2;
    const PT_INTERP: u32 = 3;
    const PT_TLS: u32 = 7;
    const SHT_RELA: u32 = 4;
    const SHT_REL: u32 = 9;
    const SHF_ALLOC: u64 = 2;
    let u16_at = |o: usize| elf.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| {
        elf.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
    };
    let u64_at = |o: usize| {
        elf.get(o..o + 8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
    };
    if elf.len() < 64 || &elf[..4] != b"\x7fELF" {
        return Err(format!("{name}: not an ELF"));
    }
    let entry = u64_at(24).expect("header");
    if entry < USER_BASE {
        return Err(format!(
            "{name}: entry point {entry:#x} outside the user region (missing build.rs with aricode_build::user_program)"
        ));
    }
    if u16_at(16) != Some(ET_EXEC) {
        return Err(format!(
            "{name}: not a static executable (position-independent or shared object; ADR 0065)"
        ));
    }
    let phoff = u64_at(32).expect("header") as usize;
    let phentsize = u16_at(54).expect("header") as usize;
    let phnum = u16_at(56).expect("header") as usize;
    for i in 0..phnum {
        let at = phoff
            .checked_add(i * phentsize)
            .ok_or_else(|| format!("{name}: program headers out of range"))?;
        match u32_at(at) {
            Some(PT_INTERP) => {
                return Err(format!(
                    "{name}: asks for a dynamic loader (PT_INTERP; ADR 0065)"
                ))
            }
            Some(PT_DYNAMIC) => {
                return Err(format!(
                    "{name}: has a dynamic section (PT_DYNAMIC; ADR 0065)"
                ))
            }
            Some(PT_TLS) => {
                return Err(format!(
                    "{name}: uses thread-local storage (PT_TLS; ADR 0065)"
                ))
            }
            Some(_) => {}
            None => return Err(format!("{name}: program headers out of range")),
        }
    }
    let shoff = u64_at(40).expect("header") as usize;
    let shentsize = u16_at(58).expect("header") as usize;
    let shnum = u16_at(60).expect("header") as usize;
    for i in 0..shnum {
        let at = shoff
            .checked_add(i * shentsize)
            .ok_or_else(|| format!("{name}: section headers out of range"))?;
        let (Some(kind), Some(flags)) = (u32_at(at + 4), u64_at(at + 8)) else {
            return Err(format!("{name}: section headers out of range"));
        };
        if matches!(kind, SHT_RELA | SHT_REL) && flags & SHF_ALLOC != 0 {
            return Err(format!(
                "{name}: carries relocations to apply at load time (ADR 0065)"
            ));
        }
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
        "[xtask] external build {name} v{version} ({}, {USER_TARGET})",
        dir.display()
    );
    let mut c = user_cargo();
    c.env("APP_VERSION", version.to_string())
        .args(["build", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(["--target", USER_TARGET, "--target-dir"])
        .arg(&target_dir);
    if release {
        c.arg("--release");
    }
    run(c)?;
    let elf = target_dir
        .join(USER_TARGET)
        .join(profile_dir(release))
        .join(&name);
    let built = std::fs::read(&elf).map_err(|e| format!("{}: {e}", elf.display()))?;
    check_user_elf(&name, &built)?;
    crate::deps::check_external(dir)?;
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
        // Any of these would replace the target's flags in
        // .cargo/config.toml, the portable cryptography's included (audit
        // of ADR 0066, L5).
        for v in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
        ] {
            c.env_remove(v);
        }
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
    let kernel = t
        .join(KERNEL_TARGET)
        .join(profile_dir(release))
        .join("arikernel");
    let loader = t
        .join(UEFI_TARGET)
        .join(profile_dir(release))
        .join("ariboot.efi");
    for (name, path) in [("arikernel", &kernel), ("ariboot", &loader)] {
        let listing = crate::stack::disassemble(path)?;
        check_no_simd(name, &listing)?;
    }
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

/// The kernel and AriBoot must not touch a vector register (XMM, YMM, ZMM,
/// MMX or an AVX-512 mask): user threads' registers stay live in the
/// kernel (ADR 0065), and the adopted cryptography would use AES-NI or
/// AVX2 if its portable backends were not forced (ADR 0066). Saving and
/// restoring the whole state (`fxsave`, `xrstor`...) names no register and
/// is allowed. `listing` is `objdump -d` output in AT&T syntax.
pub fn check_no_simd(name: &str, listing: &str) -> Result {
    // `%xmm0`..`%zmm31`, `%mm0`..`%mm7`, `%k0`..`%k7`: a prefix and a digit.
    let uses_vector_register = |line: &str| {
        line.match_indices('%').any(|(i, _)| {
            let rest = &line.as_bytes()[i + 1..];
            ["xmm", "ymm", "zmm", "mm", "k"].iter().any(|p| {
                rest.starts_with(p.as_bytes()) && rest.get(p.len()).is_some_and(u8::is_ascii_digit)
            })
        })
    };
    // Instructions that change vector state without naming a register:
    // the upper halves of YMM, MXCSR, the MMX/x87 switch (audit of ADR
    // 0066, L4). The kernel's own `fxsave`/`xrstor`, `fninit` and the
    // error-pointer reset (`fnclex`, `emms`, `fild`) are allowed.
    const IMPLICIT: [&str; 4] = ["vzeroupper", "vzeroall", "ldmxcsr", "femms"];
    let implicit = |line: &str| line.split_whitespace().any(|w| IMPLICIT.contains(&w));
    let offending: Vec<&str> = listing
        .lines()
        .filter(|l| uses_vector_register(l) || implicit(l))
        .take(5)
        .collect();
    if offending.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{name} uses vector registers, which the kernel and AriBoot must never do (ADR 0065, ADR 0066); first instructions:\n  {}",
            offending.join("\n  ")
        ))
    }
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
    run(c)?;
    // The host picks the SIMD backends at run time; the kernel and AriBoot
    // run the portable ones (ADR 0066), which this tests as they build
    // them (audit of ADR 0066, L3). Its own target directory keeps the
    // flags from rebuilding the host's.
    println!("[xtask] aricode-crypto with the trusted base's portable backends");
    let mut c = cargo();
    c.env("CARGO_ENCODED_RUSTFLAGS", TRUSTED_CRYPTO_CFGS.join("\x1f"))
        .args([
            "test",
            "-p",
            "aricode-crypto",
            "--features",
            "soft",
            "--target-dir",
        ])
        .arg(root().join("target").join("soft-crypto"));
    run(c)
}

/// The `--cfg` flags `.cargo/config.toml` gives the kernel's and AriBoot's
/// targets, for the host test of their cryptography (a unit test keeps the
/// two lists equal).
const TRUSTED_CRYPTO_CFGS: &[&str] = &[
    "--cfg",
    "aes_force_soft",
    "--cfg",
    "polyval_force_soft",
    "--cfg",
    "chacha20_force_soft",
    "--cfg",
    "poly1305_force_soft",
    "--cfg",
    "curve25519_dalek_backend=\"serial\"",
];

pub fn check() -> Result {
    let mut c = cargo();
    c.args(["fmt", "--all", "--", "--check"]);
    run(c)?;
    for (pkg, target) in PACKAGES {
        let mut c = if *target == Some(USER_TARGET) {
            user_cargo()
        } else {
            cargo()
        };
        c.args(["clippy", "-p", pkg]);
        if let Some(t) = target {
            c.args(["--target", t]);
        }
        c.args(["--", "-D", "warnings"]);
        run(c)?;
    }
    let user: Vec<&str> = PACKAGES
        .iter()
        .filter(|(_, t)| *t == Some(USER_TARGET))
        .map(|(p, _)| *p)
        .collect();
    crate::deps::check(&user)?;
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

    /// An ELF header (static executable at the user base) followed by
    /// one program header of type `ptype`.
    fn elf_with(ptype: u32) -> Vec<u8> {
        let mut elf = vec![0u8; 64 + 56];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[24..32].copy_from_slice(&0x0040_0000u64.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[64..68].copy_from_slice(&ptype.to_le_bytes());
        elf
    }

    /// The host test of the trusted base's cryptography uses the flags its
    /// targets get from .cargo/config.toml.
    #[test]
    fn trusted_crypto_cfgs_match_the_config() {
        let config: toml::Table = std::fs::read_to_string(root().join(".cargo/config.toml"))
            .expect("read .cargo/config.toml")
            .parse()
            .expect("parse .cargo/config.toml");
        for target in [KERNEL_TARGET, UEFI_TARGET] {
            let flags: Vec<&str> = config["target"][target]["rustflags"]
                .as_array()
                .expect("rustflags")
                .iter()
                .map(|f| f.as_str().expect("flag"))
                .collect();
            let cfgs: Vec<&str> = flags
                .windows(2)
                .filter(|w| w[0] == "--cfg")
                .flat_map(|w| ["--cfg", w[1]])
                .collect();
            assert_eq!(cfgs, TRUSTED_CRYPTO_CFGS, "{target}");
        }
    }

    #[test]
    fn trusted_base_never_touches_vector_registers() {
        let clean = "ffffffff80001000: fxsave64 (%rdi)\n  ffff: xrstor64 (%rsi)\n  ffff: mov %rax,%rbx\n  ffff: fninit\n  ffff: emms\n  ffff: mov %r8,%rcx\n";
        assert!(check_no_simd("k", clean).is_ok());
        for bad in [
            "  1000: movdqa %xmm0,(%rdi)",
            "  1000: vpxor %ymm1,%ymm1,%ymm1",
            "  1000: vmovdqa64 %zmm3,(%rax)",
            "  1000: movq %mm0,%rax",
            "  1000: kmovw %k1,%eax",
            "  1000: aesenc %xmm1,%xmm0",
            "  1000:\tvzeroupper",
            "  1000:\tldmxcsr (%rsp)",
        ] {
            let e = check_no_simd("k", bad).unwrap_err();
            assert!(e.contains("vector registers"), "{bad}: {e}");
        }
    }

    #[test]
    fn user_elf_entry_must_be_in_user_region() {
        let mut elf = elf_with(1);
        assert!(check_user_elf("x", &elf).is_ok());
        elf[24..32].copy_from_slice(&0x0020_1d00u64.to_le_bytes());
        assert!(check_user_elf("x", &elf).unwrap_err().contains("build.rs"));
        assert!(check_user_elf("x", b"nope").is_err());
    }

    /// ADR 0065: a program built for the Linux target that would need a
    /// loader is refused, not shipped.
    #[test]
    fn user_elf_must_be_static() {
        let interp = check_user_elf("x", &elf_with(3)).unwrap_err();
        assert!(interp.contains("PT_INTERP"), "{interp}");
        let dynamic = check_user_elf("x", &elf_with(2)).unwrap_err();
        assert!(dynamic.contains("PT_DYNAMIC"), "{dynamic}");
        let mut pie = elf_with(1);
        pie[16..18].copy_from_slice(&3u16.to_le_bytes());
        assert!(check_user_elf("x", &pie).unwrap_err().contains("static"));
        let mut cut = elf_with(1);
        cut.truncate(66);
        assert!(check_user_elf("x", &cut)
            .unwrap_err()
            .contains("out of range"));
        let tls = check_user_elf("x", &elf_with(7)).unwrap_err();
        assert!(tls.contains("PT_TLS"), "{tls}");
    }

    /// A loaded `.rela` section (static glibc's `.rela.iplt`) is refused;
    /// an unloaded one (relocations kept for a debugger) is not.
    #[test]
    fn user_elf_must_not_need_load_time_relocations() {
        let with_section = |kind: u32, flags: u64| {
            let mut elf = elf_with(1);
            let shoff = elf.len() as u64;
            elf.resize(elf.len() + 64, 0);
            elf[40..48].copy_from_slice(&shoff.to_le_bytes());
            elf[58..60].copy_from_slice(&64u16.to_le_bytes());
            elf[60..62].copy_from_slice(&1u16.to_le_bytes());
            let at = shoff as usize;
            elf[at + 4..at + 8].copy_from_slice(&kind.to_le_bytes());
            elf[at + 8..at + 16].copy_from_slice(&flags.to_le_bytes());
            elf
        };
        let rela = check_user_elf("x", &with_section(4, 2)).unwrap_err();
        assert!(rela.contains("load time"), "{rela}");
        assert!(check_user_elf("x", &with_section(9, 2)).is_err());
        assert!(check_user_elf("x", &with_section(4, 0)).is_ok());
        assert!(check_user_elf("x", &with_section(1, 2)).is_ok());
    }
}
