// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! I/O APIC: routes global system interrupts (GSI) to LAPIC vectors.

use crate::memory::vmm;
use crate::sync::IrqLock;
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
use log::{info, warn};

/// Virtual window of the first I/O APIC (right after the LAPIC).
const IOAPIC_VIRT: u64 = 0xFFFF_D000_0000_1000;
const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;

const REG_ID: u32 = 0x00;
const REG_VER: u32 = 0x01;
const RED_TBL: u32 = 0x10;

const MASKED: u64 = 1 << 16;
const LEVEL: u64 = 1 << 15;
const ACTIVE_LOW: u64 = 1 << 13;

struct IoApic {
    gsi_base: u32,
    entries: u32,
}

static IOAPIC: IrqLock<Option<IoApic>> = IrqLock::new(None);

fn read(reg: u32) -> u32 {
    // SAFETY: I/O APIC MMIO registers mapped in `init`; access serialized by
    // the caller's lock.
    unsafe {
        ((IOAPIC_VIRT + IOREGSEL) as *mut u32).write_volatile(reg);
        ((IOAPIC_VIRT + IOWIN) as *const u32).read_volatile()
    }
}

fn write(reg: u32, value: u32) {
    // SAFETY: same as `read`.
    unsafe {
        ((IOAPIC_VIRT + IOREGSEL) as *mut u32).write_volatile(reg);
        ((IOAPIC_VIRT + IOWIN) as *mut u32).write_volatile(value);
    }
}

/// Maps the first I/O APIC from the MADT and masks all its entries.
pub fn init() {
    let Some(ioapic) = crate::acpi::with(|a| a.madt.io_apics.first().copied()) else {
        warn!("ioapic: none in the MADT");
        return;
    };
    vmm::with(|m| {
        m.map_4k(
            IOAPIC_VIRT,
            ioapic.address as u64,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .expect("ioapic: map");
    let mut guard = IOAPIC.lock();
    let entries = ((read(REG_VER) >> 16) & 0xFF) + 1;
    for i in 0..entries {
        write(RED_TBL + 2 * i, MASKED as u32);
        write(RED_TBL + 2 * i + 1, 0);
    }
    info!(
        "ioapic: id {} @ {:#x}, {entries} entries, gsi base {}, all masked",
        read(REG_ID) >> 24,
        ioapic.address,
        ioapic.gsi_base
    );
    *guard = Some(IoApic {
        gsi_base: ioapic.gsi_base,
        entries,
    });
}

fn entry_index(gsi: u32) -> Option<u32> {
    let guard = IOAPIC.lock();
    let io = guard.as_ref()?;
    let idx = gsi.checked_sub(io.gsi_base)?;
    (idx < io.entries).then_some(idx)
}

/// Routes `gsi` to `vector` on LAPIC `lapic_id`, unmasked.
pub fn route(
    gsi: u32,
    vector: u8,
    lapic_id: u8,
    level: bool,
    active_low: bool,
) -> Result<(), &'static str> {
    let idx = entry_index(gsi).ok_or("ioapic: gsi out of range")?;
    let mut low = vector as u64;
    if level {
        low |= LEVEL;
    }
    if active_low {
        low |= ACTIVE_LOW;
    }
    let _guard = IOAPIC.lock();
    write(RED_TBL + 2 * idx + 1, (lapic_id as u32) << 24);
    write(RED_TBL + 2 * idx, low as u32);
    Ok(())
}

pub fn set_masked(gsi: u32, masked: bool) {
    let Some(idx) = entry_index(gsi) else { return };
    let _guard = IOAPIC.lock();
    let low = read(RED_TBL + 2 * idx) as u64;
    let low = if masked { low | MASKED } else { low & !MASKED };
    write(RED_TBL + 2 * idx, low as u32);
}
