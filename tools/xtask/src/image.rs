// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Assembles the EFI partition (directory QEMU exposes as FAT).

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

/// Root of the scenarios' working directories.
pub fn scenarios_root() -> PathBuf {
    build_dir().join("scenarios")
}

/// Base ESP at `build/esp/`: BOOTX64.EFI + kernel.elf, no cmdline.
pub fn build_base_esp(release: bool) -> Result<PathBuf> {
    let a = build_os(release)?;
    // Every verifier must embed the key the images are signed with
    // (re-audit, R1).
    crate::keys::check_embedded_key(&a.ariboot)?;
    for verifier in ["init", "installer", "updater"] {
        crate::keys::check_embedded_key(&a.user_bin.join(verifier))?;
    }
    let esp = build_dir().join("esp");
    write_esp(&esp, &a, None)?;
    println!("[xtask] ESP image at {}", esp.display());
    Ok(esp)
}

/// Working directory of a scenario under `scenarios_root`, ready to be
/// removed and rewritten.
///
/// Besides the lexical validation of the name, real containment is checked:
/// neither the scenario directory nor its `esp` may be symlinks, and the
/// resulting canonical path must stay inside the canonical root. That way a
/// pre-existing link cannot redirect `remove_dir_all` outside the tree.
pub fn scenario_dir(scenarios_root: &Path, name: &str) -> Result<PathBuf> {
    crate::scenario::validate_name(name)?;
    io(fs::create_dir_all(scenarios_root), "create scenarios/")?;
    let root = io(scenarios_root.canonicalize(), "canonicalize scenarios/")?;
    let dir = root.join(name);
    reject_symlink(&dir)?;
    reject_symlink(&dir.join("esp"))?;
    io(fs::create_dir_all(&dir), "create scenario dir")?;
    let real = io(dir.canonicalize(), "canonicalize scenario dir")?;
    if !real.starts_with(&root) {
        return Err(format!("{} escapes {}", real.display(), root.display()));
    }
    Ok(real)
}

fn reject_symlink(path: &Path) -> Result {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} is a symlink; not used as a target",
            path.display()
        )),
        _ => Ok(()),
    }
}

/// ESP for a scenario: copy of the base + cmdline + fault injections.
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
        io(fs::write(esp.join(CMDLINE), cmdline), "write cmdline")?;
    }
    let kernel = esp.join(KERNEL);
    if inject.remove_kernel {
        io(fs::remove_file(&kernel), "remove kernel")?;
        return Ok(esp);
    }
    if let Some(n) = inject.truncate_kernel {
        let data = io(fs::read(&kernel), "read kernel")?;
        io(
            fs::write(&kernel, &data[..n.min(data.len())]),
            "truncate kernel",
        )?;
    }
    if let Some(entry) = inject.patch_entry {
        let mut data = io(fs::read(&kernel), "read kernel")?;
        elf_patch::set_entry(&mut data, entry)?;
        io(fs::write(&kernel, &data), "patch e_entry")?;
    }
    if inject.add_empty_segment_at_entry {
        let mut data = io(fs::read(&kernel), "read kernel")?;
        elf_patch::append_empty_rw_segment_at_entry(&mut data)?;
        io(fs::write(&kernel, &data), "add empty segment")?;
    }
    Ok(esp)
}

/// Minimal ELF64 manipulations to inject faults. Only what the scenarios
/// need; not a general parser.
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
            return Err("patch_entry: kernel too short".into());
        }
        data[E_ENTRY..E_ENTRY + 8].copy_from_slice(&entry.to_le_bytes());
        Ok(())
    }

    /// Adds an RW PT_LOAD with `memsz = 0`, `vaddr = e_entry` and alignment 1
    /// in the gap that follows the program header table.
    pub fn append_empty_rw_segment_at_entry(data: &mut [u8]) -> Result {
        if data.len() < 64 {
            return Err("add_empty_segment: kernel too short".into());
        }
        let phoff = u64_at(data, E_PHOFF) as usize;
        let phentsize = u16_at(data, E_PHENTSIZE) as usize;
        let phnum = u16_at(data, E_PHNUM) as usize;
        if phentsize != 56 {
            return Err(format!(
                "add_empty_segment: phentsize {phentsize} not supported"
            ));
        }
        let new_ph = phoff + phentsize * phnum;
        // The first byte of segment content marks the end of the available gap.
        let first_content = (0..phnum)
            .map(|i| u64_at(data, phoff + i * phentsize + 8) as usize)
            .filter(|&off| off > 0)
            .min()
            .unwrap_or(data.len());
        if new_ph + phentsize > first_content || new_ph + phentsize > data.len() {
            return Err("add_empty_segment: no gap after the program headers".into());
        }
        if data[new_ph..new_ph + phentsize].iter().any(|&b| b != 0) {
            return Err("add_empty_segment: the gap is not zeroed".into());
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

/// Packs `init` and the user programs into an initrd.
fn build_initrd(user_bin: &Path) -> Result<Vec<u8>> {
    let names = crate::build::initrd_programs()?;
    // Programs without debug info: the initrd goes from ~5 MiB to a few
    // hundred KiB and boot is faster.
    let mut contents: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|n| stripped_elf(&user_bin.join(n)).map(|d| (n.clone(), d)))
        .collect::<Result<_>>()?;
    for (name, path) in crate::build::initrd_manifests()? {
        contents.push((name.clone(), io(fs::read(&path), &format!("read {name}"))?));
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
            "create ESP",
        )?;
    }
    io(fs::copy(&a.ariboot, esp.join(BOOTX64)), "copy ariboot")?;
    io(fs::copy(&a.kernel, esp.join(KERNEL)), "copy kernel")?;
    io(
        fs::write(esp.join(INITRD), build_initrd(&a.user_bin)?),
        "write initrd",
    )?;
    if let Some(c) = cmdline {
        io(fs::write(esp.join(CMDLINE), c), "write cmdline")?;
    }
    Ok(())
}

