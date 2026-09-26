//! Terminación de QEMU vía `isa-debug-exit` (puerto 0xF4).
//!
//! Solo se usa si la línea de comandos incluye `qemu_exit`; en hardware real
//! el puerto no existe y la escritura es inocua. QEMU sale con
//! `(valor << 1) | 1`: 0x10 -> 33 (éxito), 0x11 -> 35 (fallo).

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum ExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit(code: ExitCode) -> ! {
    // SAFETY: escritura a un puerto de depuración de QEMU; sin efectos en memoria.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xF4u16, in("al") code as u8, options(nomem, nostack));
    }
    crate::halt_loop()
}
