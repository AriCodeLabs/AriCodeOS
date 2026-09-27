//! Sincronización del kernel: `IrqLock` de `aricode-sync` con la política de
//! interrupciones de x86-64 (guardar `RFLAGS.IF`, `cli`, y restaurar).
//!
//! Anidamientos permitidos (exterior -> interior), documentados en
//! `docs/architecture.md`, sección Sincronización:
//!
//! ```text
//! VMM -> PMM            el Mapper pide frames al PMM con el VMM tomado
//! heap.mapped -> VMM    y por tanto heap.mapped -> PMM
//! PROCESSES -> SCHED    process::wait consulta el planificador
//! PROCESSES -> IOMMU    soltar un buffer DMA (tabla de handles) lo retira de los dominios
//! IOMMU -> PMM          los dominios piden y devuelven frames de tablas
//! ```
//!
//! Nunca al revés: en particular, nunca entrar al VMM con el PMM tomado, y
//! nunca tomar PROCESSES desde dentro del planificador. Los demás locks
//! (heap.heap, console, colas de espera) son hoja.

use aricode_sync::IrqPolicy;
use core::arch::asm;

pub struct KernelIrq;

impl IrqPolicy for KernelIrq {
    type State = bool;

    fn save_and_disable() -> bool {
        let rflags: u64;
        // SAFETY: leer RFLAGS y deshabilitar interrupciones no toca memoria.
        unsafe {
            asm!("pushfq", "pop {}", "cli", out(reg) rflags, options(nomem, preserves_flags));
        }
        rflags & (1 << 9) != 0
    }

    fn restore(was_enabled: bool) {
        if was_enabled {
            // SAFETY: rehabilitar interrupciones que estaban habilitadas al entrar.
            unsafe { asm!("sti", options(nomem, nostack)) };
        }
    }
}

pub type IrqLock<T> = aricode_sync::IrqLock<T, KernelIrq>;

/// `true` si las interrupciones están habilitadas en esta CPU.
pub fn interrupts_enabled() -> bool {
    let rflags: u64;
    // SAFETY: solo lee RFLAGS.
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & (1 << 9) != 0
}

/// Habilita las interrupciones externas. Solo tras instalar IDT y APIC.
///
/// # Safety
///
/// Todo handler alcanzable debe estar instalado y el estado global protegido.
pub unsafe fn enable_interrupts() {
    asm!("sti", options(nomem, nostack));
}
