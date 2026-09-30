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
    // Programas sin informacion de depuracion: el initrd pasa de ~5 MiB a
    // unos cientos de KiB y el arranque es mas rapido.
    let mut contents: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|n| stripped_elf(&user_bin.join(n)).map(|d| (n.clone(), d)))
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

/// Disco de pruebas virtio-blk: 12 MiB con GPT y cinco particiones. Se
/// regenera en cada arranque (los tests escriben en él).
///
/// ```text
/// ARI0     34..6143      AriFS con ficheros de ejemplo (patrón en bruto desde el LBA 4096)
/// SYSA     6144..30719   imagen de sistema firmada, slot A (versión 1), 12 MiB
/// SYSB     30720..55295  slot B (vacío, o la variante que pida el escenario)
/// UPDATE   55296..79871  imagen de actualización (versión 2) que instala `updater`
/// ARIBOOT  79872..79935  estado de arranque A/B (sector 0)
/// ```
///
/// El kernel de las imágenes va sin información de depuración (`strip`) si la
/// herramienta existe; con ella el ELF supera los 8 MiB.
pub const DISK_SECTORS: u64 = 98304;
pub const PART_ARI0: (u64, u64) = (34, 6143);
pub const PART_SYSA: (u64, u64) = (6144, 30719);
pub const PART_SYSB: (u64, u64) = (30720, 55295);
pub const PART_UPDATE: (u64, u64) = (55296, 79871);
pub const PART_ARIBOOT: (u64, u64) = (79872, 79935);

/// Estado inicial del disco que pide un escenario (`disk = "..."`).
/// Imagen para un pendrive (Fase 34): GPT con la ESP (FAT32, 64 MiB) y las
/// mismas particiones de AriCodeOS que el disco de pruebas, alineadas a 1 MiB.
///
/// ```text
/// ESP      2048..133119     EFI/BOOT/BOOTX64.EFI, aricodeos/{kernel.elf,initrd.img,cmdline.txt}
/// ARI0     133120..139263   AriFS con los ficheros de ejemplo
/// SYSA     139264..163839   imagen de sistema firmada, slot A
/// SYSB     163840..188415   slot B (vacío)
/// UPDATE   188416..212991   vacía
/// ARIBOOT  212992..213055   estado de arranque: A activo
/// ```
pub const USB_SECTORS: u64 = 213_120;
pub const USB_PART_ESP: (u64, u64) = (2048, 133_119);
pub const USB_PART_ARI0: (u64, u64) = (133_120, 139_263);
pub const USB_PART_SYSA: (u64, u64) = (139_264, 163_839);
pub const USB_PART_SYSB: (u64, u64) = (163_840, 188_415);
pub const USB_PART_UPDATE: (u64, u64) = (188_416, 212_991);
pub const USB_PART_ARIBOOT: (u64, u64) = (212_992, 213_055);
/// GUID de tipo de la partición de sistema EFI, en el orden en disco.
pub const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

/// Ficheros de un directorio (recursivo) como rutas relativas con `/`.
fn walk_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result {
    let mut entries: Vec<_> = io(fs::read_dir(dir), "leer dir")?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if e.path().is_dir() {
            walk_files(&e.path(), &rel, out)?;
        } else {
            out.push((rel, e.path()));
        }
    }
    Ok(())
}

