//! IDT: los 256 vectores apuntan a los stubs generados; #DF usa la pila IST.

use super::gdt::{self, DOUBLE_FAULT_IST};
use crate::sync::SingleCpu;
use aricode_x86_cpu::{exception, Idt, IdtEntry};
use core::arch::asm;

static IDT: SingleCpu<Idt> = SingleCpu::new(Idt::new());

extern "C" {
    /// Tabla generada en `stubs.s`: dirección del stub de cada vector.
    static interrupt_stub_table: [u64; 256];
}

pub fn init() {
    let cs = gdt::selectors().code;
    // SAFETY: una CPU, interrupciones deshabilitadas, llamada única tras
    // `gdt::init`; la tabla de stubs es de solo lectura y está enlazada.
    unsafe {
        let idt = &mut *IDT.get();
        for vector in 0..=255u8 {
            let ist = if vector == exception::DOUBLE_FAULT {
                DOUBLE_FAULT_IST
            } else {
                0
            };
            let handler = interrupt_stub_table[vector as usize];
            idt.set(vector, IdtEntry::interrupt_gate(handler, cs, ist, 0));
        }
        let ptr = idt.pointer();
        asm!("lidt [{}]", in(reg) &ptr, options(nostack, preserves_flags));
    }
}
