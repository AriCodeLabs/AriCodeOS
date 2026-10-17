// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! 8254 PIT, channel 2, as a time reference to calibrate the APIC.
//! It raises no interrupts: the OUT2 output is polled through port 0x61.

use super::port::{inb, outb};

const PIT_HZ: u64 = 1_193_182;
const PIT_CH0: u16 = 0x40;
const PIT_CH2: u16 = 0x42;
const PIT_CMD: u16 = 0x43;
const SPEAKER_CTRL: u16 = 0x61;

/// Busy-waits `ms` milliseconds (54 at most) as measured by the PIT.
pub fn wait_ms(ms: u64) {
    let count = (PIT_HZ * ms / 1000).min(0xFFFF) as u16;
    // SAFETY: programs channel 2 and the speaker gate; no sound because
    // bit 1 (speaker enable) stays at 0.
    unsafe {
        let ctrl = inb(SPEAKER_CTRL) & !0x03;
        outb(SPEAKER_CTRL, ctrl); // gate low
        outb(PIT_CMD, 0xB0); // channel 2, lo/hi byte, mode 0 (one-shot count)
        outb(PIT_CH2, count as u8);
        outb(PIT_CH2, (count >> 8) as u8);
        outb(SPEAKER_CTRL, ctrl | 0x01); // gate high: start counting
        while inb(SPEAKER_CTRL) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        outb(SPEAKER_CTRL, ctrl);
    }
}

/// Channel 0 as a periodic generator (IRQ0) at `hz`: an interrupt source for
/// testing user drivers.
pub fn start_periodic(hz: u64) {
    let divisor = (PIT_HZ / hz).clamp(1, 0xFFFF) as u16;
    // SAFETY: channel 0, mode 2 (rate generator), lo/hi byte.
    unsafe {
        outb(PIT_CMD, 0x34);
        outb(PIT_CH0, divisor as u8);
        outb(PIT_CH0, (divisor >> 8) as u8);
    }
}

/// Stops channel 0 (mode 0 one-shot: silence after one firing).
pub fn stop() {
    // SAFETY: channel 0, mode 0, count 1: at most one more interrupt.
    unsafe {
        outb(PIT_CMD, 0x30);
        outb(PIT_CH0, 1);
        outb(PIT_CH0, 0);
    }
}
