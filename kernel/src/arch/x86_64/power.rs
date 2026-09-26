//! Reinicio de la máquina (Fase 21, `system_power`).

use super::port::{inb, outb};

/// Reinicia por el controlador 8042 (pulso de reset) y, si no surte efecto,
/// fuerza un triple fallo. Nunca retorna.
pub fn reboot() -> ! {
    // SAFETY: puertos del 8042; secuencia estándar de reset del PC.
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
    // Triple fallo: IDT vacía y una interrupción.
    let idt: [u16; 5] = [0; 5];
    // SAFETY: cargar una IDT de límite 0 y provocar una interrupción reinicia
    // la CPU; no hay vuelta atrás y es la intención.
    unsafe {
        core::arch::asm!("lidt [{}]", "int3", in(reg) &idt, options(nostack));
    }
    crate::halt_loop()
}