/// virtio-blk test disk: 12 MiB with GPT and five partitions. It is
/// regenerated on every boot (the tests write to it).
///
/// ```text
/// ARI0     34..6143      AriFS with sample files (raw pattern from LBA 4096)
/// SYSA     6144..30719   signed system image, slot A (version 1), 12 MiB
/// SYSB     30720..55295  slot B (empty, or the variant the scenario asks for)
/// UPDATE   55296..79871  update image (version 2) that `updater` installs
/// ARIBOOT  79872..79935  A/B boot state (sector 0)
/// ```
///
/// The images' kernel carries no debug info (`strip`) if the tool exists;
/// with it the ELF exceeds 8 MiB.
pub const DISK_SECTORS: u64 = 98304;
pub const PART_ARI0: (u64, u64) = (34, 6143);
pub const PART_SYSA: (u64, u64) = (6144, 30719);
pub const PART_SYSB: (u64, u64) = (30720, 55295);
pub const PART_UPDATE: (u64, u64) = (55296, 79871);
pub const PART_ARIBOOT: (u64, u64) = (79872, 79935);

/// Initial disk state a scenario asks for (`disk = "..."`).
/// Image for a USB stick (Phase 34): GPT with the ESP (FAT32, 64 MiB) and the
/// same AriCodeOS partitions as the test disk, aligned to 1 MiB.
///
/// ```text
/// ESP      2048..133119     EFI/BOOT/BOOTX64.EFI, aricodeos/{kernel.elf,initrd.img,cmdline.txt}
/// ARI0     133120..139263   AriFS with only `USB_FILES` (a README)
/// SYSA     139264..163839   signed system image, slot A
/// SYSB     163840..188415   slot B (empty)
/// UPDATE   188416..212991   empty
/// ARIBOOT  212992..213055   boot state: A active
/// ```
pub const USB_SECTORS: u64 = 213_120;
pub const USB_PART_ESP: (u64, u64) = (2048, 133_119);
pub const USB_PART_ARI0: (u64, u64) = (133_120, 139_263);
pub const USB_PART_SYSA: (u64, u64) = (139_264, 163_839);
pub const USB_PART_SYSB: (u64, u64) = (163_840, 188_415);
pub const USB_PART_UPDATE: (u64, u64) = (188_416, 212_991);
pub const USB_PART_ARIBOOT: (u64, u64) = (212_992, 213_055);
/// EFI system partition type GUID, in on-disk byte order.
pub const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

