//! Cambio de contexto x86-64 (System V): se guardan los registros que el
//! llamador no espera ver modificados (rbx, rbp, r12..r15) y el puntero de
//! pila; todo lo demás lo salva el compilador alrededor de la llamada.
//!
//! Un hilo nuevo arranca con una pila preparada por `initial_stack_frame`
//! de modo que el primer `switch_context` hacia él "retorna" a
//! `thread_start`, que suelta el lock del planificador, habilita
//! interrupciones y llama a la entrada.

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.switch_context, "ax"
.global switch_context
// switch_context(old_rsp: *mut u64 [rdi], new_rsp: u64 [rsi])
switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.global thread_start
// Primer "retorno" de un hilo nuevo: r12 = entrada, r13 = argumento. Antes
// de habilitar interrupciones suelta el lock del planificador que dejo
// tomado la CPU que conmuto hacia el.
thread_start:
    call thread_first_run
    mov rdi, r13
    sti
    call r12
    call thread_exit_trampoline
    ud2
"#
);

extern "C" {
    /// Guarda el contexto actual en `*old_rsp` y salta al de `new_rsp`.
    pub fn switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn thread_start();
}

/// Firma de la entrada de un hilo.
pub type ThreadEntry = extern "C" fn(arg: usize);

/// Escribe en la cima de una pila nueva el marco que `switch_context`
/// consumirá al entrar por primera vez. Devuelve el `rsp` a guardar.
///
/// # Safety
///
/// `stack_top` debe ser el fin (exclusivo) de una pila mapeada y escribible
/// con sitio para 8 palabras.
pub unsafe fn initial_stack_frame(stack_top: u64, entry: ThreadEntry, arg: usize) -> u64 {
    // Alineación: tras el `ret` a thread_start, rsp debe ser múltiplo de 16
    // antes del `call r12` para que la entrada vea rsp ≡ 8 (mod 16), como
    // exige el ABI. Se apilan 7 palabras: `ret` consume una y `switch_context`
    // seis, así que la cima debe ser múltiplo de 16.
    let mut sp = stack_top & !0xF;
    let mut push = |v: u64| {
        sp -= 8;
        (sp as *mut u64).write(v);
    };
    push(thread_start as *const () as usize as u64); // dirección de retorno del `ret`
    push(0); // rbp
    push(0); // rbx
    push(entry as usize as u64); // r12
    push(arg as u64); // r13
    push(0); // r14
    push(0); // r15
    sp
}
