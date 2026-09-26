//! Manejo de excepciones: diagnóstico detallado (sección 17 del documento
//! maestro) y pánico, salvo #BP, que es recuperable y sirve de prueba.
//!
//! Todo el volcado va solo por serie (`serial_println!`): una excepción puede
//! ocurrir con la consola de framebuffer a medio escribir.

use super::interrupts::{read_cr2, InterruptFrame};
use crate::serial_println;
use aricode_x86_cpu::exception::{self, *};

pub fn handle(frame: &mut InterruptFrame) {
    let vector = frame.vector as u8;
    match vector {
        BREAKPOINT => {
            log::warn!("#BP breakpoint en rip={:#x}, continuando", frame.rip);
        }
        0..=31 => fatal(frame),
        _ => {
            // Sin APIC ni IRQs habilitadas todavía; cualquier vector externo
            // es inesperado pero no fatal.
            log::warn!("interrupcion inesperada, vector {vector}");
        }
    }
}

fn fatal(frame: &InterruptFrame) -> ! {
    let vector = frame.vector as u8;
    let (mnemonic, name) = exception::name(vector);
    serial_println!();
    serial_println!("EXCEPTION {mnemonic} ({vector}) {name}");
    serial_println!(
        "RIP: {:#018x}  CS: {:#06x}  RFLAGS: {:#018x}",
        frame.rip,
        frame.cs,
        frame.rflags
    );
    serial_println!("RSP: {:#018x}  SS: {:#06x}", frame.rsp, frame.ss);
    if exception::has_error_code(vector) {
        serial_println!("ERROR: {:#x}", frame.error_code);
    }
    let cr2 = read_cr2();
    if crate::memory::vmm::in_stack_guard(frame.rsp) || crate::memory::vmm::in_stack_guard(cr2) {
        serial_println!(
            "STACK OVERFLOW: la pila del kernel toco su guard page ({:#x}..{:#x})",
            crate::memory::vmm::KERNEL_STACK_GUARD,
            crate::memory::vmm::KERNEL_STACK_BOTTOM
        );
    }
    if vector == PAGE_FAULT {
        let e = frame.error_code;
        serial_println!("CR2: {:#018x}", cr2);
        serial_println!(
            "ACCESS: {}  MODE: {}  CAUSE: {}",
            if e & PF_INSTRUCTION != 0 {
                "FETCH"
            } else if e & PF_WRITE != 0 {
                "WRITE"
            } else {
                "READ"
            },
            if e & PF_USER != 0 { "USER" } else { "KERNEL" },
            if e & PF_RESERVED != 0 {
                "reserved bit"
            } else if e & PF_PRESENT != 0 {
                "protection"
            } else {
                "not present"
            }
        );
    }
    serial_println!(
        "RAX: {:#018x}  RBX: {:#018x}  RCX: {:#018x}  RDX: {:#018x}",
        frame.rax,
        frame.rbx,
        frame.rcx,
        frame.rdx
    );
    serial_println!(
        "RSI: {:#018x}  RDI: {:#018x}  RBP: {:#018x}  R8:  {:#018x}",
        frame.rsi,
        frame.rdi,
        frame.rbp,
        frame.r8
    );
    serial_println!(
        "R9:  {:#018x}  R10: {:#018x}  R11: {:#018x}  R12: {:#018x}",
        frame.r9,
        frame.r10,
        frame.r11,
        frame.r12
    );
    serial_println!(
        "R13: {:#018x}  R14: {:#018x}  R15: {:#018x}",
        frame.r13,
        frame.r14,
        frame.r15
    );
    panic!(
        "excepcion {mnemonic} ({vector}) {name} en rip={:#x}",
        frame.rip
    );
}