/// Files of a directory (recursive) as relative paths with `/`.
fn walk_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result {
    let mut entries: Vec<_> = io(fs::read_dir(dir), "read dir")?
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

/// Writes the full USB image to `path` with the contents of `esp_dir` as
/// the EFI partition and slot A loaded with the system image.
/// `extra`: additional files for the ARI0 partition (e.g. `authorized_keys`).
/// `disk_guid`: the GPT disk GUID (random for real sticks, fixed for tests).
pub fn write_usb_image(
    path: &Path,
    esp_dir: &Path,
    extra: &[(String, Vec<u8>)],
    disk_guid: [u8; 16],
) -> Result {
    let esp_sectors = (USB_PART_ESP.1 - USB_PART_ESP.0 + 1) as u32;
    let mut fat =
        aricode_fat::Fat32Builder::new(esp_sectors, 1).map_err(|e| format!("fat: {e:?}"))?;
    let mut files = Vec::new();
    walk_files(esp_dir, "", &mut files)?;
    for (rel, p) in &files {
        let data = io(fs::read(p), "read ESP file")?;
        fat.add_file(rel, &data)
            .map_err(|e| format!("fat {rel}: {e:?}"))?;
    }
    let mut img = vec![0u8; USB_SECTORS as usize * 512];
    let guid = aricode_gpt::ARICODEOS_DATA_GUID;
    aricode_gpt::write_disk_with_guid(
        &mut img,
        disk_guid,
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
    format_arifs(&mut img, USB_PART_ARI0, USB_FILES)?;
    if !extra.is_empty() {
        let mut fs = aricode_arifs::AriFs::open(VecDisk {
            data: &mut img,
            base: USB_PART_ARI0.0,
        })
        .map_err(|e| format!("arifs: {e:?}"))?;
        for (name, content) in extra {
            let i = fs
                .create(name)
                .map_err(|e| format!("arifs {name}: {e:?}"))?;
            fs.write(i, 0, content)
                .map_err(|e| format!("arifs {name}: {e:?}"))?;
        }
    }
    // No test data (incident 0005): neither the `ARIDISK` raw-area pattern
    // nor the test disk's sample files. A `selftest` boot from the stick
    // stops at `fs_client`'s first check (no `hola.txt`), before any write.
    write_slot(&mut img, USB_PART_SYSA, &system_image(1, false)?)?;
    let state = aricode_ab::BootState::fresh(aricode_ab::Slot::A);
    let o = USB_PART_ARIBOOT.0 as usize * 512;
    img[o..o + 512].copy_from_slice(&state.encode());
    io(fs::write(path, &img), "write USB image")?;
    println!(
        "[xtask] USB image at {} ({} MiB, {} files in the ESP)",
        path.display(),
        img.len() >> 20,
        files.len()
    );
    Ok(())
}

/// `cargo xtask usb-image [--cmdline S]`: image ready to `dd` onto a USB stick.
/// `authorized_key`: an OpenSSH public key file installed in ARI0 as
/// `authorized_keys`, so the machine accepts SSH logins with that key.
pub fn build_usb_image(
    release: bool,
    cmdline: &str,
    authorized_key: Option<&Path>,
    update_source: Option<&str>,
) -> Result<PathBuf> {
    let line = aricode_cmdline::CmdLine::new(cmdline);
    if let Some(key) = line.first_of(aricode_cmdline::keys::TEST_ONLY) {
        return Err(format!(
            "--cmdline \"{cmdline}\": `{key}` is a test-only key (aricode_cmdline::keys::TEST_ONLY) and does not belong on a stick for a real machine"
        ));
    }
    if let Some(key) = line.first_of(aricode_cmdline::keys::LOADER_ONLY) {
        return Err(format!(
            "--cmdline \"{cmdline}\": `{key}` is added by AriBoot when it boots a slot (aricode_cmdline::keys::LOADER_ONLY); do not write it"
        ));
    }
    let base = build_base_esp(release)?;
    let esp = build_scenario_esp(
        &base,
        &scenarios_root(),
        "usb-image",
        cmdline,
        &Default::default(),
    )?;
    let mut extra = Vec::new();
    if let Some(key) = authorized_key {
        let text = io(fs::read(key), "read the public key")?;
        if !text.starts_with(b"ssh-ed25519 ") {
            return Err(format!(
                "{}: only ssh-ed25519 keys are accepted",
                key.display()
            ));
        }
        extra.push((String::from("authorized_keys"), text));
        println!("[xtask] authorized_keys from {}", key.display());
        // Host key made here with the host's randomness (a laptop without
        // rdrand has little entropy on its first boot); kept in build/ so
        // rebuilt images keep the fingerprint.
        let (seed, hex) = ssh_host_key(&crate::build_dir().join("ssh_host_ed25519_key"))?;
        extra.push((String::from("ssh_host_ed25519_key"), hex));
        println!(
            "[xtask] SSH host key {} (build/ssh_host_ed25519_key)",
            aricode_ssh::fingerprint(&aricode_crypto::ed25519::public_key(&seed))
        );
    }
    if let Some(input) = update_source {
        let mut sources = aricode_update_index::Sources::default();
        let url = sources
            .add(input)
            .map_err(|e| format!("--update-source {input}: {e:?}"))?
            .to_string();
        extra.push((
            aricode_update_index::main_path(aricode_update_index::SOURCES_FILE),
            sources.to_text().into_bytes(),
        ));
        println!("[xtask] update source {url}");
    }
    // Every stick gets its own disk GUID from OS entropy: the storage
    // drivers use it to tell AriCodeOS sticks apart (ADR 0031).
    let disk_guid = random_disk_guid()?;
    println!("[xtask] GPT disk GUID {}", hex_lower(&disk_guid));
    let out = crate::build_dir().join("aricodeos-usb.img");
    write_usb_image(&out, &esp, &extra, disk_guid)?;
    Ok(out)
}

/// A random GPT disk GUID (version 4 / RFC 4122 variant bits in GPT's
/// mixed-endian layout) from `/dev/urandom`.
fn random_disk_guid() -> Result<[u8; 16]> {
    use std::io::Read;
    let mut g = [0u8; 16];
    io(
        fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut g)),
        "read /dev/urandom",
    )?;
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    Ok(g)
}

fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskVariant {
    /// Slot A active with the current image; B empty; UPDATE with version 2.
    #[default]
    Slots,
    /// Like `Slots` but with an empty UPDATE partition: only a download can
    /// fill it (network update scenarios).
    UpdateEmpty,
    /// No slot or state partitions: AriBoot boots from the ESP.
    EspOnly,
    /// B installed (version 2) and pending with 3 attempts.
    PendingB,
    /// B pending but with no attempts left: rollback to A.
    ExhaustedB,
    /// B pending with an image whose signature is invalid: rejection and rollback.
    CorruptB,
    /// A active with an image whose signature is invalid: AriBoot boots
    /// from the ESP and the installed version is unknown (ADR 0029, H1).
    CorruptA,
    /// Like `PendingB` (B version 2 is confirmed on boot) but UPDATE holds
    /// version 1: `update apply` must refuse the downgrade.
    PendingBOlderUpdate,
    /// Normal slots and an AriFS table with only `FS_FREE_ENTRIES` free
    /// entries (filler files): what `update` and `upgrade` need must fit.
    FsNearlyFull,
    /// Like `FsNearlyFull` with one entry less: `upgrade` must refuse
    /// before installing anything.
    FsOneShort,
    /// Normal slots and a committed but unapplied `README` replacement in the
    /// AriFS journal (power cut after commit): fsd replays it.
    FsPendingReplace,
    /// Normal slots and a corrupted byte in the AriFS table: fsd does not mount.
    FsCorrupt,
    /// The WASM module's manifest grants only `print` (ADR 0019).
    WasmRestricted,
    /// `demo.wasm` replaced with an ill-typed module: the validator rejects it.
    WasmInvalid,
    /// `demo.wasm` imports `aricode.file_write` with another signature: the
    /// runner does not link it (audit 6ee19fb, finding 5).
    WasmBadImport,
    /// USB stick image: the ESP lives inside the disk (no `fat:` in QEMU).
    UsbImage,
}

