//! Montaje de la partición EFI (directorio que QEMU expone como FAT).

use crate::build::{build_os, Artifacts};
use crate::scenario::Inject;
use crate::{build_dir, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
pub const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

const BOOTX64: &str = "EFI/BOOT/BOOTX64.EFI";
const KERNEL: &str = "aricodeos/kernel.elf";
const CMDLINE: &str = "aricodeos/cmdline.txt";

fn io<T>(r: std::io::Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

/// ESP base en `build/esp/`: BOOTX64.EFI + kernel.elf, sin cmdline.
pub fn build_base_esp(release: bool) -> Result<PathBuf> {
    let a = build_os(release)?;
    let esp = build_dir().join("esp");
    write_esp(&esp, &a, None)?;
    println!("[xtask] imagen ESP en {}", esp.display());
    Ok(esp)
}

/// ESP para un escenario: copia del base + cmdline + inyecciones de fallo.
pub fn build_scenario_esp(
    base: &Path,
    name: &str,
    cmdline: &str,
    inject: &Inject,
) -> Result<PathBuf> {
    let esp = build_dir().join("scenarios").join(name).join("esp");
    let _ = fs::remove_dir_all(&esp);
    copy_dir(base, &esp)?;
    if !cmdline.trim().is_empty() {
        io(fs::write(esp.join(CMDLINE), cmdline), "escribir cmdline")?;
    }
    let kernel = esp.join(KERNEL);
    if inject.remove_kernel {
        io(fs::remove_file(&kernel), "quitar kernel")?;
    } else if let Some(n) = inject.truncate_kernel {
        let data = io(fs::read(&kernel), "leer kernel")?;
        io(
            fs::write(&kernel, &data[..n.min(data.len())]),
            "truncar kernel",
        )?;
    }
    Ok(esp)
}

fn write_esp(esp: &Path, a: &Artifacts, cmdline: Option<&str>) -> Result {
    let _ = fs::remove_dir_all(esp);
    for rel in [BOOTX64, KERNEL] {
        io(
            fs::create_dir_all(esp.join(rel).parent().unwrap()),
            "crear ESP",
        )?;
    }
    io(fs::copy(&a.ariboot, esp.join(BOOTX64)), "copiar ariboot")?;
    io(fs::copy(&a.kernel, esp.join(KERNEL)), "copiar kernel")?;
    if let Some(c) = cmdline {
        io(fs::write(esp.join(CMDLINE), c), "escribir cmdline")?;
    }
    Ok(())
}

/// Copia privada de las variables OVMF: el firmware las escribe al arrancar.
pub fn ovmf_vars_for(dir: &Path) -> Result<PathBuf> {
    io(fs::create_dir_all(dir), "crear dir")?;
    let vars = dir.join("OVMF_VARS.fd");
    io(fs::copy(OVMF_VARS, &vars), "copiar OVMF_VARS")?;
    Ok(vars)
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    io(fs::create_dir_all(to), "crear dir")?;
    for entry in io(fs::read_dir(from), "leer dir")? {
        let entry = io(entry, "entrada")?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            io(fs::copy(entry.path(), target), "copiar")?;
        }
    }
    Ok(())
}
