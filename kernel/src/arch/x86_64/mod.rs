//! x86-64: GDT/TSS, IDT, interrupt stubs and exception handling.

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

/// Installs the BSP's per-CPU tables (GDT + TSS with an IST stack for #DF),
/// the IDT, `syscall` and the GS base. Must be called once, with interrupts
/// disabled, before any operation that can fail in a controlled way. The
/// APs repeat the same steps in `smp::ap_entry`.
pub fn init() {
    let bsp = crate::cpu::slot(0);
    gdt::init_for(bsp);
    idt::init();
    syscall::init();
    crate::cpu::activate(bsp);
    bsp.set_online();
}
