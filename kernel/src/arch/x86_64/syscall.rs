//! Entrada de syscalls por la instrucción `syscall` (Fase 11).
//!
//! `syscall` no cambia de pila: la entrada intercambia GS (`swapgs`) para
//! ver el `Cpu` de esta CPU, guarda el `rsp` de usuario en su scratch, carga
//! la pila de kernel del hilo actual (fijada en cada cambio de contexto) y
//! construye un `SyscallFrame` con los argumentos. Las interrupciones llegan
//! enmascaradas por `IA32_FMASK` y se habilitan una vez en la pila de
//! kernel; se vuelven a deshabilitar antes de `sysretq`, que va precedido
//! del `swapgs` de vuelta.
//!
//! El scratch es por CPU: entre `syscall` y la carga de la pila de kernel
//! no puede entrar nadie en esta CPU (IF = 0) y otra CPU usa el suyo.

use super::msr;
use crate::cpu::{OFF_KERNEL_RSP, OFF_USER_RSP};
use aricode_x86_cpu::layout;
use core::arch::global_asm;

global_asm!(
    r#"
.section .text.syscall_entry, "ax"
.global syscall_entry
syscall_entry:
    swapgs
    mov gs:[{user_rsp}], rsp
    mov rsp, gs:[{kernel_rsp}]
    push qword ptr gs:[{user_rsp}]
    push r11        // rflags de usuario
    push rcx        // rip de usuario
    push rax        // numero
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9
    mov rdi, rsp
    cld
    call syscall_dispatch
    cli
    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    add rsp, 8      // el numero se sustituye por el resultado en rax
    pop rcx
    pop r11
    pop rsp
    swapgs
    sysretq
"#,
    user_rsp = const OFF_USER_RSP,
    kernel_rsp = const OFF_KERNEL_RSP,
);

extern "C" {
    fn syscall_entry();
}

/// Lo que `syscall_entry` deja en la pila, leído desde `rsp` hacia arriba.
#[repr(C)]
#[derive(Debug)]
pub struct SyscallFrame {
    pub r9: u64,
    pub r8: u64,
    pub r10: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub number: u64,
    pub user_rip: u64,
    pub user_rflags: u64,
    pub user_rsp: u64,
}

/// Habilita `syscall`/`sysret` y apunta la entrada. Los MSR son por CPU:
/// cada una lo llama al arrancar.
pub fn init() {
    // SAFETY: los valores siguen el layout de la GDT y la entrada está enlazada.
    unsafe {
        msr::write(msr::IA32_EFER, msr::read(msr::IA32_EFER) | msr::EFER_SCE);
        msr::write(msr::IA32_STAR, layout::STAR);
        msr::write(msr::IA32_LSTAR, syscall_entry as *const () as u64);
        // Enmascarar IF, TF, DF y AC al entrar.
        msr::write(msr::IA32_FMASK, (1 << 9) | (1 << 8) | (1 << 10) | (1 << 18));
    }
}

/// Punto de entrada Rust. Retorna el valor para `rax`.
#[no_mangle]
extern "C" fn syscall_dispatch(frame: &mut SyscallFrame) -> i64 {
    // Ya en la pila de kernel: se pueden atender interrupciones.
    // SAFETY: estado del kernel consistente; el hilo tiene su pila de kernel.
    unsafe { crate::sync::enable_interrupts() };
    crate::user::syscalls::dispatch(frame)
}
