// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! QEMU termination via `isa-debug-exit` (port 0xF4) when the cmdline
//! contains `qemu_exit`. Lets loader failure scenarios end quickly instead
//! of waiting for the runner timeout.

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum ExitCode {
    /// -> exit code 35
    Failed = 0x11,
}

pub fn exit(code: ExitCode) -> ! {
    // SAFETY: write to a QEMU debug port; no memory effects.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xF4u16, in("al") code as u8, options(nomem, nostack));
    }
    loop {
        // SAFETY: if we are not running under QEMU, halt the CPU.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
