//! Transición a ring 3.

use aricode_x86_cpu::layout;
use core::arch::asm;

/// Salta a `rip` en ring 3 con la pila `rsp`, `arg` en `rdi` e interrupciones
/// habilitadas.
/// Nunca retorna: el proceso solo vuelve al kernel por `syscall`,
/// interrupción o excepción.
///
/// # Safety
///
/// `rip` y `rsp` deben estar mapeados con USER en el espacio activo; la pila
/// de kernel del hilo debe estar fijada en el TSS y en `syscall`.
pub unsafe fn enter(rip: u64, rsp: u64, arg: u64) -> ! {
    const RFLAGS_IF: u64 = 1 << 9;
    asm!(
        "mov ds, {ds:x}",
        "mov es, {ds:x}",
        "push {ss}",
        "push {rsp}",
        "push {rflags}",
        "push {cs}",
        "push {rip}",
        "iretq",
        ds = in(reg) layout::USER_DATA.0 as u64,
        ss = in(reg) layout::USER_DATA.0 as u64,
        rsp = in(reg) rsp,
        rflags = in(reg) RFLAGS_IF | 2,
        cs = in(reg) layout::USER_CODE.0 as u64,
        rip = in(reg) rip,
        in("rdi") arg,
        options(noreturn)
    );
}
