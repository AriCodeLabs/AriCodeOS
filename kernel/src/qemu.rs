//! QEMU termination via `isa-debug-exit` (port 0xF4).
//!
//! Only used if the command line includes `qemu_exit`; on real hardware the
//! port does not exist and the write is harmless. QEMU exits with
//! `(value << 1) | 1`: 0x10 -> 33 (success), 0x11 -> 35 (failure).

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum ExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit(code: ExitCode) -> ! {
    // SAFETY: write to a QEMU debug port; no memory effects.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xF4u16, in("al") code as u8, options(nomem, nostack));
    }
    crate::halt_loop()
}
