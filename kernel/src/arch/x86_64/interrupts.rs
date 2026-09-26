//! Marco de interrupción y trampolín común (ver `stubs.s`).

use core::arch::global_asm;

global_asm!(include_str!("stubs.s"));

/// Lo que `interrupt_common` deja en la pila, en el orden en que se lee desde
/// `rsp` hacia arriba. Los últimos cinco campos los empuja la CPU.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Punto de entrada Rust de todas las interrupciones y excepciones.
#[no_mangle]
extern "C" fn interrupt_dispatch(frame: &mut InterruptFrame) {
    super::exceptions::handle(frame);
}

/// Lee CR2 (dirección que provocó el último #PF).
pub fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: lectura de un registro de control; sin efectos.
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags))
    };
    v
}