/// Escribe la imagen USB completa en `path` con el contenido de `esp_dir`
/// como partición EFI y el slot A cargado con la imagen de sistema.
pub fn write_usb_image(path: &Path, esp_dir: &Path) -> Result {
    let esp_sectors = (USB_PART_ESP.1 - USB_PART_ESP.0 + 1) as u32;
    let mut fat =
        aricode_fat::Fat32Builder::new(esp_sectors, 1).map_err(|e| format!("fat: {e:?}"))?;
    let mut files = Vec::new();
    walk_files(esp_dir, "", &mut files)?;
    for (rel, p) in &files {
        let data = io(fs::read(p), "leer fichero de la ESP")?;
        fat.add_file(rel, &data)
            .map_err(|e| format!("fat {rel}: {e:?}"))?;
    }
    let mut img = vec![0u8; USB_SECTORS as usize * 512];
    let guid = aricode_gpt::ARICODEOS_DATA_GUID;
    aricode_gpt::write_disk(
        &mut img,
        &[
            ("ESP", USB_PART_ESP.0, USB_PART_ESP.1, ESP_TYPE_GUID),
            ("ARI0", USB_PART_ARI0.0, USB_PART_ARI0.1, guid),
            ("SYSA", USB_PART_SYSA.0, USB_PART_SYSA.1, guid),
            ("SYSB", USB_PART_SYSB.0, USB_PART_SYSB.1, guid),
            ("UPDATE", USB_PART_UPDATE.0, USB_PART_UPDATE.1, guid),
            ("ARIBOOT", USB_PART_ARIBOOT.0, USB_PART_ARIBOOT.1, guid),
        ],
    )
    .map_err(|e| format!("gpt: {e:?}"))?;
    let o = USB_PART_ESP.0 as usize * 512;
    let fat_img = fat.finish();
    img[o..o + fat_img.len()].copy_from_slice(&fat_img);
    populate_arifs(&mut img, USB_PART_ARI0)?;
    // Area en bruto en la cola de ARI0, al mismo desplazamiento que en el
    // disco de pruebas (init se lo indica a blk_client).
    for lba in USB_PART_ARI0.0 + RAW_PATTERN_OFFSET..=USB_PART_ARI0.1 {
        let o = lba as usize * 512;
        img[o..o + 8].copy_from_slice(b"ARIDISK\0");
        img[o + 8..o + 16].copy_from_slice(&lba.to_le_bytes());
    }
    write_slot(&mut img, USB_PART_SYSA, &system_image(1, false)?)?;
    let state = aricode_ab::BootState::fresh(aricode_ab::Slot::A);
    let o = USB_PART_ARIBOOT.0 as usize * 512;
    img[o..o + 512].copy_from_slice(&state.encode());
    io(fs::write(path, &img), "escribir imagen USB")?;
    println!(
        "[xtask] imagen USB en {} ({} MiB, {} ficheros en la ESP)",
        path.display(),
        img.len() >> 20,
        files.len()
    );
    Ok(())
}

/// `cargo xtask usb-image [--cmdline S]`: imagen lista para `dd` en un pendrive.
pub fn build_usb_image(release: bool, cmdline: &str) -> Result<PathBuf> {
    let base = build_base_esp(release)?;
    let esp = build_scenario_esp(
        &base,
        &scenarios_root(),
        "usb-image",
        cmdline,
        &Default::default(),
    )?;
    let out = crate::build_dir().join("aricodeos-usb.img");
    write_usb_image(&out, &esp)?;
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskVariant {
    /// Slot A activo con la imagen actual; B vacío; UPDATE con la versión 2.
    #[default]
    Slots,
    /// Sin particiones de slots ni estado: AriBoot arranca desde la ESP.
    EspOnly,
    /// B instalado (versión 2) y pendiente con 3 intentos.
    PendingB,
    /// B pendiente pero sin intentos: rollback a A.
    ExhaustedB,
    /// B pendiente con una imagen cuya firma no vale: rechazo y rollback.
    CorruptB,
    /// Slots normales y un reemplazo de `README` comprometido en el journal
    /// de AriFS sin aplicar (corte tras el compromiso): fsd lo rehace.
    FsPendingReplace,
    /// Slots normales y un byte alterado en la tabla de AriFS: fsd no monta.
    FsCorrupt,
    /// El manifiesto del módulo WASM solo concede `print` (ADR 0019).
    WasmRestricted,
    /// `demo.wasm` sustituido por un módulo mal tipado: el validador lo rechaza.
    WasmInvalid,
    /// `demo.wasm` importa `aricode.file_write` con otra firma: el runner no
    /// lo enlaza (auditoría 6ee19fb, hallazgo 5).
    WasmBadImport,
    /// Imagen para pendrive: la ESP va dentro del disco (sin `fat:` en QEMU).
    UsbImage,
}

impl DiskVariant {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "slots" => Self::Slots,
            "esp-only" => Self::EspOnly,
            "pending-b" => Self::PendingB,
            "b-exhausted" => Self::ExhaustedB,
            "b-corrupt" => Self::CorruptB,
            "fs-pending-replace" => Self::FsPendingReplace,
            "fs-corrupt" => Self::FsCorrupt,
            "wasm-restricted" => Self::WasmRestricted,
            "wasm-invalid" => Self::WasmInvalid,
            "wasm-bad-import" => Self::WasmBadImport,
            "usb-image" => Self::UsbImage,
            _ => return None,
        })
    }
}

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
    fn flush(&mut self) -> std::result::Result<(), aricode_arifs::FsError> {
        Ok(())
    }
}

