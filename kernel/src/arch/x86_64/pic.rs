//! 8259 PIC heredado: se remapea fuera de los vectores de excepción y se
//! enmascara por completo. Las interrupciones llegan por el APIC.

use super::port::{inb, io_wait, outb};

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// Vectores a los que se remapean IRQ0..15 antes de enmascararlos: aunque
/// llegara una espuria, no colisionaría con una excepción (0..31).
pub const PIC1_OFFSET: u8 = 0x20;
pub const PIC2_OFFSET: u8 = 0x28;

pub fn disable() {
    // SAFETY: secuencia de inicialización ICW1..ICW4 del 8259 y máscara total.
    unsafe {
        outb(PIC1_CMD, 0x11); // ICW1: init + ICW4
        io_wait();
        outb(PIC2_CMD, 0x11);
        io_wait();
        outb(PIC1_DATA, PIC1_OFFSET); // ICW2: offsets
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();
        outb(PIC1_DATA, 4); // ICW3: esclavo en IRQ2
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();
        outb(PIC1_DATA, 0x01); // ICW4: modo 8086
        io_wait();
        outb(PIC2_DATA, 0x01);
        io_wait();
        outb(PIC1_DATA, 0xFF); // enmascarar todo
        outb(PIC2_DATA, 0xFF);
        let _ = inb(PIC1_DATA);
    }
}
