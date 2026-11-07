// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Copies between the kernel and a process that fail instead of faulting the kernel
//! (ADR 0082, decision 6). A range is checked before the copy, but with several
//! threads another one may unmap it in between: the copy's page fault then lands
//! on one known instruction, and the page-fault handler sends it to a recovery
//! point that returns an error, as Linux's exception table and Zircon's user copy
//! do. No other kernel fault is ever recovered.

use super::interrupts::InterruptFrame;
use core::arch::global_asm;

// `aricode_user_copy(dst, src, len) -> 0 copied, 1 faulted`, System V: rdi, rsi,
// rdx. `rep movsb` is the only instruction that touches the process's memory, and
// a fault leaves `rip` on it. Rust's ABI enters with the direction flag clear.
global_asm!(
    ".global aricode_user_copy",
    "aricode_user_copy:",
    "    mov rcx, rdx",
    ".global aricode_user_copy_insn",
    "aricode_user_copy_insn:",
    "    rep movsb",
    "    xor eax, eax",
    "    ret",
    ".global aricode_user_copy_fault",
    "aricode_user_copy_fault:",
    "    mov eax, 1",
    "    ret",
);

extern "C" {
    fn aricode_user_copy(dst: *mut u8, src: *const u8, len: usize) -> u64;
    fn aricode_user_copy_insn();
    fn aricode_user_copy_fault();
}

/// Copies `len` bytes from `src` to `dst`, one of them in the active process's
/// address space; `false` if a page of it was not there.
///
/// # Safety
/// The kernel side must be valid for `len` bytes; the process side must have been
/// checked as the process's own memory (`check_user_range`), so that only its
/// disappearance can fault.
pub unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) -> bool {
    // SAFETY: as the caller promised; a fault on the process side is recovered.
    unsafe { aricode_user_copy(dst, src, len) == 0 }
}

/// A page fault in ring 0 on the copy's instruction: the copy returns its error.
/// `false` for any other kernel fault, which stays fatal.
pub fn recover(frame: &mut InterruptFrame) -> bool {
    if frame.cs & 3 != 0 || frame.rip != aricode_user_copy_insn as *const () as u64 {
        return false;
    }
    frame.rip = aricode_user_copy_fault as *const () as u64;
    true
}

/// `test=usercopy`: a copy from a page that is not mapped returns its error instead
/// of a kernel panic, and a copy that is fine still copies.
pub fn selftest() {
    let mut out = [0u8; 16];
    let source = [7u8; 16];
    // SAFETY: both kernel buffers of 16 bytes.
    let ok = unsafe { copy(out.as_mut_ptr(), source.as_ptr(), 16) };
    assert!(ok && out == source, "usercopy selftest: a good copy failed");
    // Page 0 is never mapped: the null guard.
    // SAFETY: the copy's fault is recovered; `out` is valid.
    let faulted = !unsafe { copy(out.as_mut_ptr(), 0x10 as *const u8, 16) };
    assert!(
        faulted,
        "usercopy selftest: a copy from page 0 did not fail"
    );
    log::info!("usercopy selftest: a fault in a user copy returns an error: OK");
}
