//! 8254 PIT, canal 2, como referencia de tiempo para calibrar el APIC.
//! No genera interrupciones: se sondea la salida OUT2 por el puerto 0x61.

use super::port::{inb, outb};

const PIT_HZ: u64 = 1_193_182;
const PIT_CH2: u16 = 0x42;
const PIT_CMD: u16 = 0x43;
const SPEAKER_CTRL: u16 = 0x61;

/// Espera activa de `ms` milisegundos (máximo 54) medida por el PIT.
pub fn wait_ms(ms: u64) {
    let count = (PIT_HZ * ms / 1000).min(0xFFFF) as u16;
    // SAFETY: programación del canal 2 y del gate del altavoz; sin sonido
    // porque el bit 1 (speaker enable) queda a 0.
    unsafe {
        let ctrl = inb(SPEAKER_CTRL) & !0x03;
        outb(SPEAKER_CTRL, ctrl); // gate abajo
        outb(PIT_CMD, 0xB0); // canal 2, lo/hi byte, modo 0 (cuenta única)
        outb(PIT_CH2, count as u8);
        outb(PIT_CH2, (count >> 8) as u8);
        outb(SPEAKER_CTRL, ctrl | 0x01); // gate arriba: empieza a contar
        while inb(SPEAKER_CTRL) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        outb(SPEAKER_CTRL, ctrl);
    }
}
