//! GDT propia del kernel y TSS con pila dedicada para double fault.

use crate::sync::SingleCpu;
use aricode_x86_cpu::{Gdt, SegmentDescriptor, Selector, Tss, TssDescriptor};
use core::arch::asm;

/// Índice IST (1..=7) usado por #DF. IST1 = `Tss::ist[0]`.
pub const DOUBLE_FAULT_IST: u8 = 1;
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

static DOUBLE_FAULT_STACK: SingleCpu<Stack> = SingleCpu::new(Stack([0; DOUBLE_FAULT_STACK_SIZE]));
static TSS: SingleCpu<Tss> = SingleCpu::new(Tss::new());
static GDT: SingleCpu<Gdt<8>> = SingleCpu::new(Gdt::new());

pub struct Selectors {
    pub code: Selector,
    pub data: Selector,
    pub tss: Selector,
}

static SELECTORS: SingleCpu<Selectors> = SingleCpu::new(Selectors {
    code: Selector::NULL,
    data: Selector::NULL,
    tss: Selector::NULL,
});

pub fn selectors() -> &'static Selectors {
    // SAFETY: solo lectura tras `init`; ver `SingleCpu`.
    unsafe { &*SELECTORS.get() }
}

pub fn init() {
    // SAFETY: una CPU, interrupciones deshabilitadas, llamada única (ver `SingleCpu`).
    unsafe {
        let stack_top = DOUBLE_FAULT_STACK.get() as u64 + DOUBLE_FAULT_STACK_SIZE as u64;
        (*TSS.get()).ist[(DOUBLE_FAULT_IST - 1) as usize] = stack_top;

        let gdt = &mut *GDT.get();
        let code = gdt.push(SegmentDescriptor::code64(0));
        let data = gdt.push(SegmentDescriptor::data(0));
        let tss = gdt.push_tss(TssDescriptor::new(TSS.get() as u64));
        *SELECTORS.get() = Selectors { code, data, tss };

        load(gdt.pointer(), code, data, tss);
    }
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
