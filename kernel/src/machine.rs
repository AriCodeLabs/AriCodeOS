// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Which machine this is, for `diag` (`sysinfo::MACHINE`): the make and
//! model from SMBIOS, never its serial, how it powers off (the FADT), and
//! the device identifiers the ACPI
//! tables declare, which name devices no bus enumerates (a touchpad on I2C,
//! its controller). Read only.

use crate::sync::IrqLock;
use alloc::string::String;
use aricode_smbios::Entry;
use boot_protocol::BootInfo;
use core::fmt::Write;

/// The SMBIOS entry point found at boot, and where.
static SMBIOS: IrqLock<Option<(Entry, &'static str)>> = IrqLock::new(None);

/// UEFI memory types an SMBIOS entry point may sit in (UEFI 2.x, 7.2;
/// DSP0134, 5.2.2): reserved, runtime services code and data, ACPI
/// reclaimable and NVS.
const FIRMWARE_TYPES: [u32; 5] = [0, 5, 6, 9, 10];
/// `EFI_MEMORY_WB`: only memory the firmware calls cacheable RAM is read,
/// never a device's registers.
const EFI_MEMORY_WB: u64 = 0x8;
/// Regions larger than this are not scanned.
const SCAN_MAX: u64 = 64 << 20;

/// Finds the SMBIOS entry point at boot, while the memory map is still
/// there: the legacy area first (a BIOS, or firmware that keeps one), then
/// the firmware's own memory (UEFI puts it there and names it only in its
/// configuration table, which the loader does not pass on).
pub fn init(info: &BootInfo) {
    let (base, len) = aricode_smbios::LEGACY_AREA;
    let mut found = crate::acpi::firmware_bytes(base, len)
        .and_then(aricode_smbios::find_entry)
        .map(|e| (e, "the legacy area"));
    if found.is_none() {
        // SAFETY: the map is in boot memory reachable through the physical
        // map, not reclaimed yet.
        for d in unsafe { info.memory_regions() } {
            if !FIRMWARE_TYPES.contains(&d.ty)
                || d.attribute & EFI_MEMORY_WB == 0
                || d.size_bytes() > SCAN_MAX
            {
                continue;
            }
            let entry = crate::acpi::firmware_bytes(d.phys_start, d.size_bytes() as usize)
                .and_then(aricode_smbios::find_entry);
            if let Some(e) = entry {
                found = Some((e, "firmware memory"));
                break;
            }
        }
    }
    *SMBIOS.lock() = found;
}

/// The report: a `machine` line, `power` lines (the FADT's power-off facts,
/// incident 0065) and `acpi` lines, as `diag` prints them.
pub fn report() -> String {
    let mut out = String::new();
    let entry = *SMBIOS.lock();
    let system = entry.and_then(|(e, at)| {
        let table = crate::acpi::firmware_bytes(e.table, e.len as usize)?;
        aricode_smbios::system(table).map(|s| (e, at, s))
    });
    match system {
        Some((e, at, s)) => {
            let _ = write!(out, "machine {} {}", s.maker, s.product);
            if !s.version.is_empty() {
                let _ = write!(out, ", {}", s.version);
            }
            if !s.family.is_empty() && s.family != s.version {
                let _ = write!(out, " (family {})", s.family);
            }
            let _ = writeln!(out, "; SMBIOS {}.{} in {at}", e.major, e.minor);
        }
        None => {
            let _ = writeln!(
                out,
                "machine unknown: no SMBIOS entry point found{}",
                if entry.is_some() {
                    " (its table unreadable)"
                } else {
                    ""
                }
            );
        }
    }
    // Before the device identifiers, which a short buffer may cut.
    crate::arch::x86_64::power::report(&mut out);
    crate::acpi::with(|a| {
        if a.ids.is_empty() {
            let _ = writeln!(out, "acpi    no device identifiers found");
            return;
        }
        // Each device with its identifiers, a few to a line.
        let mut line = String::new();
        let mut devices = 0;
        let mut last = [0u8; 4];
        for d in &a.ids {
            if d.device != last {
                if devices % 6 == 0 && devices > 0 {
                    let _ = writeln!(out, "acpi   {line}");
                    line.clear();
                }
                let _ = write!(line, " {}:", d.device_str());
                last = d.device;
                devices += 1;
            }
            let _ = write!(line, " {}", d.id_str());
        }
        if !line.is_empty() {
            let _ = writeln!(out, "acpi   {line}");
        }
    });
    out
}
