//! x86-64: GDT/TSS, IDT, stubs de interrupción y manejo de excepciones.

pub mod apic;
pub mod exceptions;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod ioapic;
pub mod msr;
pub mod pic;
pub mod pit;
pub mod port;
pub mod power;
pub mod smp;
pub mod syscall;
pub mod usermode;

/// Instala en el BSP sus tablas por CPU (GDT + TSS con pila IST para #DF),
/// la IDT, `syscall` y la base de GS. Debe llamarse una vez, con
/// interrupciones deshabilitadas, antes de cualquier operación que pueda
/// fallar de forma controlada. Los APs repiten lo mismo en `smp::ap_entry`.
pub fn init() {
    let bsp = crate::cpu::slot(0);
    gdt::init_for(bsp);
    idt::init();
    syscall::init();
    crate::cpu::activate(bsp);
    bsp.set_online();
}
