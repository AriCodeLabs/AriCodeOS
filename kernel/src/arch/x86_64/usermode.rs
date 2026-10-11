//! Transition to ring 3.

use aricode_x86_cpu::layout;
use core::arch::asm;

/// Jumps to `rip` in ring 3 with stack `rsp`, `arg` in `rdi` and interrupts
/// enabled.
/// Never returns: the process only comes back to the kernel through
/// `syscall`, an interrupt or an exception.
///
/// # Safety
///
/// `rip` and `rsp` must be mapped USER in the active address space; the
/// thread's kernel stack must be set in the TSS and for `syscall`.
pub unsafe fn enter(rip: u64, rsp: u64, arg: u64) -> ! {
    const RFLAGS_IF: u64 = 1 << 9;
    // `cli` before `swapgs`: between it and `iretq` the CPU is in ring 0
    // with the user GS, and an interrupt would not swap it.
    asm!(
        "cli",
        "mov ds, {ds:x}",
        "mov es, {ds:x}",
        "push {ss}",
        "push {rsp}",
        "push {rflags}",
        "push {cs}",
        "push {rip}",
        "swapgs",
        "iretq",
        ds = in(reg) layout::USER_DATA.0 as u64,
        ss = in(reg) layout::USER_DATA.0 as u64,
        rsp = in(reg) rsp,
        rflags = in(reg) RFLAGS_IF | 2,
        cs = in(reg) layout::USER_CODE.0 as u64,
        rip = in(reg) rip,
        in("rdi") arg,
        options(noreturn)
    );
}
