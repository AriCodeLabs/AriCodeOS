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
const INITRD: &str = "aricodeos/initrd.img";

fn io<T>(r: std::io::Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

/// Raíz de los directorios de trabajo de los escenarios.
pub fn scenarios_root() -> PathBuf {
    build_dir().join("scenarios")
}

/// ESP base en `build/esp/`: BOOTX64.EFI + kernel.elf, sin cmdline.
pub fn build_base_esp(release: bool) -> Result<PathBuf> {
    let a = build_os(release)?;
    let esp = build_dir().join("esp");
    write_esp(&esp, &a, None)?;
    println!("[xtask] imagen ESP en {}", esp.display());
    Ok(esp)
}

/// Directorio de trabajo de un escenario bajo `scenarios_root`, listo para ser
/// borrado y reescrito.
///
/// Además de la validación léxica del nombre, se comprueba la contención real:
/// ni el directorio del escenario ni su `esp` pueden ser enlaces simbólicos, y
/// la ruta canónica resultante debe seguir dentro de la raíz canónica. Así un
/// enlace preexistente no puede redirigir el `remove_dir_all` fuera del árbol.
pub fn scenario_dir(scenarios_root: &Path, name: &str) -> Result<PathBuf> {
    crate::scenario::validate_name(name)?;
    io(fs::create_dir_all(scenarios_root), "crear scenarios/")?;
    let root = io(scenarios_root.canonicalize(), "canonicalizar scenarios/")?;
    let dir = root.join(name);
    reject_symlink(&dir)?;
    reject_symlink(&dir.join("esp"))?;
    io(fs::create_dir_all(&dir), "crear dir del escenario")?;
    let real = io(dir.canonicalize(), "canonicalizar dir del escenario")?;
    if !real.starts_with(&root) {
        return Err(format!("{} escapa de {}", real.display(), root.display()));
    }
    Ok(real)
}

fn reject_symlink(path: &Path) -> Result {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} es un enlace simbolico; no se usa como destino",
            path.display()
        )),
        _ => Ok(()),
    }
}

/// ESP para un escenario: copia del base + cmdline + inyecciones de fallo.
pub fn build_scenario_esp(
    base: &Path,
    scenarios_root: &Path,
    name: &str,
    cmdline: &str,
    inject: &Inject,
) -> Result<PathBuf> {
    let esp = scenario_dir(scenarios_root, name)?.join("esp");
    let _ = fs::remove_dir_all(&esp);
    copy_dir(base, &esp)?;
    if !cmdline.trim().is_empty() {
        io(fs::write(esp.join(CMDLINE), cmdline), "escribir cmdline")?;
    }
    let kernel = esp.join(KERNEL);
    if inject.remove_kernel {
        io(fs::remove_file(&kernel), "quitar kernel")?;
        return Ok(esp);
    }
    if let Some(n) = inject.truncate_kernel {
        let data = io(fs::read(&kernel), "leer kernel")?;
        io(
            fs::write(&kernel, &data[..n.min(data.len())]),
            "truncar kernel",
        )?;
    }
    if let Some(entry) = inject.patch_entry {
        let mut data = io(fs::read(&kernel), "leer kernel")?;
        elf_patch::set_entry(&mut data, entry)?;
        io(fs::write(&kernel, &data), "parchear e_entry")?;
    }
    if inject.add_empty_segment_at_entry {
        let mut data = io(fs::read(&kernel), "leer kernel")?;
        elf_patch::append_empty_rw_segment_at_entry(&mut data)?;
        io(fs::write(&kernel, &data), "anadir segmento vacio")?;
    }
    Ok(esp)
}

/// Manipulaciones mínimas de un ELF64 para inyectar fallos. Solo lo que los
/// escenarios necesitan; no es un parser general.
mod elf_patch {
    use crate::Result;

    const E_ENTRY: usize = 24;
    const E_PHOFF: usize = 32;
    const E_PHENTSIZE: usize = 54;
    const E_PHNUM: usize = 56;
    const PT_LOAD: u32 = 1;

    fn u16_at(d: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([d[o], d[o + 1]])
    }
    fn u64_at(d: &[u8], o: usize) -> u64 {
        u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
    }

    pub fn set_entry(data: &mut [u8], entry: u64) -> Result {
        if data.len() < 64 {
            return Err("patch_entry: kernel demasiado corto".into());
        }
        data[E_ENTRY..E_ENTRY + 8].copy_from_slice(&entry.to_le_bytes());
        Ok(())
    }

