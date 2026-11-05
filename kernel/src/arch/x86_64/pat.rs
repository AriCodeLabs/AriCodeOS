// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The page attribute table (ADR 0078, stage 0): `aricode_x86_paging::pat`,
//! where entry 1 is write-combining, for the framebuffer.

use super::msr;
use core::arch::asm;
use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicBool, Ordering};

const IA32_PAT: u32 = 0x277;
/// CPUID.01H:EDX: the table exists (a hypervisor may hide it).
const CPUID_PAT: u32 = 1 << 16;
const CR0_CD: u64 = 1 << 30;
const CR0_NW: u64 = 1 << 29;

static AVAILABLE: AtomicBool = AtomicBool::new(true);

/// Programs this CPU's table. The register is per CPU: the BSP and every
/// AP call this at startup, with interrupts off, before anything is mapped
/// with entry 1.
///
/// The firmware may have left a table of its own under pages in use, so
/// this follows the Intel SDM's procedure for changing memory types (vol. 3,
/// "Programming the PAT"): caches off and flushed, TLB flushed, then the
/// same again after the write. It runs once per CPU per boot.
pub fn init() {
    if __cpuid(1).edx & CPUID_PAT == 0 {
        AVAILABLE.store(false, Ordering::Relaxed);
        log::warn!("cpu: no PAT: the framebuffer stays uncached");
        return;
    }
    let mut cr0: u64;
    // SAFETY: ring 0, interrupts off. Turning the cache off and flushing it
    // loses nothing, and flushing the TLB only makes it refill. The table's
    // only change from the power-on value is an entry no page uses yet.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        asm!("mov cr0, {}", in(reg) (cr0 | CR0_CD) & !CR0_NW, options(nostack));
        asm!("wbinvd", options(nostack, preserves_flags));
        super::smp::flush_all_local();
        msr::write(IA32_PAT, aricode_x86_paging::pat::VALUE);
        asm!("wbinvd", options(nostack, preserves_flags));
        super::smp::flush_all_local();
        asm!("mov cr0, {}", in(reg) cr0, options(nostack));
    }
}

/// The leaf flags for a write-combining page: uncached on a CPU without
/// the table, whose entry 1 would be write-through.
pub fn write_combining_flags() -> u64 {
    use aricode_x86_paging::{pat, CACHE_DISABLE, WRITE_THROUGH};
    if AVAILABLE.load(Ordering::Relaxed) {
        pat::WRITE_COMBINING
    } else {
        CACHE_DISABLE | WRITE_THROUGH
    }
}
