//! Terminación de QEMU vía `isa-debug-exit` (puerto 0xF4). Solo con la feature
//! `qemu-exit`; en hardware real no existe y escribir ahí es inocuo.
//!
//! QEMU sale con código `(valor << 1) | 1`.

#[allow(dead_code)]
#[repr(u8)]
pub enum ExitCode {
    /// -> código de salida 33
    Success = 0x10,
    /// -> código de salida 35
    Failed = 0x11,
}

pub fn exit(code: ExitCode) -> ! {
    // SAFETY: escritura a un puerto de depuración de QEMU.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xF4u16, in("al") code as u8, options(nomem, nostack));
    }
    crate::halt_loop()
}
