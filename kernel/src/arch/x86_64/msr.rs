//! Registros específicos del modelo.

use core::arch::asm;

pub const IA32_APIC_BASE: u32 = 0x1B;
pub const IA32_EFER: u32 = 0xC000_0080;
pub const IA32_STAR: u32 = 0xC000_0081;
pub const IA32_LSTAR: u32 = 0xC000_0082;
pub const IA32_FMASK: u32 = 0xC000_0084;

pub const EFER_SCE: u64 = 1 << 0;

pub fn read(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: lectura de MSR en ring 0.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}

/// # Safety
/// El llamador conoce el registro y el efecto del valor.
pub unsafe fn write(msr: u32, value: u64) {
    asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack));
}
