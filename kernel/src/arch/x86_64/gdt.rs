//! GDT y TSS por CPU (con pila dedicada para double fault). El layout de
//! selectores es el mismo en todas las CPUs (`aricode_x86_cpu::layout`).

use crate::cpu::Cpu;
use aricode_x86_cpu::{layout, Gdt, SegmentDescriptor, Selector, Tss, TssDescriptor};
use core::arch::asm;

/// Índice IST (1..=7) usado por #DF. IST1 = `Tss::ist[0]`.
pub const DOUBLE_FAULT_IST: u8 = 1;
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// Tablas que la CPU lee por su dirección: viven en el `Cpu` estático de
/// cada una y no se mueven.
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

/// Construye las tablas de `cpu` y las carga en la CPU que llama, que debe
/// ser esa misma, con interrupciones deshabilitadas.
pub fn init_for(cpu: &'static Cpu) {
    // SAFETY: arranque de la propia CPU, sin interrupciones.
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
    // SAFETY: la GDT y el TSS son estáticos, ya construidos y nunca se mueven.
    unsafe { load(t.gdt.pointer(), sel.code, sel.data, sel.tss) };
}

/// Pila que usará esta CPU al entrar en ring 0 desde ring 3 por interrupción
/// o excepción (`TSS.rsp0`) y por `syscall`. Se actualiza en cada cambio de
/// contexto, con interrupciones deshabilitadas.
pub fn set_kernel_stack(top: u64) {
    let cpu = crate::cpu::current();
    // SAFETY: llamado con IF = 0 desde la propia CPU (cambio de contexto).
    unsafe { cpu.tables().tss.rsp[0] = top };
    cpu.set_kernel_rsp(top);
}

/// Carga la GDT y recarga todos los selectores de segmento y el TR.
///
/// # Safety
///
/// `ptr` debe describir una GDT válida que contenga `code`, `data` y `tss`
/// y que permanezca en memoria mientras la CPU la use.
unsafe fn load(
    ptr: aricode_x86_cpu::DescriptorTablePointer,
    code: Selector,
    data: Selector,
    tss: Selector,
) {
    asm!("lgdt [{}]", in(reg) &ptr, options(nostack, preserves_flags));
    // CS solo se recarga con un far return.
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