/// Primer sector del área en bruto del disco de pruebas: desde aquí hasta el
/// final de la partición cada sector lleva `ARIDISK\0` y su LBA, fuera del
/// alcance de AriFS (32 ficheros × 64 sectores caben antes).
pub const RAW_PATTERN_LBA: u64 = 4096;
/// Desplazamiento del area en bruto dentro de ARI0 (igual en todos los discos).
pub const RAW_PATTERN_OFFSET: u64 = RAW_PATTERN_LBA - PART_ARI0.0;

/// Ficheros de ejemplo del disco de pruebas.
pub const DISK_FILES: &[(&str, &[u8])] = &[
    ("hola.txt", b"Hola desde AriFS\n"),
    ("README", b"AriCodeOS: disco de pruebas con AriFS v1.\n"),
];

/// Imagen de sistema firmada con la clave de desarrollo: contenedor con
/// manifiesto (versión y hashes), firma, kernel e initrd de la ESP base.
/// Con `corrupt`, la firma se altera para que el cargador la rechace.
pub fn system_image(version: u32, corrupt: bool) -> Result<Vec<u8>> {
    let esp = crate::build_dir().join("esp");
    let kernel = stripped_elf(&esp.join(KERNEL))?;
    let initrd = io(fs::read(esp.join(INITRD)), "leer initrd.img de la ESP base")?;
    let manifest = aricode_ab::Manifest {
        version,
        kernel_sha256: aricode_crypto::sha256::sha256(&kernel),
        initrd_sha256: aricode_crypto::sha256::sha256(&initrd),
    };
    let mut buf = [0u8; 192];
    let text = manifest.write(&mut buf).as_bytes().to_vec();
    let seed: [u8; 32] = crate::keys::read_hex(
        &crate::root()
            .join(crate::keys::DEV_KEY_DIR)
            .join(crate::keys::SEED_FILE),
    )?;
    let mut sig = aricode_crypto::ed25519::sign(&seed, &text);
    if corrupt {
        sig[5] ^= 0x40;
    }
    aricode_initrd::build(&[
        ("manifest", text.as_slice()),
        ("manifest.sig", &sig[..]),
        ("kernel.elf", kernel.as_slice()),
        ("initrd.img", initrd.as_slice()),
    ])
    .map_err(|e| format!("imagen de sistema: {e:?}"))
}

/// ELF sin secciones de depuración (el kernel y el initrd solo necesitan los
/// segmentos `PT_LOAD`; con depuración el kernel supera los 5 MiB). Si no hay
/// `strip`, se usa el fichero tal cual.
pub fn stripped_elf(path: &Path) -> Result<Vec<u8>> {
    let dir = crate::build_dir().join("stripped");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(path.file_name().unwrap_or_default());
    let status = std::process::Command::new("strip")
        .args(["--strip-debug", "-o"])
        .arg(&out)
        .arg(path)
        .status();
    match status {
        Ok(st) if st.success() => io(fs::read(&out), "leer ELF sin depuracion"),
        _ => io(fs::read(path), &format!("leer {}", path.display())),
    }
}

/// Escribe cabecera + contenedor al principio de una partición.
fn write_slot(img: &mut [u8], part: (u64, u64), container: &[u8]) -> Result {
    let cap = (part.1 - part.0 + 1) as usize * 512 - 512;
    if container.len() > cap {
        return Err(format!(
            "la imagen de sistema ({} bytes) no cabe en el slot ({cap} bytes)",
            container.len()
        ));
    }
    let o = part.0 as usize * 512;
    img[o..o + 512].copy_from_slice(&aricode_ab::Image::header(container.len() as u32));
    img[o + 512..o + 512 + container.len()].copy_from_slice(container);
    Ok(())
}

