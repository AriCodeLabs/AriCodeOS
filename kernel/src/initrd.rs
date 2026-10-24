// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Initrd handed over by AriBoot (`BootInfo::initrd_*`), reachable through
//! the physical map. Contains `init` and the test programs.

use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use aricode_initrd::Initrd;
use boot_protocol::BootInfo;
use log::{info, warn};

/// The initrd's files by name, built once from the validated image: a
/// lookup no longer parses and validates the whole image again
/// (performance audit 731c398, finding F). The first entry of a name wins,
/// as `Initrd::find` decides.
static INDEX: IrqLock<Option<BTreeMap<&'static str, &'static [u8]>>> = IrqLock::new(None);
/// SHA-256 prefix of the initrd as the kernel finds it on entry (before
/// touching memory): if it differs from the one AriBoot reports, the image
/// was corrupted before the kernel; if it differs from the one computed in
/// `init`, afterwards.
static ENTRY_DIGEST: IrqLock<[u8; 4]> = IrqLock::new([0; 4]);

fn digest_prefix(bytes: &[u8]) -> [u8; 4] {
    let d = aricode_crypto::sha256::sha256(bytes);
    [d[0], d[1], d[2], d[3]]
}

fn bytes_of(info: &BootInfo) -> &'static [u8] {
    let virt = crate::memory::phys::phys_to_virt(info.initrd_address);
    // SAFETY: AriBoot copied the image to pages the kernel keeps
    // (`BootInfo::kernel_ranges`); the physical map is active.
    unsafe { core::slice::from_raw_parts(virt as *const u8, info.initrd_size as usize) }
}

/// Takes the initrd fingerprint in `kernel_entry`, before PMM, VMM or heap.
pub fn snapshot_digest(info: &BootInfo) {
    if info.initrd_size != 0 {
        *ENTRY_DIGEST.lock() = digest_prefix(bytes_of(info));
    }
}

pub fn init(info: &BootInfo) {
    if info.initrd_size == 0 {
        warn!("initrd: missing");
        return;
    }
    let bytes = bytes_of(info);
    let entry = *ENTRY_DIGEST.lock();
    let now = digest_prefix(bytes);
    match Initrd::parse(bytes) {
        Ok(rd) => {
            info!(
                "initrd: {} files, {} KiB @ {:#x}, sha256 {:02x}{:02x}{:02x}{:02x}",
                rd.len(),
                bytes.len() / 1024,
                info.initrd_address,
                now[0],
                now[1],
                now[2],
                now[3]
            );
            let mut index = BTreeMap::new();
            for entry in rd.entries().flatten() {
                index.entry(entry.name).or_insert(entry.data);
            }
            *INDEX.lock() = Some(index);
        }
        Err(e) => warn!(
            "initrd: invalid image ({e:?}), ignored; sha256 on entry {:02x}{:02x}{:02x}{:02x}, now {:02x}{:02x}{:02x}{:02x}",
            entry[0], entry[1], entry[2], entry[3], now[0], now[1], now[2], now[3]
        ),
    }
}

/// Contents of an initrd file by name.
pub fn find(name: &str) -> Option<&'static [u8]> {
    INDEX.lock().as_ref()?.get(name).copied()
}