    /// Añade un PT_LOAD RW con `memsz = 0`, `vaddr = e_entry` y alineación 1
    /// en el hueco que sigue a la tabla de program headers.
    pub fn append_empty_rw_segment_at_entry(data: &mut [u8]) -> Result {
        if data.len() < 64 {
            return Err("add_empty_segment: kernel demasiado corto".into());
        }
        let phoff = u64_at(data, E_PHOFF) as usize;
        let phentsize = u16_at(data, E_PHENTSIZE) as usize;
        let phnum = u16_at(data, E_PHNUM) as usize;
        if phentsize != 56 {
            return Err(format!(
                "add_empty_segment: phentsize {phentsize} no soportado"
            ));
        }
        let new_ph = phoff + phentsize * phnum;
        // El primer byte de contenido de segmento marca el fin del hueco disponible.
        let first_content = (0..phnum)
            .map(|i| u64_at(data, phoff + i * phentsize + 8) as usize)
            .filter(|&off| off > 0)
            .min()
            .unwrap_or(data.len());
        if new_ph + phentsize > first_content || new_ph + phentsize > data.len() {
            return Err("add_empty_segment: sin hueco tras los program headers".into());
        }
        if data[new_ph..new_ph + phentsize].iter().any(|&b| b != 0) {
            return Err("add_empty_segment: el hueco no esta a cero".into());
        }
        let entry = u64_at(data, E_ENTRY);
        let ph = &mut data[new_ph..new_ph + phentsize];
        ph[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());
        ph[4..8].copy_from_slice(&6u32.to_le_bytes()); // PF_R | PF_W
        ph[8..16].copy_from_slice(&0u64.to_le_bytes()); // offset
        ph[16..24].copy_from_slice(&entry.to_le_bytes()); // vaddr
        ph[24..32].copy_from_slice(&entry.to_le_bytes()); // paddr
        ph[32..40].copy_from_slice(&0u64.to_le_bytes()); // filesz
        ph[40..48].copy_from_slice(&0u64.to_le_bytes()); // memsz
        ph[48..56].copy_from_slice(&1u64.to_le_bytes()); // align
        data[E_PHNUM..E_PHNUM + 2].copy_from_slice(&((phnum + 1) as u16).to_le_bytes());
        Ok(())
    }
}

/// Empaqueta `init` y los programas de usuario en un initrd.
fn build_initrd(user_bin: &Path) -> Result<Vec<u8>> {
    let names = crate::build::initrd_programs()?;
    let mut contents: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|n| io(fs::read(user_bin.join(n)), &format!("leer {n}")).map(|d| (n.clone(), d)))
        .collect::<Result<_>>()?;
    for (name, path) in crate::build::initrd_manifests()? {
        contents.push((name.clone(), io(fs::read(&path), &format!("leer {name}"))?));
    }
    let files: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    aricode_initrd::build(&files).map_err(|e| format!("initrd: {e:?}"))
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
    io(
        fs::write(esp.join(INITRD), build_initrd(&a.user_bin)?),
        "escribir initrd",
    )?;
    if let Some(c) = cmdline {
        io(fs::write(esp.join(CMDLINE), c), "escribir cmdline")?;
    }
    Ok(())
}

/// Disco de pruebas virtio-blk: 4 MiB con GPT y una partición AriCodeOS
/// (`ARI0`) formateada como AriFS con ficheros de ejemplo. Se regenera en
/// cada arranque (los tests escriben en él).
pub const DISK_SECTORS: u64 = 8192;

struct VecDisk<'a> {
    data: &'a mut [u8],
    base: u64,
}

impl aricode_arifs::BlockDevice for VecDisk<'_> {
    fn read_sector(
        &mut self,
        lba: u64,
        buf: &mut [u8; 512],
    ) -> std::result::Result<(), aricode_arifs::FsError> {
        let o = (self.base + lba) as usize * 512;
        buf.copy_from_slice(
            self.data
                .get(o..o + 512)
                .ok_or(aricode_arifs::FsError::Io)?,
        );
        Ok(())
    }
    fn write_sector(
        &mut self,
        lba: u64,
        buf: &[u8; 512],
    ) -> std::result::Result<(), aricode_arifs::FsError> {
        let o = (self.base + lba) as usize * 512;
        self.data
            .get_mut(o..o + 512)
            .ok_or(aricode_arifs::FsError::Io)?
            .copy_from_slice(buf);
        Ok(())
    }
}

/// Primer sector del área en bruto del disco de pruebas: desde aquí hasta el
/// final de la partición cada sector lleva `ARIDISK\0` y su LBA, fuera del
/// alcance de AriFS (32 ficheros × 16 sectores caben antes).
pub const RAW_PATTERN_LBA: u64 = 4096;

/// Ficheros de ejemplo del disco de pruebas.
pub const DISK_FILES: &[(&str, &[u8])] = &[
    ("hola.txt", b"Hola desde AriFS\n"),
    ("README", b"AriCodeOS: disco de pruebas con AriFS v0.\n"),
];

