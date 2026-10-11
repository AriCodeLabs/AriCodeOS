//! Machine reboot (Phase 21, `system_power`).

use super::port::{inb, outb};

/// Reboots through the 8042 controller (reset pulse) and, if that has no
/// effect, forces a triple fault. Never returns.
pub fn reboot() -> ! {
    // SAFETY: 8042 ports; standard PC reset sequence.
    unsafe {
        for _ in 0..100_000 {
            if inb(0x64) & 0x02 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
    }
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }
    // Triple fault: empty IDT and an interrupt.
    let idt: [u16; 5] = [0; 5];
    // SAFETY: loading an IDT with limit 0 and raising an interrupt resets
    // the CPU; there is no way back and that is the intent.
    unsafe {
        core::arch::asm!("lidt [{}]", "int3", in(reg) &idt, options(nostack));
    }
    crate::halt_loop()
}