pub fn write_test_disk(
    path: &Path,
    variant: DiskVariant,
    installed: &[(String, Vec<u8>)],
) -> Result {
    if variant == DiskVariant::UsbImage {
        return Err(String::from(
            "la imagen USB necesita la ESP: usar write_usb_image",
        ));
    }
    let mut img = vec![0u8; DISK_SECTORS as usize * 512];
    let guid = aricode_gpt::ARICODEOS_DATA_GUID;
    let mut parts = vec![("ARI0", PART_ARI0.0, PART_ARI0.1, guid)];
    if variant != DiskVariant::EspOnly {
        parts.push(("SYSA", PART_SYSA.0, PART_SYSA.1, guid));
        parts.push(("SYSB", PART_SYSB.0, PART_SYSB.1, guid));
        parts.push(("UPDATE", PART_UPDATE.0, PART_UPDATE.1, guid));
        parts.push(("ARIBOOT", PART_ARIBOOT.0, PART_ARIBOOT.1, guid));
    }
    aricode_gpt::write_disk(&mut img, &parts).map_err(|e| format!("gpt: {e:?}"))?;
    populate_arifs(&mut img, PART_ARI0)?;
    install_files(&mut img, PART_ARI0, installed)?;
    match variant {
        DiskVariant::FsPendingReplace => stage_arifs_replace(&mut img, PART_ARI0)?,
        DiskVariant::WasmRestricted => {
            overwrite_arifs(&mut img, PART_ARI0, WASM_MANIFEST_FILE, b"caps = print\n")?
        }
        DiskVariant::WasmInvalid => {
            overwrite_arifs(&mut img, PART_ARI0, WASM_MODULE_FILE, INVALID_WASM)?
        }
        DiskVariant::WasmBadImport => {
            overwrite_arifs(&mut img, PART_ARI0, WASM_MODULE_FILE, BAD_IMPORT_WASM)?
        }
        DiskVariant::FsCorrupt => {
            // Primer byte del nombre de la primera entrada en uso.
            img[(PART_ARI0.0 + aricode_arifs::TABLE_LBA) as usize * 512] ^= 0x20;
        }
        _ => {}
    }
    let (_, last) = PART_ARI0;
    for lba in RAW_PATTERN_LBA..=last {
        let o = lba as usize * 512;
        img[o..o + 8].copy_from_slice(b"ARIDISK\0");
        img[o + 8..o + 16].copy_from_slice(&lba.to_le_bytes());
    }
    if variant != DiskVariant::EspOnly {
        use aricode_ab::{schedule, BootState, Slot};
        let v1 = system_image(1, false)?;
        let v2 = system_image(2, false)?;
        write_slot(&mut img, PART_SYSA, &v1)?;
        write_slot(&mut img, PART_UPDATE, &v2)?;
        let mut state = BootState::fresh(Slot::A);
        match variant {
            DiskVariant::PendingB => {
                write_slot(&mut img, PART_SYSB, &v2)?;
                state = schedule(state, Slot::B);
            }
            DiskVariant::ExhaustedB => {
                write_slot(&mut img, PART_SYSB, &v2)?;
                state = schedule(state, Slot::B);
                state.pending = Some((Slot::B, 0));
            }
            DiskVariant::CorruptB => {
                write_slot(&mut img, PART_SYSB, &system_image(2, true)?)?;
                state = schedule(state, Slot::B);
            }
            DiskVariant::Slots
            | DiskVariant::EspOnly
            | DiskVariant::UsbImage
            | DiskVariant::FsPendingReplace
            | DiskVariant::FsCorrupt
            | DiskVariant::WasmRestricted
            | DiskVariant::WasmInvalid
            | DiskVariant::WasmBadImport => {}
        }
        let o = PART_ARIBOOT.0 as usize * 512;
        img[o..o + 512].copy_from_slice(&state.encode());
    }
    io(fs::write(path, img), "escribir disco de pruebas")
}

/// Añade a AriFS ficheros de cualquier tamaño (paquetes instalados, ADR
/// 0017): capacidad en múltiplos de 64 sectores.
fn install_files(img: &mut [u8], part: (u64, u64), files: &[(String, Vec<u8>)]) -> Result {
    if files.is_empty() {
        return Ok(());
    }
    let mut fs = aricode_arifs::AriFs::open(VecDisk {
        data: img,
        base: part.0,
    })
    .map_err(|e| format!("arifs: {e:?}"))?;
    let unit = aricode_arifs::FILE_CAPACITY_SECTORS as usize * 512;
    for (name, bytes) in files {
        let units = bytes.len().div_ceil(unit).max(1);
        let i = fs
            .create_sized(
                name,
                (units * aricode_arifs::FILE_CAPACITY_SECTORS as usize) as u32,
            )
            .map_err(|e| format!("arifs {name}: {e:?}"))?;
        fs.write(i, 0, bytes)
            .map_err(|e| format!("arifs {name} ({} bytes): {e:?}", bytes.len()))?;
    }
    fs.flush().map_err(|e| format!("arifs: {e:?}"))
}

/// Contenido que `filesystem.replace_recovery` espera leer en `README`.
pub const RECOVERED_README: &[u8] = b"AriCodeOS: recuperado tras un corte\n";

