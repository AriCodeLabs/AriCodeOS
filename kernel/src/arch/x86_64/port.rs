//! E/S por puertos (`in`/`out`).

use core::arch::asm;

/// # Safety
/// Escribir en un puerto tiene efectos en hardware; el llamador conoce el dispositivo.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// Leer un puerto puede tener efectos secundarios en el dispositivo.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

/// Retardo corto clásico: una escritura a un puerto no usado.
#[inline]
pub fn io_wait() {
    // SAFETY: el puerto 0x80 (POST) no tiene efectos en hardware moderno.
    unsafe { outb(0x80, 0) };
}
