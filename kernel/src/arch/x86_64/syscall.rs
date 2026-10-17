// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Syscall entry through the `syscall` instruction (Phase 11).
//!
//! `syscall` does not switch stacks: the entry swaps GS (`swapgs`) to see
//! this CPU's `Cpu`, saves the user `rsp` in its scratch slot, loads the
//! current thread's kernel stack (set on every context switch) and builds a
//! `SyscallFrame` with the arguments. Interrupts arrive masked by
//! `IA32_FMASK` and are enabled once on the kernel stack; they are disabled
//! again before `sysretq`, which is preceded by the return `swapgs`.
//!
//! The scratch slot is per CPU: between `syscall` and loading the kernel
//! stack nothing else can run on this CPU (IF = 0) and other CPUs use their
//! own.

use super::msr;
use crate::cpu::{OFF_KERNEL_RSP, OFF_USER_RSP};
use aricode_x86_cpu::layout;
use core::arch::global_asm;

global_asm!(
    r#"
.section .text.syscall_entry, "ax"
.global syscall_entry
syscall_entry:
    swapgs
    mov gs:[{user_rsp}], rsp
    mov rsp, gs:[{kernel_rsp}]
    push qword ptr gs:[{user_rsp}]
    push r11        // user rflags
    push rcx        // user rip
    push rax        // number
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9
    mov rdi, rsp
    cld
    call syscall_dispatch
    cli
    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    add rsp, 8      // the number is replaced by the result in rax
    pop rcx
    pop r11
    pop rsp
    swapgs
    sysretq
"#,
    user_rsp = const OFF_USER_RSP,
    kernel_rsp = const OFF_KERNEL_RSP,
);

extern "C" {
    fn syscall_entry();
}

/// What `syscall_entry` leaves on the stack, read from `rsp` upwards.
#[repr(C)]
#[derive(Debug)]
pub struct SyscallFrame {
    pub r9: u64,
    pub r8: u64,
    pub r10: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub number: u64,
    pub user_rip: u64,
    pub user_rflags: u64,
    pub user_rsp: u64,
}

/// Enables `syscall`/`sysret` and points them at the entry. The MSRs are
/// per CPU: each one calls this at startup.
pub fn init() {
    // SAFETY: the values follow the GDT layout and the entry is linked in.
    unsafe {
        msr::write(msr::IA32_EFER, msr::read(msr::IA32_EFER) | msr::EFER_SCE);
        msr::write(msr::IA32_STAR, layout::STAR);
        msr::write(msr::IA32_LSTAR, syscall_entry as *const () as u64);
        // Mask IF, TF, DF and AC on entry.
        msr::write(msr::IA32_FMASK, (1 << 9) | (1 << 8) | (1 << 10) | (1 << 18));
    }
}

/// Rust entry point. Returns the value for `rax`.
#[no_mangle]
extern "C" fn syscall_dispatch(frame: &mut SyscallFrame) -> i64 {
    // Already on the kernel stack: interrupts can be serviced.
    // SAFETY: kernel state is consistent; the thread has its kernel stack.
    unsafe { crate::sync::enable_interrupts() };
    crate::user::syscalls::dispatch(frame)
}
