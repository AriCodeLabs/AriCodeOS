//! Port I/O (`in`/`out`).

use core::arch::asm;

/// # Safety
/// Writing to a port has hardware effects; the caller knows the device.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// Reading a port may have side effects on the device.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

/// # Safety
/// Writing to a port has hardware effects; the caller knows the device.
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// Reading a port may have side effects on the device.
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

/// Classic short delay: a write to an unused port.
#[inline]
pub fn io_wait() {
    // SAFETY: port 0x80 (POST) has no effect on modern hardware.
    unsafe { outb(0x80, 0) };
}
