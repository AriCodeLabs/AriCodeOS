//! Output on serial port COM1 (16550). The only reliable console in M0 and
//! the one the QEMU test runner captures.

use crate::arch::x86_64::port::{inb, outb};
use core::fmt;

const COM1: u16 = 0x3F8;

/// Serial port COM1. Stateless type: the UART registers are the state.
pub struct Serial;

impl Serial {
    /// Configures 115200 8N1 without interrupts, FIFO enabled.
    pub fn init(&self) {
        // SAFETY: standard 16550 initialization sequence on COM1.
        unsafe {
            outb(COM1 + 1, 0x00); // no interrupts
            outb(COM1 + 3, 0x80); // DLAB on
            outb(COM1, 0x01); // divisor 1 -> 115200
            outb(COM1 + 1, 0x00);
            outb(COM1 + 3, 0x03); // 8N1, DLAB off
            outb(COM1 + 2, 0xC7); // FIFO on, clear, threshold 14
            outb(COM1 + 4, 0x0B); // DTR, RTS, OUT2
        }
    }

    fn write_byte(&self, byte: u8) {
        // SAFETY: LSR read and THR write on COM1.
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

/// Text without log formatting on every console (serial and framebuffer).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::write_raw(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::console::write_raw(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Serial only: for exceptions and the panic handler, which must not touch
/// the framebuffer console. Each message goes out whole (serial lock with a
/// bounded wait; see `console::write_serial`).
#[macro_export]
macro_rules! serial_println {
    () => { $crate::console::write_serial(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::console::write_serial(format_args!("{}\n", format_args!($($arg)*))) };
}
