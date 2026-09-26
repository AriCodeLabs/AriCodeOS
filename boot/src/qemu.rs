//! Terminación de QEMU vía `isa-debug-exit` (puerto 0xF4) cuando la cmdline
//! incluye `qemu_exit`. Permite que los escenarios de fallo del cargador
//! terminen rápido en vez de esperar al timeout del runner.

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum ExitCode {
    /// -> código de salida 35
    Failed = 0x11,
}

pub fn exit(code: ExitCode) -> ! {
    // SAFETY: escritura a un puerto de depuración de QEMU; sin efectos en memoria.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xF4u16, in("al") code as u8, options(nomem, nostack));
    }
    loop {
        // SAFETY: si no estamos en QEMU, detener la CPU.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
