//! GDT propia del kernel y TSS con pila dedicada para double fault.

use crate::sync::IrqLock;
use aricode_x86_cpu::{layout, Gdt, SegmentDescriptor, Selector, Tss, TssDescriptor};
use core::arch::asm;

/// Índice IST (1..=7) usado por #DF. IST1 = `Tss::ist[0]`.
pub const DOUBLE_FAULT_IST: u8 = 1;
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

// Estas estructuras las lee la CPU directamente por su dirección: el lock solo
// serializa la construcción; después no se modifican y no se mueven.
static DOUBLE_FAULT_STACK: IrqLock<Stack> = IrqLock::new(Stack([0; DOUBLE_FAULT_STACK_SIZE]));
static TSS: IrqLock<Tss> = IrqLock::new(Tss::new());
static GDT: IrqLock<Gdt<8>> = IrqLock::new(Gdt::new());

#[derive(Debug, Clone, Copy)]
pub struct Selectors {
    pub code: Selector,
    pub data: Selector,
    pub user_data: Selector,
    pub user_code: Selector,
    pub tss: Selector,
}

static SELECTORS: IrqLock<Selectors> = IrqLock::new(Selectors {
    code: Selector::NULL,
    data: Selector::NULL,
    user_data: Selector::NULL,
    user_code: Selector::NULL,
    tss: Selector::NULL,
});

pub fn selectors() -> Selectors {
    *SELECTORS.lock()
}

pub fn init() {
    let stack_top = {
        let stack = DOUBLE_FAULT_STACK.lock();
        stack.0.as_ptr() as u64 + DOUBLE_FAULT_STACK_SIZE as u64
    };
    let tss_addr = {
        let mut tss = TSS.lock();
        tss.ist[(DOUBLE_FAULT_IST - 1) as usize] = stack_top;
        &*tss as *const Tss as u64
    };
    let (ptr, sel) = {
        let mut gdt = GDT.lock();
        let code = gdt.push(SegmentDescriptor::code64(0));
        let data = gdt.push(SegmentDescriptor::data(0));
        let user_data = gdt.push(SegmentDescriptor::data(3));
        let user_code = gdt.push(SegmentDescriptor::code64(3));
        let tss = gdt.push_tss(TssDescriptor::new(tss_addr));
        (
            gdt.pointer(),
            Selectors {
                code,
                data,
                user_data,
                user_code,
                tss,
            },
        )
    };
    assert_eq!(sel.code, layout::KERNEL_CODE);
    assert_eq!(sel.user_data, layout::USER_DATA);
    assert_eq!(sel.user_code, layout::USER_CODE);
    assert_eq!(sel.tss, layout::TSS);
    *SELECTORS.lock() = sel;
    // SAFETY: la GDT y el TSS son estáticos, ya construidos y nunca se mueven.
    unsafe { load(ptr, sel.code, sel.data, sel.tss) };
}

/// Pila que usará la CPU al entrar en ring 0 desde ring 3 por interrupción
/// o excepción (`TSS.rsp0`). Se actualiza en cada cambio de contexto.
pub fn set_kernel_stack(top: u64) {
    TSS.lock().rsp[0] = top;
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
