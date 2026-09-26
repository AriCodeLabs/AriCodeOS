//! Salida por puerto serie COM1 (16550). Única consola fiable en M0 y la que
//! captura el test runner de QEMU.

use crate::arch::x86_64::port::{inb, outb};
use core::fmt;

const COM1: u16 = 0x3F8;

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

/// Texto sin formato de log en todas las consolas (serie y framebuffer).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::write_raw(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::console::write_raw(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Solo serie: para el panic handler, que no debe tocar estado compartido.
#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($crate::serial::Serial, $($arg)*);
    }};
}
