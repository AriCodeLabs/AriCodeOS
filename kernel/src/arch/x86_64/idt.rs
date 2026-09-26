//! IDT: los 256 vectores apuntan a los stubs generados; #DF usa la pila IST.

use super::gdt::{self, DOUBLE_FAULT_IST};
use crate::sync::IrqLock;
use aricode_x86_cpu::{exception, Idt, IdtEntry};
use core::arch::asm;

// La CPU lee la IDT por su dirección; el lock solo serializa la construcción.
static IDT: IrqLock<Idt> = IrqLock::new(Idt::new());

extern "C" {
    /// Tabla generada en `stubs.s`: dirección del stub de cada vector.
    static interrupt_stub_table: [u64; 256];
}

pub fn init() {
    let cs = gdt::selectors().code;
    let ptr = {
        let mut idt = IDT.lock();
        for vector in 0..=255u8 {
            let ist = if vector == exception::DOUBLE_FAULT {
                DOUBLE_FAULT_IST
            } else {
                0
            };
            // SAFETY: la tabla de stubs es de solo lectura y está enlazada.
            let handler = unsafe { interrupt_stub_table[vector as usize] };
            idt.set(vector, IdtEntry::interrupt_gate(handler, cs, ist, 0));
        }
        idt.pointer()
    };
    // SAFETY: la IDT es estática, construida arriba y nunca se mueve.
    unsafe { asm!("lidt [{}]", in(reg) &ptr, options(nostack, preserves_flags)) };
}
