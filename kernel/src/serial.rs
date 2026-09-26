//! Salida por puerto serie COM1 (16550). Única consola fiable en M0 y la que
//! captura el test runner de QEMU.

use core::arch::asm;
use core::fmt;

const COM1: u16 = 0x3F8;

#[inline]
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: instrucción de E/S; el llamador garantiza que el puerto es COM1.
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: ídem `outb`.
    asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

/// Puerto serie COM1. Tipo sin estado: los registros del UART son el estado.
pub struct Serial;

impl Serial {
    /// Configura 115200 8N1 sin interrupciones, FIFO activada.
    pub fn init(&self) {
        // SAFETY: secuencia estándar de inicialización del 16550 sobre COM1.
        unsafe {
            outb(COM1 + 1, 0x00); // sin interrupciones
            outb(COM1 + 3, 0x80); // DLAB on
            outb(COM1, 0x01); // divisor 1 -> 115200
            outb(COM1 + 1, 0x00);
            outb(COM1 + 3, 0x03); // 8N1, DLAB off
            outb(COM1 + 2, 0xC7); // FIFO on, limpiar, umbral 14
            outb(COM1 + 4, 0x0B); // DTR, RTS, OUT2
        }
    }

    fn write_byte(&self, byte: u8) {
        // SAFETY: lectura de LSR y escritura de THR sobre COM1.
        unsafe {
            while inb(COM1 + 5) & 0x20 == 0 {}
            outb(COM1, byte);
        }
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::serial::Serial, $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($crate::serial::Serial, $($arg)*);
    }};
}
