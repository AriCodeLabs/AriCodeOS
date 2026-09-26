//! x86-64: GDT/TSS, IDT, stubs de interrupción y manejo de excepciones.

pub mod apic;
pub mod exceptions;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod pic;
pub mod pit;
pub mod port;

/// Instala GDT + TSS (con pila IST para #DF) e IDT. Debe llamarse una vez,
/// con interrupciones deshabilitadas, antes de cualquier operación que pueda
/// fallar de forma controlada.
pub fn init() {
    gdt::init();
    idt::init();
}