/// Deja en el journal de AriFS un reemplazo de `README` comprometido y sin
/// aplicar: el estado exacto que queda tras un corte justo después del
/// compromiso (ADR 0014). El siguiente montaje debe rehacerlo.
fn stage_arifs_replace(img: &mut [u8], part: (u64, u64)) -> Result {
    let mut fs = aricode_arifs::AriFs::open(VecDisk {
        data: img,
        base: part.0,
    })
    .map_err(|e| format!("arifs: {e:?}"))?;
    let i = fs
        .find("README")
        .ok_or_else(|| String::from("arifs: falta README"))?;
    fs.stage_replace(i, RECOVERED_README)
        .map(|_| ())
        .map_err(|e| format!("arifs README: {e:?}"))
}

/// Formatea AriFS en `part` con los ficheros de ejemplo y el módulo WASM.
fn populate_arifs(img: &mut [u8], part: (u64, u64)) -> Result {
    let (first, last) = part;
    let sectors = last - first + 1;
    let mut fs = aricode_arifs::AriFs::format(
        VecDisk {
            data: img,
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
    // Módulo WebAssembly de ejemplo y su manifiesto en el directorio privado
    // de `wasm_runner` (lo deja `build_wasm_programs`; sin él, el runner
    // informa y sale).
    let demo = crate::build_dir().join("demo.wasm");
    if let Ok(bytes) = fs::read(&demo) {
        for (name, content) in [
            (WASM_MODULE_FILE, bytes.as_slice()),
            (WASM_MANIFEST_FILE, WASM_MANIFEST),
        ] {
            let i = fs
                .create(name)
                .map_err(|e| format!("arifs {name}: {e:?}"))?;
            fs.write(i, 0, content)
                .map_err(|e| format!("arifs {name} ({} bytes): {e:?}", content.len()))?;
        }
    }
    Ok(())
}

const WASM_MODULE_FILE: &str = "wasm_runner/demo.wasm";
const WASM_MANIFEST_FILE: &str = "wasm_runner/demo.manifest";
/// Lo que el módulo de ejemplo pide (ADR 0019).
const WASM_MANIFEST: &[u8] = b"caps = print, clock, files\n";
/// Módulo mal tipado: `main() -> i32` cuyo cuerpo es `i32.const 1; i32.add`
/// (suma con un solo operando). Analiza bien y no valida.
const INVALID_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // cabecera
    0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7F, // type: () -> i32
    0x03, 0x02, 0x01, 0x00, // function: tipo 0
    0x05, 0x03, 0x01, 0x00, 0x01, // memory: 1 pagina
    0x07, 0x08, 0x01, 0x04, 0x6D, 0x61, 0x69, 0x6E, 0x00, 0x00, // export "main"
    0x0A, 0x07, 0x01, 0x05, 0x00, 0x41, 0x01, 0x6A, 0x0B, // code
];

/// Módulo válido que importa `aricode.file_write` como `() -> i32` y lo llama
/// desde `main(i64) -> i32` (fixture de la auditoría 6ee19fb): el runner debe
/// rechazar la firma antes de ejecutar.
const BAD_IMPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // cabecera
    0x01, 0x0A, 0x02, 0x60, 0x00, 0x01, 0x7F, 0x60, 0x01, 0x7E, 0x01, 0x7F, // tipos
    0x02, 0x16, 0x01, 0x07, 0x61, 0x72, 0x69, 0x63, 0x6F, 0x64, 0x65, 0x0A, 0x66, 0x69, 0x6C, 0x65,
    0x5F, 0x77, 0x72, 0x69, 0x74, 0x65, 0x00, 0x00, // import aricode.file_write: tipo 0
    0x03, 0x02, 0x01, 0x01, // function: tipo 1
    0x07, 0x08, 0x01, 0x04, 0x6D, 0x61, 0x69, 0x6E, 0x00, 0x01, // export "main" = func 1
    0x0A, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0B, // main: call 0
];

/// Sustituye entero un fichero de AriFS (reemplazo atómico de la librería).
fn overwrite_arifs(img: &mut [u8], part: (u64, u64), name: &str, bytes: &[u8]) -> Result {
    let mut fs = aricode_arifs::AriFs::open(VecDisk {
        data: img,
        base: part.0,
    })
    .map_err(|e| format!("arifs: {e:?}"))?;
    let i = fs
        .find(name)
        .ok_or_else(|| format!("arifs: falta {name}"))?;
    fs.replace(i, bytes)
        .map_err(|e| format!("arifs {name}: {e:?}"))
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