impl DiskVariant {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "slots" => Self::Slots,
            "update-empty" => Self::UpdateEmpty,
            "esp-only" => Self::EspOnly,
            "pending-b" => Self::PendingB,
            "b-exhausted" => Self::ExhaustedB,
            "b-corrupt" => Self::CorruptB,
            "a-corrupt" => Self::CorruptA,
            "pending-b-older-update" => Self::PendingBOlderUpdate,
            "fs-nearly-full" => Self::FsNearlyFull,
            "fs-one-short" => Self::FsOneShort,
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

/// First sector of the test disk's raw area: from here to the end of the
/// partition every sector holds `ARIDISK\0` and its LBA, out of AriFS's
/// reach (32 files x 64 sectors fit before it).
pub const RAW_PATTERN_LBA: u64 = 4096;

/// The only file of a stick's ARI0 besides what `usb-image` is asked to
/// install (`authorized_keys`, host key, update source): the owner's disk
/// carries no test or sample data (incident 0005).
pub const USB_FILES: &[(&str, &[u8])] = &[(
    "README",
    b"AriCodeOS system stick: this AriFS partition holds your files.\nhttps://github.com/AriCodeLabs/AriCodeOS\n",
)];

/// Sample files of the test disk.
pub const DISK_FILES: &[(&str, &[u8])] = &[
    ("hola.txt", b"Hola desde AriFS\n"),
    ("README", b"AriCodeOS: disco de pruebas con AriFS v1.\n"),
];

/// System image signed with the development key: a container with the
/// manifest (version and hashes), signature, kernel and initrd of the base
/// ESP. With `corrupt`, the signature is altered so the loader rejects it.
pub fn system_image(version: u32, corrupt: bool) -> Result<Vec<u8>> {
    let esp = crate::build_dir().join("esp");
    let kernel = stripped_elf(&esp.join(KERNEL))?;
    let initrd = io(
        fs::read(esp.join(INITRD)),
        "read initrd.img from the base ESP",
    )?;
    let manifest = aricode_ab::Manifest {
        version,
        kernel_sha256: aricode_crypto::sha256::sha256(&kernel),
        initrd_sha256: aricode_crypto::sha256::sha256(&initrd),
    };
    let mut buf = [0u8; 192];
    let text = manifest.write(&mut buf).as_bytes().to_vec();
    let seed = crate::keys::signing_seed()?;
    let mut sig = aricode_crypto::ed25519::sign(&seed, &text);
    if corrupt {
        sig[5] ^= 0x40;
    }
    let container = aricode_initrd::build(&[
        ("manifest", text.as_slice()),
        ("manifest.sig", &sig[..]),
        ("kernel.elf", kernel.as_slice()),
        ("initrd.img", initrd.as_slice()),
    ])
    .map_err(|e| format!("system image: {e:?}"))?;
    // Post-build check (re-audit, R1): a valid image must verify with the
    // key the build embeds.
    if !corrupt
        && aricode_ab::Image::verify(&container, &crate::keys::expected_public_key()?).is_err()
    {
        return Err(String::from(
            "system image does not verify with the embedded public key",
        ));
    }
    Ok(container)
}

/// ELF without debug sections (the kernel and the initrd only need the
/// `PT_LOAD` segments; with debug info the kernel exceeds 5 MiB). Without
/// `strip`, the file is used as is.
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
        Ok(st) if st.success() => io(fs::read(&out), "read stripped ELF"),
        _ => io(fs::read(path), &format!("read {}", path.display())),
    }
}