pub fn write_test_disk(path: &Path) -> Result {
    let mut img = vec![0u8; DISK_SECTORS as usize * 512];
    let (first, last) =
        aricode_gpt::write_disk(&mut img, &[]).map_err(|e| format!("gpt: {e:?}"))?;
    aricode_gpt::write_disk(
        &mut img,
        &[("ARI0", first, last, aricode_gpt::ARICODEOS_DATA_GUID)],
    )
    .map_err(|e| format!("gpt: {e:?}"))?;
    let sectors = last - first + 1;
    let mut fs = aricode_arifs::AriFs::format(
        VecDisk {
            data: &mut img,
            base: first,
        },
        sectors,
    )
    .map_err(|e| format!("arifs: {e:?}"))?;
    for (name, content) in DISK_FILES {
        let i = fs
            .create(name)
            .map_err(|e| format!("arifs {name}: {e:?}"))?;
        fs.write(i, 0, content)
            .map_err(|e| format!("arifs {name}: {e:?}"))?;
    }
    for lba in RAW_PATTERN_LBA..=last {
        let o = lba as usize * 512;
        img[o..o + 8].copy_from_slice(b"ARIDISK\0");
        img[o + 8..o + 16].copy_from_slice(&lba.to_le_bytes());
    }
    io(fs::write(path, img), "escribir disco de pruebas")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;
    use std::os::unix::fs::symlink;

    fn fake_base(tmp: &TempDir) -> PathBuf {
        let base = tmp.path().join("base");
        fs::create_dir_all(base.join("EFI/BOOT")).unwrap();
        fs::create_dir_all(base.join("aricodeos")).unwrap();
        fs::write(base.join(BOOTX64), b"efi").unwrap();
        fs::write(base.join(KERNEL), b"elf").unwrap();
        base
    }

    #[test]
    fn symlinked_scenario_dir_is_rejected_and_outside_data_survives() {
        // Revision de auditoria, hallazgo 2.
        let tmp = TempDir::new("image-symlink");
        let base = fake_base(&tmp);
        let root = tmp.path().join("scenarios");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(outside.join("esp")).unwrap();
        fs::write(outside.join("esp/marker"), b"keep").unwrap();
        fs::create_dir_all(&root).unwrap();
        symlink(&outside, root.join("boot.probe")).unwrap();

        let err =
            build_scenario_esp(&base, &root, "boot.probe", "", &Inject::default()).unwrap_err();
        assert!(err.contains("enlace simbolico"), "{err}");
        assert!(
            outside.join("esp/marker").exists(),
            "el centinela externo debe sobrevivir"
        );
        assert!(!outside.join(KERNEL).exists());
    }

    #[test]
    fn symlinked_esp_is_rejected() {
        let tmp = TempDir::new("image-symlink-esp");
        let base = fake_base(&tmp);
        let root = tmp.path().join("scenarios");
        let outside = tmp.path().join("outside-esp");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("marker"), b"keep").unwrap();
        fs::create_dir_all(root.join("boot.probe")).unwrap();
        symlink(&outside, root.join("boot.probe/esp")).unwrap();

        assert!(build_scenario_esp(&base, &root, "boot.probe", "", &Inject::default()).is_err());
        assert!(outside.join("marker").exists());
    }

    #[test]
    fn nominal_scenario_esp_is_built_inside_root() {
        let tmp = TempDir::new("image-nominal");
        let base = fake_base(&tmp);
        let root = tmp.path().join("scenarios");
        let inject = Inject {
            truncate_kernel: Some(2),
            ..Default::default()
        };
        let esp = build_scenario_esp(&base, &root, "boot.ok", "qemu_exit", &inject).unwrap();
        assert!(esp.starts_with(root.canonicalize().unwrap()));
        assert_eq!(fs::read(esp.join(KERNEL)).unwrap(), b"el");
        assert_eq!(fs::read(esp.join(CMDLINE)).unwrap(), b"qemu_exit");
    }

    #[test]
    fn empty_segment_injection_appends_a_load_header() {
        // ELF minimo: cabecera + 1 phdr + hueco a cero + contenido en 0x100.
        let mut d = vec![0u8; 0x110];
        d[E_PHOFF_TEST..E_PHOFF_TEST + 8].copy_from_slice(&64u64.to_le_bytes());
        d[54..56].copy_from_slice(&56u16.to_le_bytes());
        d[56..58].copy_from_slice(&1u16.to_le_bytes());
        d[24..32].copy_from_slice(&0x1234u64.to_le_bytes());
        d[64..68].copy_from_slice(&1u32.to_le_bytes());
        d[72..80].copy_from_slice(&0x100u64.to_le_bytes()); // offset del contenido
        elf_patch::append_empty_rw_segment_at_entry(&mut d).unwrap();
        assert_eq!(u16::from_le_bytes([d[56], d[57]]), 2);
        let ph = &d[64 + 56..64 + 112];
        assert_eq!(u32::from_le_bytes(ph[0..4].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(ph[16..24].try_into().unwrap()), 0x1234);
        assert_eq!(u64::from_le_bytes(ph[40..48].try_into().unwrap()), 0);
        // Sin hueco: el contenido empieza justo tras la tabla.
        let mut d2 = d.clone();
        d2[72..80].copy_from_slice(&(64u64 + 112).to_le_bytes());
        assert!(elf_patch::append_empty_rw_segment_at_entry(&mut d2).is_err());
    }

    const E_PHOFF_TEST: usize = 32;
}
