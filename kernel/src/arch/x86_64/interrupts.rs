// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Interrupt frame and common trampoline (see `stubs.s`).

use core::arch::global_asm;

global_asm!(include_str!("stubs.s"));

/// What `interrupt_common` leaves on the stack, in the order read from `rsp`
/// upwards. The CPU pushes the last five fields.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Rust entry point for all interrupts and exceptions.
#[no_mangle]
extern "C" fn interrupt_dispatch(frame: &mut InterruptFrame) {
    super::exceptions::handle(frame);
}

/// Reads CR2 (address that caused the last #PF).
pub fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: read of a control register; no side effects.
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags))
    };
    v
}