/// Writes header + container at the start of a partition.
fn write_slot(img: &mut [u8], part: (u64, u64), container: &[u8]) -> Result {
    let cap = (part.1 - part.0 + 1) as usize * 512 - 512;
    if container.len() > cap {
        return Err(format!(
            "the system image ({} bytes) does not fit in the slot ({cap} bytes)",
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
            "the USB image needs the ESP: use write_usb_image",
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
    populate_test_arifs(&mut img, PART_ARI0)?;
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
        DiskVariant::FsNearlyFull => fill_arifs(&mut img, PART_ARI0, FS_FREE_ENTRIES)?,
        DiskVariant::FsOneShort => fill_arifs(&mut img, PART_ARI0, FS_FREE_ENTRIES - 1)?,
        DiskVariant::FsCorrupt => {
            // First byte of the name of the first entry in use.
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
        if variant == DiskVariant::CorruptA {
            write_slot(&mut img, PART_SYSA, &system_image(1, true)?)?;
        } else {
            write_slot(&mut img, PART_SYSA, &v1)?;
        }
        match variant {
            DiskVariant::UpdateEmpty => {}
            DiskVariant::PendingBOlderUpdate => write_slot(&mut img, PART_UPDATE, &v1)?,
            _ => write_slot(&mut img, PART_UPDATE, &v2)?,
        }
        let mut state = BootState::fresh(Slot::A);
        match variant {
            DiskVariant::PendingB | DiskVariant::PendingBOlderUpdate => {
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
            | DiskVariant::CorruptA
            | DiskVariant::FsNearlyFull
            | DiskVariant::FsOneShort
            | DiskVariant::UpdateEmpty
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
    io(fs::write(path, img), "write test disk")
}

/// Adds files of any size to AriFS (installed packages, ADR 0017):
/// capacity in multiples of 64 sectors.
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

/// Free AriFS entries left for the updater by `fs-nearly-full`: exactly
/// what a first `update` and an `upgrade` of the system and one installed
/// app create in the updater's directory (`state`, `indexes`, `last.log`,
/// `history.log`, `pkg-1.aripkg`; re-audit R3, final re-audit L-c). The
/// QEMU run of 2026-10-17 confirmed these five files in `ls`.
pub const FS_FREE_ENTRIES: usize = 5;

/// Fills the AriFS table with empty `fill-NN` files until only `free`
/// entries are left. Scenarios on these disks boot without `selftest`:
/// its `fs_client` would take one more entry for `nuevo.txt` (incident
/// 0005), which is what made the first QEMU run of `update.fs_nearly_full`
/// fail.
fn fill_arifs(img: &mut [u8], part: (u64, u64), free: usize) -> Result {
    let mut fs = aricode_arifs::AriFs::open(VecDisk {
        data: img,
        base: part.0,
    })
    .map_err(|e| format!("arifs: {e:?}"))?;
    let used = fs.entries().count();
    for i in 0..aricode_arifs::MAX_FILES.saturating_sub(used + free) {
        let name = format!("fill-{i:02}");
        fs.create(&name)
            .map_err(|e| format!("arifs {name}: {e:?}"))?;
    }
    fs.flush().map_err(|e| format!("arifs: {e:?}"))
}

/// Contents `filesystem.replace_recovery` expects to read in `README`.
pub const RECOVERED_README: &[u8] = b"AriCodeOS: recuperado tras un corte\n";

/// Leaves a committed but unapplied `README` replacement in the AriFS
/// journal: the exact state left by a power cut right after the commit
/// (ADR 0014). The next mount must replay it.
fn stage_arifs_replace(img: &mut [u8], part: (u64, u64)) -> Result {
    let mut fs = aricode_arifs::AriFs::open(VecDisk {
        data: img,
        base: part.0,
    })
    .map_err(|e| format!("arifs: {e:?}"))?;
    let i = fs
        .find("README")
        .ok_or_else(|| String::from("arifs: README missing"))?;
    fs.stage_replace(i, RECOVERED_README)
        .map(|_| ())
        .map_err(|e| format!("arifs README: {e:?}"))
}

/// Formats AriFS in `part` with `files`.
fn format_arifs(img: &mut [u8], part: (u64, u64), files: &[(&str, &[u8])]) -> Result {
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
    for (name, content) in files {
        let i = fs
            .create(name)
            .map_err(|e| format!("arifs {name}: {e:?}"))?;
        fs.write(i, 0, content)
            .map_err(|e| format!("arifs {name} ({} bytes): {e:?}", content.len()))?;
    }
    Ok(())
}

/// Test disk: AriFS with the sample files and the sample WebAssembly module
/// and its manifest in `wasm_runner`'s private directory
/// (`build_wasm_programs` produces it; without it, the runner reports and
/// exits).
fn populate_test_arifs(img: &mut [u8], part: (u64, u64)) -> Result {
    let demo = fs::read(crate::build_dir().join("demo.wasm")).ok();
    let mut files = DISK_FILES.to_vec();
    if let Some(bytes) = &demo {
        files.push((WASM_MODULE_FILE, bytes));
        files.push((WASM_MANIFEST_FILE, WASM_MANIFEST));
    }
    format_arifs(img, part, &files)
}

const WASM_MODULE_FILE: &str = "wasm_runner/demo.wasm";
const WASM_MANIFEST_FILE: &str = "wasm_runner/demo.manifest";
/// What the sample module asks for (ADR 0019).
const WASM_MANIFEST: &[u8] = b"caps = print, clock, files\n";
/// Ill-typed module: `main() -> i32` whose body is `i32.const 1; i32.add`
/// (an add with a single operand). It parses but does not validate.
const INVALID_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7F, // type: () -> i32
    0x03, 0x02, 0x01, 0x00, // function: type 0
    0x05, 0x03, 0x01, 0x00, 0x01, // memory: 1 page
    0x07, 0x08, 0x01, 0x04, 0x6D, 0x61, 0x69, 0x6E, 0x00, 0x00, // export "main"
    0x0A, 0x07, 0x01, 0x05, 0x00, 0x41, 0x01, 0x6A, 0x0B, // code
];

/// Valid module that imports `aricode.file_write` as `() -> i32` and calls it
/// from `main(i64) -> i32` (audit 6ee19fb fixture): the runner must reject
/// the signature before running.
const BAD_IMPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x0A, 0x02, 0x60, 0x00, 0x01, 0x7F, 0x60, 0x01, 0x7E, 0x01, 0x7F, // types
    0x02, 0x16, 0x01, 0x07, 0x61, 0x72, 0x69, 0x63, 0x6F, 0x64, 0x65, 0x0A, 0x66, 0x69, 0x6C, 0x65,
    0x5F, 0x77, 0x72, 0x69, 0x74, 0x65, 0x00, 0x00, // import aricode.file_write: type 0
    0x03, 0x02, 0x01, 0x01, // function: type 1
    0x07, 0x08, 0x01, 0x04, 0x6D, 0x61, 0x69, 0x6E, 0x00, 0x01, // export "main" = func 1
    0x0A, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0B, // main: call 0
];

/// Replaces an AriFS file entirely (the library's atomic replace).
fn overwrite_arifs(img: &mut [u8], part: (u64, u64), name: &str, bytes: &[u8]) -> Result {
    let mut fs = aricode_arifs::AriFs::open(VecDisk {
        data: img,
        base: part.0,
    })
    .map_err(|e| format!("arifs: {e:?}"))?;
    let i = fs
        .find(name)
        .ok_or_else(|| format!("arifs: {name} missing"))?;
    fs.replace(i, bytes)
        .map_err(|e| format!("arifs {name}: {e:?}"))
}

/// Private copy of the OVMF variables: the firmware writes them at boot.
pub fn ovmf_vars_for(dir: &Path) -> Result<PathBuf> {
    io(fs::create_dir_all(dir), "create dir")?;
    let vars = dir.join("OVMF_VARS.fd");
    io(fs::copy(OVMF_VARS, &vars), "copy OVMF_VARS")?;
    Ok(vars)
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    io(fs::create_dir_all(to), "create dir")?;
    for entry in io(fs::read_dir(from), "read dir")? {
        let entry = io(entry, "entry")?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            io(fs::copy(entry.path(), target), "copy")?;
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
    fn usb_image_refuses_test_only_keys() {
        // Incident 0005 audit: checked before anything is built.
        for cmdline in [
            "shell selftest",
            "qemu_exit shell",
            "shell usb_fault=idle",
            "test=panic",
        ] {
            let err = build_usb_image(false, cmdline, None, None).unwrap_err();
            assert!(err.contains("test-only key"), "{cmdline}: {err}");
        }
        for cmdline in ["shell boot_slot=A", "shell boot_pending"] {
            let err = build_usb_image(false, cmdline, None, None).unwrap_err();
            assert!(err.contains("added by AriBoot"), "{cmdline}: {err}");
        }
    }

    #[test]
    fn stick_arifs_holds_only_the_readme() {
        let sectors = USB_PART_ARI0.1 - USB_PART_ARI0.0 + 1;
        let mut img = vec![0u8; sectors as usize * 512];
        format_arifs(&mut img, (0, sectors - 1), USB_FILES).unwrap();
        let fs = aricode_arifs::AriFs::open(VecDisk {
            data: &mut img,
            base: 0,
        })
        .unwrap();
        let names: Vec<&str> = fs.entries().map(|(_, e)| e.name_str()).collect();
        assert_eq!(names, ["README"]);
        let readme = USB_FILES[0].1;
        assert!(
            readme.is_ascii() && readme.ends_with(b"https://github.com/AriCodeLabs/AriCodeOS\n")
        );
    }

    #[test]
    fn symlinked_scenario_dir_is_rejected_and_outside_data_survives() {
        // Audit review, finding 2.
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
        assert!(err.contains("symlink"), "{err}");
        assert!(
            outside.join("esp/marker").exists(),
            "the outside sentinel must survive"
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
        // Minimal ELF: header + 1 phdr + zeroed gap + content at 0x100.
        let mut d = vec![0u8; 0x110];
        d[E_PHOFF_TEST..E_PHOFF_TEST + 8].copy_from_slice(&64u64.to_le_bytes());
        d[54..56].copy_from_slice(&56u16.to_le_bytes());
        d[56..58].copy_from_slice(&1u16.to_le_bytes());
        d[24..32].copy_from_slice(&0x1234u64.to_le_bytes());
        d[64..68].copy_from_slice(&1u32.to_le_bytes());
        d[72..80].copy_from_slice(&0x100u64.to_le_bytes()); // content offset
        elf_patch::append_empty_rw_segment_at_entry(&mut d).unwrap();
        assert_eq!(u16::from_le_bytes([d[56], d[57]]), 2);
        let ph = &d[64 + 56..64 + 112];
        assert_eq!(u32::from_le_bytes(ph[0..4].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(ph[16..24].try_into().unwrap()), 0x1234);
        assert_eq!(u64::from_le_bytes(ph[40..48].try_into().unwrap()), 0);
        // No gap: the content starts right after the table.
        let mut d2 = d.clone();
        d2[72..80].copy_from_slice(&(64u64 + 112).to_le_bytes());
        assert!(elf_patch::append_empty_rw_segment_at_entry(&mut d2).is_err());
    }

    const E_PHOFF_TEST: usize = 32;

    #[test]
    fn legacy_manifest_never_goes_below_the_published_version() {
        // Re-audit, R4.
        let tmp = TempDir::new("legacy-manifest");
        let dir = tmp.path().join("source");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.txt"), "version = 3\nimage = a.img\n").unwrap();
        let older = Channel {
            system: Some(2),
            legacy_manifest: true,
            ..Channel::default()
        };
        let err = check_legacy_manifest(&dir, &older).unwrap_err();
        assert!(err.contains("below the published 3"), "{err}");
        let without = Channel {
            legacy_manifest: false,
            ..older
        };
        assert!(check_legacy_manifest(&dir, &without).is_ok());
        let newer = Channel {
            system: Some(4),
            legacy_manifest: true,
            ..Channel::default()
        };
        assert!(check_legacy_manifest(&dir, &newer).is_ok());
    }

    fn dev_public_key() -> [u8; 32] {
        crate::keys::read_hex(
            &crate::root()
                .join(crate::keys::DEV_KEY_DIR)
                .join(crate::keys::PUB_FILE),
        )
        .unwrap()
    }

    #[test]
    fn published_index_verifies_with_the_dev_key_and_tampering_is_caught() {
        // ADR 0029: what publish-update writes is what the updater accepts,
        // and a tampered publication is what it refuses.
        let tmp = TempDir::new("publish-index");
        let dir = tmp.path().join("source");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stale.img"), b"left from an older publication").unwrap();
        publish_update_to(&dir, true, &Channel::default()).unwrap();
        assert!(
            !dir.join("stale.img").exists(),
            "one publication per directory"
        );
        let index = fs::read(dir.join(aricode_update_index::INDEX_FILE)).unwrap();
        let sig = fs::read(dir.join(aricode_update_index::SIGNATURE_FILE)).unwrap();
        let parsed = aricode_update_index::verify(&index, &sig, &dev_public_key()).unwrap();
        assert!(parsed.system.is_none() && parsed.apps.is_empty());
        // Republishing never lowers the serial, whatever the clock says
        // (audit 1, L3), and leaves no staging directories behind (L2).
        let first = parsed.serial;
        publish_update_to(&dir, true, &Channel::default()).unwrap();
        let again = fs::read_to_string(dir.join(aricode_update_index::INDEX_FILE)).unwrap();
        assert!(aricode_update_index::Index::parse(&again).unwrap().serial > first);
        assert!(!tmp.path().join("source.new").exists() && !tmp.path().join("source.old").exists());
        assert!(
            parsed.serial > 1_700_000_000,
            "serial is the publication time"
        );

        let tampered = Channel {
            tamper_index: true,
            ..Channel::default()
        };
        publish_update_to(&dir, true, &tampered).unwrap();
        let index = fs::read(dir.join(aricode_update_index::INDEX_FILE)).unwrap();
        let sig = fs::read(dir.join(aricode_update_index::SIGNATURE_FILE)).unwrap();
        assert_eq!(
            aricode_update_index::verify(&index, &sig, &dev_public_key()),
            Err(aricode_update_index::IndexError::BadSignature)
        );
    }
}

/// The SSH host key seed at `path` (hex, one line), created from
/// `/dev/urandom` if missing. Returns the seed and the file's bytes.
fn ssh_host_key(path: &Path) -> Result<([u8; 32], Vec<u8>)> {
    let mut seed = [0u8; 32];
    if let Ok(text) = fs::read_to_string(path) {
        if aricode_crypto::unhex(text.trim(), &mut seed).is_some() {
            return Ok((seed, text.into_bytes()));
        }
        return Err(format!("{}: not a 64-digit hex seed", path.display()));
    }
    use std::io::Read;
    io(
        fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed)),
        "read /dev/urandom",
    )?;
    let mut hex = [0u8; 64];
    let text = format!("{}\n", aricode_crypto::hex(&seed, &mut hex));
    io(fs::write(path, &text), "write the SSH host key")?;
    Ok((seed, text.into_bytes()))
}

/// Channel directory of the scenarios (`host_update`): separate from
/// `update_dir()` so a test run can never replace what a real machine
/// downloads (incident 0003).
pub fn test_update_dir() -> PathBuf {
    crate::build_dir().join("update-test")
}

/// Channel directory that `serve-update` publishes for real machines.
pub fn update_dir() -> PathBuf {
    crate::build_dir().join("update")
}

/// What a published source offers (ADR 0029).
#[derive(Debug, Default)]
pub struct Channel {
    /// System image version; `None` publishes no system image.
    pub system: Option<u32>,
    /// App packages: (directory of the external app, version).
    pub apps: Vec<(String, u32)>,
    /// Also write the ADR 0027 `manifest.txt` next to the image, so machines
    /// still running the single-channel updater can fetch this build (the
    /// transition release, ADR 0029 decision 9).
    pub legacy_manifest: bool,
    /// Alter the index after signing it (tests: the updater must refuse it).
    pub tamper_index: bool,
    /// Alter the image after the index recorded its SHA-256 (tests: the
    /// updater must refuse the download).
    pub tamper_image: bool,
}

/// Index serial (audit 1, L3): the Unix time, but always above the serial
/// of the publication being replaced, so a slow or wrong clock can never
/// publish an index the machines refuse as stale. A serial pushed far into
/// the future (a clock error) is recovered on each machine with `update
/// remove N`, `update` (which forgets that source's stored copy) and
/// `update add URL` (ADR 0029, decision 6).
fn index_serial(previous: Option<u64>) -> Result<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| format!("system clock before 1970: {e}"))?;
    Ok(previous.map_or(now, |p| now.max(p + 1)))
}

/// The serial of the index currently published in `dir`, if any.
fn published_serial(dir: &Path) -> Option<u64> {
    let text = fs::read_to_string(dir.join(aricode_update_index::INDEX_FILE)).ok()?;
    aricode_update_index::Index::parse(&text)
        .ok()
        .map(|i| i.serial)
}

/// The system version currently published in `dir`: from its index, or
/// from an ADR 0027 `manifest.txt`.
fn published_system_version(dir: &Path) -> Option<u32> {
    if let Ok(text) = fs::read_to_string(dir.join(aricode_update_index::INDEX_FILE)) {
        if let Ok(index) = aricode_update_index::Index::parse(&text) {
            return index.system.map(|s| s.version);
        }
    }
    fs::read_to_string(dir.join("manifest.txt"))
        .ok()?
        .lines()
        .find_map(|l| l.trim().strip_prefix("version = ")?.trim().parse().ok())
}

/// Re-audit, R4: an ADR 0027 updater installs any version that differs
/// from the one it runs, older ones included. `manifest.txt` is therefore
/// never published for a version below the one already published.
fn check_legacy_manifest(dir: &Path, channel: &Channel) -> Result {
    let (true, Some(version)) = (channel.legacy_manifest, channel.system) else {
        return Ok(());
    };
    match published_system_version(dir) {
        Some(published) if version < published => Err(format!(
            "refusing to publish manifest.txt for system {version} below the published {published}: \
             ADR 0027 updaters would downgrade to it (use --no-legacy-manifest)"
        )),
        _ => Ok(()),
    }
}

/// `index.sig` for `text`, signed with the selected key (hex line).
fn sign_index(text: &str) -> Result<String> {
    let seed = crate::keys::signing_seed()?;
    let signature = aricode_crypto::ed25519::sign(&seed, text.as_bytes());
    let mut hex = [0u8; 128];
    Ok(format!("{}\n", aricode_crypto::hex(&signature, &mut hex)))
}

/// A stored copy (slot 1 of `updater/indexes`) of the index published in `dir`, for
/// source `url`, re-signed with a serial ahead of the published one: the
/// machine then sees the published index as stale (scenario
/// `update.stale_index`).
pub fn stored_copy_ahead(dir: &Path, url: &str) -> Result<Vec<u8>> {
    let text = io(
        fs::read_to_string(dir.join(aricode_update_index::INDEX_FILE)),
        "read the published index",
    )?;
    let mut index =
        aricode_update_index::Index::parse(&text).map_err(|e| format!("published index: {e:?}"))?;
    index.serial += 1000;
    let ahead = index.write();
    let sig = sign_index(&ahead)?;
    Ok(aricode_update_index::stored::encode(
        url,
        sig.as_bytes(),
        ahead.as_bytes(),
    ))
}

/// `cargo xtask publish-update` (ADR 0027, ADR 0029): publishes into `dir`
/// one complete update source: the signed system image `aricodeos-N.img`
/// (slot layout: ARISLOT header + container), the app packages
/// `NAME-V.aripkg`, the `index` listing them (version, file, size, SHA-256),
/// `index.sig` (its Ed25519 signature with the selected key) and, for the
/// transition release, the ADR 0027 `manifest.txt`. Everything is written
/// into a sibling directory first and moved into place at the end (audit
/// 1, L2), so a machine fetching meanwhile sees the old publication or the
/// new one, never a mix of files.
pub fn publish_update_to(dir: &Path, release: bool, channel: &Channel) -> Result<PathBuf> {
    use aricode_update_index::{AppEntry, Index, SystemEntry};
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{}: not a directory name", dir.display()))?;
    let staging = dir.with_file_name(format!("{name}.new"));
    let old = dir.with_file_name(format!("{name}.old"));
    for d in [dir, &staging, &old] {
        reject_symlink(d)?;
    }
    for d in [&staging, &old] {
        if d.exists() {
            io(fs::remove_dir_all(d), "remove a leftover publication")?;
        }
    }
    check_legacy_manifest(dir, channel)?;
    io(fs::create_dir_all(&staging), "create the update directory")?;
    let sha = aricode_crypto::sha256::sha256;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if let Some(version) = channel.system {
        build_base_esp(release)?;
        let container = system_image(version, false)?;
        let mut image = aricode_ab::Image::header(container.len() as u32).to_vec();
        image.extend_from_slice(&container);
        files.push((format!("aricodeos-{version}.img"), image));
    }
    let mut apps = Vec::new();
    for (app_dir, version) in &channel.apps {
        let app = crate::build::build_external(Path::new(app_dir), *version, release, false)?;
        let file = format!("{}-{}{}", app.name, app.version, aricode_pkg::EXTENSION);
        apps.push((app.name, app.version, file.clone()));
        files.push((file, app.package));
    }
    let data = |name: &str| -> &[u8] {
        &files
            .iter()
            .find(|(n, _)| n == name)
            .expect("published file")
            .1
    };
    let size = |name: &str| -> Result<u32> {
        u32::try_from(data(name).len()).map_err(|_| format!("{name}: too large"))
    };
    let system_file = channel.system.map(|v| format!("aricodeos-{v}.img"));
    let index = Index {
        serial: index_serial(published_serial(dir))?,
        system: match (channel.system, &system_file) {
            (Some(version), Some(file)) => Some(SystemEntry {
                version,
                file,
                size: size(file)?,
                sha256: sha(data(file)),
            }),
            _ => None,
        },
        apps: apps
            .iter()
            .map(|(name, version, file)| {
                Ok(AppEntry {
                    name,
                    version: *version,
                    file,
                    size: size(file)?,
                    sha256: sha(data(file)),
                })
            })
            .collect::<Result<_>>()?,
    };
    let mut text = index.write();
    // The index must parse back exactly as published (names, sizes, the
    // 20-byte app name limit).
    if Index::parse(&text).as_ref() != Ok(&index) {
        return Err(String::from(
            "the index does not parse back (an app name over 20 bytes?)",
        ));
    }
    let signature = sign_index(&text)?;
    let legacy = match (&index.system, channel.legacy_manifest) {
        (Some(s), true) => {
            let mut hex = [0u8; 64];
            Some(format!(
                "version = {}\nimage = {}\nsize = {}\nsha256 = {}\n",
                s.version,
                s.file,
                s.size,
                aricode_crypto::hex(&s.sha256, &mut hex)
            ))
        }
        _ => None,
    };
    let serial = index.serial;
    drop(index);
    if channel.tamper_index {
        let line = format!("serial = {serial}\n");
        text = text.replace(&line, &format!("serial = {}\n", serial + 1));
    }
    if channel.tamper_image {
        let Some(file) = &system_file else {
            return Err(String::from("tamper_image needs a system image"));
        };
        let image = &mut files
            .iter_mut()
            .find(|(n, _)| n == file)
            .expect("published image")
            .1;
        let last = image.len() - 1;
        image[last] ^= 0x01;
    }
    for (name, data) in &files {
        io(
            fs::write(staging.join(name), data),
            &format!("write {name}"),
        )?;
    }
    io(
        fs::write(staging.join(aricode_update_index::INDEX_FILE), &text),
        "write index",
    )?;
    io(
        fs::write(
            staging.join(aricode_update_index::SIGNATURE_FILE),
            signature,
        ),
        "write index.sig",
    )?;
    if let Some(manifest) = legacy {
        io(
            fs::write(staging.join("manifest.txt"), manifest),
            "write manifest.txt",
        )?;
    }
    if dir.exists() {
        io(fs::rename(dir, &old), "move the previous publication aside")?;
    }
    io(fs::rename(&staging, dir), "move the publication into place")?;
    if old.exists() {
        io(fs::remove_dir_all(&old), "remove the previous publication")?;
    }
    println!(
        "[xtask] update source at {}: serial {serial}, {}, {} app(s){}{}{}",
        dir.display(),
        channel
            .system
            .map_or(String::from("no system image"), |v| format!("system {v}")),
        apps.len(),
        if channel.legacy_manifest && channel.system.is_some() {
            ", plus manifest.txt for ADR 0027 updaters"
        } else {
            ""
        },
        if channel.tamper_index {
            " (index TAMPERED after signing, for tests)"
        } else {
            ""
        },
        if channel.tamper_image {
            " (image TAMPERED after indexing, for tests)"
        } else {
            ""
        }
    );
    Ok(dir.to_path_buf())
}
