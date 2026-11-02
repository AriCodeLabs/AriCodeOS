// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! x86-64: GDT/TSS, IDT, interrupt stubs and exception handling.

pub mod apic;
pub mod exceptions;
pub mod fpu;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod ioapic;
pub mod msr;
pub mod pic;
pub mod pit;
pub mod port;
pub mod power;
pub mod smp;
pub mod syscall;
pub mod usermode;

/// Installs the BSP's per-CPU tables (GDT + TSS with an IST stack for #DF),
/// the IDT, `syscall` and the GS base, and enables the FPU and SIMD state
/// user threads get (ADR 0065). Must be called once, with interrupts
/// disabled, before any operation that can fail in a controlled way. The
/// APs repeat the same steps in `smp::ap_entry`.
pub fn init() {
    let bsp = crate::cpu::slot(0);
    gdt::init_for(bsp);
    idt::init();
    syscall::init();
    crate::cpu::activate(bsp);
    // Before the heap: the log line formats without allocating.
    let fpu = fpu::init_bsp();
    let leak = if fpu.error_pointer_leak {
        "; x87 error pointers reset before each restore"
    } else {
        ""
    };
    match fpu.xsave_mask {
        0 => log::info!(
            "cpu: user FPU state saved eagerly with fxsave ({} bytes per thread){leak}",
            fpu.area_size
        ),
        m => log::info!(
            "cpu: user FPU state saved eagerly with xsave, xcr0 {m:#x} ({} bytes per thread){leak}",
            fpu.area_size
        ),
    }
    bsp.set_online();
}
