//! Compilación de las crates del sistema y comprobaciones estáticas.

use crate::{root, Result};
use std::path::PathBuf;
use std::process::Command;

pub const UEFI_TARGET: &str = "x86_64-unknown-uefi";
pub const KERNEL_TARGET: &str = "x86_64-unknown-none";

/// Paquetes del workspace y el target con el que se compilan/lintan.
/// Única fuente de verdad para `check` y para los tests unitarios de host.
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
    ("aricode-sys", None),
    ("user-testprogs", Some(KERNEL_TARGET)),
    ("ariboot", Some(UEFI_TARGET)),
    ("arikernel", Some(KERNEL_TARGET)),
    ("xtask", None),
];

pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    c
}

pub fn run(mut c: Command) -> Result {
    let st = c
        .status()
        .map_err(|e| format!("{:?}: {e}", c.get_program()))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("comando fallido: {c:?}"))
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
}

/// Compila los programas de usuario de prueba (ELF estáticos en 0x400000) y
/// devuelve el directorio con los binarios. El kernel los embebe, así que
/// van siempre antes que él.
pub fn build_user_programs(release: bool) -> Result<PathBuf> {
    println!("[xtask] build user-testprogs ({KERNEL_TARGET})");
    let mut c = cargo();
    c.args(["build", "-p", "user-testprogs", "--target", KERNEL_TARGET]);
    if release {
        c.arg("--release");
    }
    // Mismos flags que el kernel (.cargo/config.toml): el modelo de código
    // `kernel` admite direcciones absolutas de 32 bits con signo, así que
    // 0x400000 es válido, y coincide con el `core` precompilado (LTO).
    run(c)?;
    Ok(root()
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile_dir(release)))
}

/// Variable que el kernel lee en su `build.rs` para embeber los programas.
pub const USER_BIN_ENV: &str = "ARICODE_USER_BIN";

/// Compila AriBoot y AriKernel. Un único binario de cada uno sirve para todos
/// los escenarios: el comportamiento de test se controla por cmdline.
pub fn build_os(release: bool) -> Result<Artifacts> {
    let user_bin = build_user_programs(release)?;
    for (pkg, target) in [("ariboot", UEFI_TARGET), ("arikernel", KERNEL_TARGET)] {
        println!("[xtask] build {pkg} ({target})");
        let mut c = cargo();
        c.args(["build", "-p", pkg, "--target", target]);
        if release {
            c.arg("--release");
        }
        c.env(USER_BIN_ENV, &user_bin);
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
    })
}

/// Tests unitarios de host de todas las crates que no son bare-metal.
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
    // El kernel embebe los programas de usuario: deben existir para lintarlo.
    let user_bin = build_user_programs(true)?;
    for (pkg, target) in PACKAGES {
        let mut c = cargo();
        c.args(["clippy", "-p", pkg]);
        if let Some(t) = target {
            c.args(["--target", t]);
        }
        c.env(USER_BIN_ENV, &user_bin);
        c.args(["--", "-D", "warnings"]);
        run(c)?;
    }
    Ok(())
}
