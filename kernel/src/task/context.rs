// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! x86-64 context switch (System V): saves the registers the caller does
//! not expect to change (rbx, rbp, r12..r15) and the stack pointer; the
//! compiler saves everything else around the call.
//!
//! A new thread starts with a stack prepared by `initial_stack_frame` so
//! that the first `switch_context` into it "returns" to `thread_start`,
//! which releases the scheduler lock, enables interrupts and calls the
//! entry point.

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.switch_context, "ax"
.global switch_context
// switch_context(old_rsp: *mut u64 [rdi], new_rsp: u64 [rsi])
switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.global thread_start
// First "return" of a new thread: r12 = entry, r13 = argument. Before
// enabling interrupts it releases the scheduler lock left held by the CPU
// that switched to it.
thread_start:
    call thread_first_run
    mov rdi, r13
    sti
    call r12
    call thread_exit_trampoline
    ud2
"#
);

extern "C" {
    /// Saves the current context in `*old_rsp` and jumps to the one at `new_rsp`.
    pub fn switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn thread_start();
}

/// Signature of a thread entry point.
pub type ThreadEntry = extern "C" fn(arg: usize);

/// Writes at the top of a new stack the frame `switch_context` will consume
/// on the first entry. Returns the `rsp` to save.
///
/// # Safety
///
/// `stack_top` must be the (exclusive) end of a mapped, writable stack with
/// room for 8 words.
pub unsafe fn initial_stack_frame(stack_top: u64, entry: ThreadEntry, arg: usize) -> u64 {
    // Alignment: after the `ret` to thread_start, rsp must be a multiple of 16
    // before `call r12` so the entry sees rsp = 8 (mod 16), as the ABI
    // requires. 7 words are pushed: `ret` consumes one and `switch_context`
    // six, so the top must be a multiple of 16.
    let mut sp = stack_top & !0xF;
    let mut push = |v: u64| {
        sp -= 8;
        (sp as *mut u64).write(v);
    };
    push(thread_start as *const () as usize as u64); // return address for `ret`
    push(0); // rbp
    push(0); // rbx
    push(entry as usize as u64); // r12
    push(arg as u64); // r13
    push(0); // r14
    push(0); // r15
    sp
}
