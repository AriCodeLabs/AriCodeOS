// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Legacy 8259 PIC: remapped away from the exception vectors and fully
//! masked. Interrupts arrive through the APIC.

use super::port::{inb, io_wait, outb};

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// Vectors IRQ0..15 are remapped to before masking them: even if a spurious
/// one arrived, it would not collide with an exception (0..31).
pub const PIC1_OFFSET: u8 = 0x20;
pub const PIC2_OFFSET: u8 = 0x28;

pub fn disable() {
    // SAFETY: 8259 ICW1..ICW4 initialization sequence and full mask.
    unsafe {
        outb(PIC1_CMD, 0x11); // ICW1: init + ICW4
        io_wait();
        outb(PIC2_CMD, 0x11);
        io_wait();
        outb(PIC1_DATA, PIC1_OFFSET); // ICW2: offsets
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();
        outb(PIC1_DATA, 4); // ICW3: slave on IRQ2
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();
        outb(PIC1_DATA, 0x01); // ICW4: 8086 mode
        io_wait();
        outb(PIC2_DATA, 0x01);
        io_wait();
        outb(PIC1_DATA, 0xFF); // mask everything
        outb(PIC2_DATA, 0xFF);
        let _ = inb(PIC1_DATA);
    }
}
