// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Per-CPU GDT and TSS (with a dedicated double-fault stack). The selector
//! layout is the same on every CPU (`aricode_x86_cpu::layout`).

use crate::cpu::Cpu;
use aricode_x86_cpu::{layout, Gdt, SegmentDescriptor, Selector, Tss, TssDescriptor};
use core::arch::asm;

/// IST index (1..=7) used by #DF. IST1 = `Tss::ist[0]`.
pub const DOUBLE_FAULT_IST: u8 = 1;
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// Tables the CPU reads by address: they live in each CPU's static `Cpu`
/// and never move.
pub struct Tables {
    gdt: Gdt<8>,
    tss: Tss,
    double_fault_stack: Stack,
}

impl Tables {
    pub const fn new() -> Self {
        Self {
            gdt: Gdt::new(),
            tss: Tss::new(),
            double_fault_stack: Stack([0; DOUBLE_FAULT_STACK_SIZE]),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Selectors {
    pub code: Selector,
    pub data: Selector,
    pub user_data: Selector,
    pub user_code: Selector,
    pub tss: Selector,
}

pub fn selectors() -> Selectors {
    Selectors {
        code: layout::KERNEL_CODE,
        data: layout::KERNEL_DATA,
        user_data: layout::USER_DATA,
        user_code: layout::USER_CODE,
        tss: layout::TSS,
    }
}

/// Builds the tables of `cpu` and loads them on the calling CPU, which must
/// be that same CPU, with interrupts disabled.
pub fn init_for(cpu: &'static Cpu) {
    // SAFETY: startup of this very CPU, without interrupts.
    let t = unsafe { cpu.tables() };
    let stack_top = t.double_fault_stack.0.as_ptr() as u64 + DOUBLE_FAULT_STACK_SIZE as u64;
    t.tss.ist[(DOUBLE_FAULT_IST - 1) as usize] = stack_top;
    let tss_addr = &t.tss as *const Tss as u64;
    let code = t.gdt.push(SegmentDescriptor::code64(0));
    let data = t.gdt.push(SegmentDescriptor::data(0));
    let user_data = t.gdt.push(SegmentDescriptor::data(3));
    let user_code = t.gdt.push(SegmentDescriptor::code64(3));
    let tss = t.gdt.push_tss(TssDescriptor::new(tss_addr));
    let sel = selectors();
    assert_eq!(code, sel.code);
    assert_eq!(data, sel.data);
    assert_eq!(user_data, sel.user_data);
    assert_eq!(user_code, sel.user_code);
    assert_eq!(tss, sel.tss);
    // SAFETY: the GDT and TSS are static, already built and never move.
    unsafe { load(t.gdt.pointer(), sel.code, sel.data, sel.tss) };
}

/// Stack this CPU will use when entering ring 0 from ring 3 through an
/// interrupt or exception (`TSS.rsp0`) and through `syscall`. Updated on
/// every context switch, with interrupts disabled.
pub fn set_kernel_stack(top: u64) {
    let cpu = crate::cpu::current();
    // SAFETY: called with IF = 0 from this very CPU (context switch).
    unsafe { cpu.tables().tss.rsp[0] = top };
    cpu.set_kernel_rsp(top);
}

/// Loads the GDT and reloads all segment selectors and TR.
///
/// # Safety
///
/// `ptr` must describe a valid GDT that contains `code`, `data` and `tss`
/// and stays in memory while the CPU uses it.
unsafe fn load(
    ptr: aricode_x86_cpu::DescriptorTablePointer,
    code: Selector,
    data: Selector,
    tss: Selector,
) {
    asm!("lgdt [{}]", in(reg) &ptr, options(nostack, preserves_flags));
    // CS can only be reloaded with a far return.
    asm!(
        "push {code}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",
        code = in(reg) code.0 as u64,
        tmp = lateout(reg) _,
        options(preserves_flags),
    );
    asm!(
        "mov ds, {0:x}",
        "mov es, {0:x}",
        "mov ss, {0:x}",
        "mov fs, {1:x}",
        "mov gs, {1:x}",
        in(reg) data.0,
        in(reg) 0u16,
        options(nostack, preserves_flags),
    );
    asm!("ltr {0:x}", in(reg) tss.0, options(nostack, preserves_flags));
}
