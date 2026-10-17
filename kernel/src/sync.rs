// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel synchronization: `IrqLock` from `aricode-sync` with the x86-64
//! interrupt policy (save `RFLAGS.IF`, `cli`, and restore).
//!
//! Allowed nesting (outer -> inner), documented in `docs/architecture.md`,
//! Synchronization section:
//!
//! ```text
//! VMM -> PMM            the Mapper requests frames from the PMM with the VMM held
//! heap.mapped -> VMM    and therefore heap.mapped -> PMM
//! PROCESSES -> SCHED    process::wait queries the scheduler
//! PROCESSES -> IOMMU    dropping a DMA buffer (handle table) removes it from the domains
//! IOMMU -> PMM          domains request and return page-table frames
//! ```
//!
//! Never the other way round: in particular, never enter the VMM with the PMM
//! held, and never take PROCESSES from inside the scheduler. The other locks
//! (heap.heap, console, wait queues) are leaves.

use aricode_sync::IrqPolicy;
use core::arch::asm;

pub struct KernelIrq;

impl IrqPolicy for KernelIrq {
    type State = bool;

    fn save_and_disable() -> bool {
        let rflags: u64;
        // SAFETY: reading RFLAGS and disabling interrupts does not touch memory.
        unsafe {
            asm!("pushfq", "pop {}", "cli", out(reg) rflags, options(nomem, preserves_flags));
        }
        rflags & (1 << 9) != 0
    }

    fn restore(was_enabled: bool) {
        if was_enabled {
            // SAFETY: re-enable interrupts that were enabled on entry.
            unsafe { asm!("sti", options(nomem, nostack)) };
        }
    }
}

pub type IrqLock<T> = aricode_sync::IrqLock<T, KernelIrq>;

/// `true` if interrupts are enabled on this CPU.
pub fn interrupts_enabled() -> bool {
    let rflags: u64;
    // SAFETY: only reads RFLAGS.
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & (1 << 9) != 0
}

/// Enables external interrupts. Only after installing the IDT and APIC.
///
/// # Safety
///
/// Every reachable handler must be installed and global state protected.
pub unsafe fn enable_interrupts() {
    asm!("sti", options(nomem, nostack));
}
