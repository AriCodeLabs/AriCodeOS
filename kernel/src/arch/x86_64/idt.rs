// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! IDT: all 256 vectors point to the generated stubs; #DF uses the IST stack.
//! There is a single one, loaded by every CPU.

use super::gdt::{self, DOUBLE_FAULT_IST};
use crate::sync::IrqLock;
use aricode_x86_cpu::{exception, Idt, IdtEntry};
use core::arch::asm;

// The CPU reads the IDT by address; the lock only serializes construction.
static IDT: IrqLock<Idt> = IrqLock::new(Idt::new());

extern "C" {
    /// Table generated in `stubs.s`: address of each vector's stub.
    static interrupt_stub_table: [u64; 256];
}

/// Builds the IDT (once, on the BSP) and loads it.
pub fn init() {
    let cs = gdt::selectors().code;
    {
        let mut idt = IDT.lock();
        for vector in 0..=255u8 {
            let ist = if vector == exception::DOUBLE_FAULT {
                DOUBLE_FAULT_IST
            } else {
                0
            };
            // SAFETY: the stub table is read-only and linked in.
            let handler = unsafe { interrupt_stub_table[vector as usize] };
            idt.set(vector, IdtEntry::interrupt_gate(handler, cs, ist, 0));
        }
    }
    load();
}

/// Loads the already built IDT on the calling CPU (the APs).
pub fn load() {
    let ptr = IDT.lock().pointer();
    // SAFETY: the IDT is static, built in `init` and never moves.
    unsafe { asm!("lidt [{}]", in(reg) &ptr, options(nostack, preserves_flags)) };
}
