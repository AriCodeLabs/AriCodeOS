// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! x87, SSE and AVX state across context switches (ADR 0065). `arg`:
//!
//! - 1..=15: instance `arg`. Checks that the process starts with clean
//!   registers (nothing left by an earlier process) and an ABI-aligned
//!   stack, then loads registers with a pattern of its own (different in
//!   every round), yields, sleeps or spins until preempted, and checks it
//!   got them back, `ROUNDS` times. Several instances at once each find
//!   only their own pattern. Exits with 0.
//! - 17..=31: the same as instance `arg - 16`, but every round sleeps.
//!   With fewer of them than CPUs, CPUs go idle and threads wake on
//!   another CPU, then come back to one that ran only its idle thread
//!   meanwhile: the case where skipping the restore would be wrong.
//! - 100: divides by zero with the SIMD exception unmasked (`#XM`); exits
//!   with 31 if the CPU only set the flag (QEMU's TCG does not raise it).
//! - 101: divides by zero with the x87 exception unmasked (`#MF`).
//!
//! `_start` is assembly: it saves the registers before any compiled code
//! can touch them.
#![no_std]
#![no_main]

use aricode_rt::sys;
use core::arch::{asm, global_asm, x86_64::__cpuid_count};

/// Bytes of an area; the components this kernel enables fit in 1 KiB.
const AREA: usize = 4096;
const ROUNDS: u32 = 300;

#[repr(C, align(64))]
struct Area([u8; AREA]);

// Legacy area (FXSAVE layout, also the start of an XSAVE one).
const FCW: usize = 0;
const FTW: usize = 4;
const MXCSR: usize = 24;
const MXCSR_MASK: usize = 28;
const ST0: usize = 32;
const XMM0: usize = 160;
const XSTATE_BV: usize = 512;
const XSTATE_AVX: u64 = 1 << 2;

static mut INITIAL: Area = Area([0; AREA]);
/// 1 if `_start` used `XSAVE` (the kernel enabled it), 0 for `FXSAVE`.
static mut INITIAL_XSAVE: u8 = 0;
/// `rsp` at `_start`'s first instruction.
static mut ENTRY_RSP: u64 = 0;
static mut EXPECTED: Area = Area([0; AREA]);
static mut OBSERVED: Area = Area([0; AREA]);

global_asm!(
    r#"
.section .text._start, "ax"
.global _start
_start:
    mov [rip + {rsp}], rsp
    mov r12, rdi
    mov eax, 1
    cpuid
    bt ecx, 27
    jnc 2f
    xor ecx, ecx
    xgetbv
    xsave64 [rip + {area}]
    mov byte ptr [rip + {flag}], 1
    jmp 3f
2:
    fxsave64 [rip + {area}]
3:
    mov rdi, r12
    jmp {main}
"#,
    rsp = sym ENTRY_RSP,
    area = sym INITIAL,
    flag = sym INITIAL_XSAVE,
    main = sym fpu_main,
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    sys::print(b"fpu_probe: panic\n");
    sys::process_exit(aricode_rt::PANIC_EXIT_CODE)
}

fn fail(k: u64, what: &[u8], code: u64) -> ! {
    let mut b = [0u8; 20];
    sys::print(b"fpu_probe ");
    sys::print(aricode_rt::fmt_u64(k, &mut b));
    sys::print(b": ");
    sys::print(what);
    sys::print(b"\n");
    sys::process_exit(code)
}

/// `XCR0` when the kernel enabled `XSAVE`, else 0.
fn xcr0() -> u64 {
    // SAFETY: plain reads of `_start`'s results; one thread.
    if unsafe { INITIAL_XSAVE } == 0 {
        return 0;
    }
    let (lo, hi): (u32, u32);
    // SAFETY: XGETBV with ECX = 0 is allowed in ring 3 once CR4.OSXSAVE is
    // set, which `_start` checked.
    unsafe { asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    u64::from(lo) | (u64::from(hi) << 32)
}

/// Offset and size of the AVX upper halves in a standard XSAVE area.
fn avx_slot() -> (usize, usize) {
    let leaf = __cpuid_count(0xD, 2);
    (leaf.ebx as usize, leaf.eax as usize)
}

/// How a round gives the CPU away while its pattern is in the registers.
#[derive(Clone, Copy)]
enum Wait {
    /// `thread_yield`: straight to another ready thread.
    Yield,
    /// `thread_sleep(1)`: through the idle thread, and possibly onto
    /// another CPU.
    Sleep,
    /// No system call: spins for `SPIN_CYCLES` TSC cycles, so the timer
    /// preempts it in the middle of the round.
    Spin,
}

/// First instance number whose rounds all sleep.
const SLEEPER: u64 = 17;

/// About 50 ms at TCG's TSC rate: longer than a quantum (two 10 ms ticks).
const SPIN_CYCLES: u64 = 100_000_000;

/// Loads `exp`, gives the CPU away as `wait` says, saves into `obs`, and
/// loads the clean `initial` again before compiled code runs.
///
/// # Safety
///
/// The three areas are 64-byte aligned, valid for the mechanism in use.
unsafe fn round_trip(exp: *const u8, obs: *mut u8, initial: *const u8, mask: u64, wait: Wait) {
    let (number, ms, spin) = match wait {
        Wait::Yield => (sys::abi::nr::THREAD_YIELD, 0u64, 0u64),
        Wait::Sleep => (sys::abi::nr::THREAD_SLEEP, 1, 0),
        Wait::Spin => (0, 0, SPIN_CYCLES),
    };
    let (lo, hi) = (mask as u32, (mask >> 32) as u32);
    if mask == 0 {
        asm!(
            "fxrstor64 [{exp}]",
            "mov rax, {number}",
            "test {spin}, {spin}",
            "jnz 2f",
            "syscall",
            "jmp 4f",
            "2:",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "mov r8, rax",
            "3:",
            "pause",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "sub rax, r8",
            "cmp rax, {spin}",
            "jb 3b",
            "4:",
            "fxsave64 [{obs}]",
            "fxrstor64 [{init}]",
            exp = in(reg) exp,
            obs = in(reg) obs,
            init = in(reg) initial,
            number = in(reg) number,
            spin = in(reg) spin,
            out("rax") _, out("rdx") _, out("r8") _,
            inout("rdi") ms => _,
            out("rcx") _, out("r11") _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
            out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack)
        );
    } else {
        asm!(
            "mov eax, {lo:e}",
            "mov edx, {hi:e}",
            "xrstor64 [{exp}]",
            "mov rax, {number}",
            "test {spin}, {spin}",
            "jnz 2f",
            "syscall",
            "jmp 4f",
            "2:",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "mov r8, rax",
            "3:",
            "pause",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "sub rax, r8",
            "cmp rax, {spin}",
            "jb 3b",
            "4:",
            "mov eax, {lo:e}",
            "mov edx, {hi:e}",
            "xsave64 [{obs}]",
            "xrstor64 [{init}]",
            lo = in(reg) lo,
            hi = in(reg) hi,
            number = in(reg) number,
            spin = in(reg) spin,
            exp = in(reg) exp,
            obs = in(reg) obs,
            init = in(reg) initial,
            out("rax") _, out("rdx") _, out("r8") _,
            inout("rdi") ms => _,
            out("rcx") _, out("r11") _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
            out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack)
        );
    }
}

fn u16_at(a: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([a[o], a[o + 1]])
}

fn u32_at(a: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(a[o..o + 4].try_into().expect("4 bytes"))
}

/// A new process: x87 after `FNINIT`, MXCSR at reset, every data
/// register zero, and `rsp + 8` aligned to 16. With `XSAVE`, a component
/// whose `XSTATE_BV` bit is clear is in its init state by definition, and
/// its bytes may not have been written.
fn check_initial(k: u64, initial: &[u8], mask: u64) {
    // SAFETY: written once by `_start`.
    if unsafe { ENTRY_RSP } % 16 != 8 {
        fail(k, b"entry stack not aligned as after a call", 10);
    }
    let in_use = |bit: u64| {
        mask == 0
            || u64::from_le_bytes(initial[XSTATE_BV..XSTATE_BV + 8].try_into().expect("8")) & bit
                != 0
    };
    if u32_at(initial, MXCSR) != 0x1F80 {
        fail(k, b"initial MXCSR is not 0x1f80", 12);
    }
    if in_use(1) {
        if u16_at(initial, FCW) != 0x037F {
            fail(k, b"initial x87 control word is not 0x037f", 11);
        }
        if initial[FTW] != 0 || initial[ST0..XMM0].iter().any(|&b| b != 0) {
            fail(k, b"x87 registers left by another process", 13);
        }
    }
    if in_use(2) && initial[XMM0..XMM0 + 256].iter().any(|&b| b != 0) {
        fail(k, b"XMM registers left by another process", 14);
    }
    if mask & XSTATE_AVX != 0 && in_use(XSTATE_AVX) {
        let (off, len) = avx_slot();
        if initial[off..off + len].iter().any(|&b| b != 0) {
            fail(k, b"YMM upper halves left by another process", 15);
        }
    }
}

/// The pattern of instance `k` in round `round`: x87 precision and data
/// registers, MXCSR rounding mode and flush-to-zero, every XMM byte and,
/// with AVX, every YMM upper byte. No two (instance, round) pairs share
/// register contents (audit of ADR 0065, finding 1): registers left from
/// an earlier round, or from another instance, never match.
fn fill_pattern(k: u64, round: u32, exp: &mut [u8], initial: &[u8], mask: u64) {
    exp[..].copy_from_slice(initial);
    // A bijection of `k * 1000 + round` (rounds < 1000): an odd multiplier
    // is invertible modulo 2^32, and every 4 consecutive bytes carry all
    // of it.
    let h = (k as u32 * 1000 + round).wrapping_mul(0x9E37_79B1);
    let byte = |i: usize, salt: u8| (h >> ((i % 4) * 8)) as u8 ^ i as u8 ^ salt;
    let v = k as u32 + round;
    let precision = [0u16, 2, 3][(v % 3) as usize] << 8;
    exp[FCW..FCW + 2].copy_from_slice(&((0x037F & !0x0300) | precision).to_le_bytes());
    // Only bits MXCSR_MASK allows (0 means the architectural default).
    let allowed = match u32_at(initial, MXCSR_MASK) {
        0 => 0xFFBF,
        m => m,
    };
    let mxcsr = (0x1F80 | ((v & 3) << 13) | ((v & 1) << 15)) & allowed;
    exp[MXCSR..MXCSR + 4].copy_from_slice(&mxcsr.to_le_bytes());
    exp[FTW] = 0xFF;
    for r in 0..8 {
        for b in 0..10 {
            exp[ST0 + r * 16 + b] = byte(r * 16 + b, 0x3C);
        }
    }
    for (i, b) in exp[XMM0..XMM0 + 256].iter_mut().enumerate() {
        *b = byte(i, 0xA5);
    }
    if mask != 0 {
        let mut bv = u64::from_le_bytes(exp[XSTATE_BV..XSTATE_BV + 8].try_into().expect("8"));
        bv |= mask & 0b111;
        exp[XSTATE_BV..XSTATE_BV + 8].copy_from_slice(&bv.to_le_bytes());
        if mask & XSTATE_AVX != 0 {
            let (off, len) = avx_slot();
            for (i, b) in exp[off..off + len].iter_mut().enumerate() {
                *b = byte(i, 0x5A);
            }
        }
    }
}

/// What a round must give back: control words, x87 data registers and
/// tags, XMM and YMM upper halves.
fn compare(k: u64, round: u32, exp: &[u8], obs: &[u8], mask: u64) {
    let mut bad: Option<&[u8]> = None;
    if u16_at(exp, FCW) != u16_at(obs, FCW) {
        bad = Some(b"x87 control word changed");
    } else if u32_at(exp, MXCSR) != u32_at(obs, MXCSR) {
        bad = Some(b"MXCSR changed");
    } else if exp[FTW] != obs[FTW]
        || (0..8)
            .any(|r| exp[ST0 + r * 16..ST0 + r * 16 + 10] != obs[ST0 + r * 16..ST0 + r * 16 + 10])
    {
        bad = Some(b"x87 registers changed");
    } else if exp[XMM0..XMM0 + 256] != obs[XMM0..XMM0 + 256] {
        bad = Some(b"XMM registers changed");
    } else if mask & XSTATE_AVX != 0 {
        let (off, len) = avx_slot();
        if exp[off..off + len] != obs[off..off + len] {
            bad = Some(b"YMM upper halves changed");
        }
    }
    if let Some(what) = bad {
        let mut b = [0u8; 20];
        sys::print(b"fpu_probe: round ");
        sys::print(aricode_rt::fmt_u64(u64::from(round), &mut b));
        sys::print(b"\n");
        fail(k, what, 20);
    }
}

/// Unmasks one exception and divides by zero: the kernel ends only this
/// process.
fn divide_by_zero(x87: bool) -> ! {
    let one = 1.0f32;
    let zero = 0.0f32;
    if x87 {
        // Unmask ZM in the control word, then FDIV and FWAIT raise #MF.
        let cw: u16 = 0x037F & !(1 << 2);
        // SAFETY: deliberate fault; the process ends.
        unsafe {
            asm!(
                "fldcw [{cw}]",
                "fld dword ptr [{one}]",
                "fdiv dword ptr [{zero}]",
                "fwait",
                cw = in(reg) &cw,
                one = in(reg) &one,
                zero = in(reg) &zero,
                options(nostack)
            )
        };
    } else {
        // Unmask ZM in MXCSR, then DIVSS raises #XM.
        let csr: u32 = 0x1F80 & !(1 << 9);
        // SAFETY: deliberate fault; the process ends.
        unsafe {
            asm!(
                "ldmxcsr [{csr}]",
                "movss xmm0, [{one}]",
                "divss xmm0, [{zero}]",
                csr = in(reg) &csr,
                one = in(reg) &one,
                zero = in(reg) &zero,
                out("xmm0") _,
                options(nostack)
            )
        };
    }
    // No fault: show the status the CPU recorded instead.
    let status: u32 = if x87 {
        let sw: u16;
        // SAFETY: reads the x87 status word.
        unsafe { asm!("fnstsw ax", out("ax") sw, options(nomem, nostack)) };
        u32::from(sw)
    } else {
        let mut csr = 0u32;
        // SAFETY: stores MXCSR into a local.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack)) };
        csr
    };
    let mut b = [0u8; 16];
    sys::print(if x87 {
        b"fpu_probe: no #MF, x87 status word 0x" as &[u8]
    } else {
        b"fpu_probe: no #XM, MXCSR 0x"
    });
    sys::print(aricode_rt::fmt_hex(u64::from(status), 4, &mut b));
    sys::print(b"\n");
    // QEMU's TCG records the SIMD exception (ZE, bit 2) without raising
    // #XM; the kernel's self-test accepts that one case and says so.
    if !x87 && status & (1 << 2) != 0 {
        sys::process_exit(NO_XM)
    }
    fail(0, b"division by zero did not fault", 30)
}

/// Exit code: ZE set in MXCSR with the exception unmasked, but no #XM.
const NO_XM: u64 = 31;

extern "C" fn fpu_main(k: u64) -> ! {
    match k {
        100 => divide_by_zero(false),
        101 => divide_by_zero(true),
        1..=15 | SLEEPER..=31 => {}
        _ => fail(k, b"unknown mode", 2),
    }
    let mask = xcr0();
    // SAFETY: one thread; the statics are only used here and by `_start`,
    // which has finished.
    let (initial, exp, obs) = unsafe {
        (
            &(*core::ptr::addr_of!(INITIAL)).0,
            &mut (*core::ptr::addr_of_mut!(EXPECTED)).0,
            &mut (*core::ptr::addr_of_mut!(OBSERVED)).0,
        )
    };
    check_initial(k, initial, mask);
    for round in 0..ROUNDS {
        fill_pattern(k, round, exp, initial, mask);
        let wait = match round % 50 {
            _ if k >= SLEEPER => Wait::Sleep,
            49 => Wait::Spin,
            r if r % 16 == 15 => Wait::Sleep,
            _ => Wait::Yield,
        };
        obs.fill(0);
        // SAFETY: 64-byte aligned areas of the mechanism `_start` found.
        unsafe { round_trip(exp.as_ptr(), obs.as_mut_ptr(), initial.as_ptr(), mask, wait) };
        compare(k, round, exp, obs, mask);
    }
    let mut b = [0u8; 20];
    sys::print(b"fpu_probe ");
    sys::print(aricode_rt::fmt_u64(k, &mut b));
    sys::print(if mask == 0 {
        b": fxsave" as &[u8]
    } else {
        b": xsave"
    });
    sys::print(b" state kept over ");
    sys::print(aricode_rt::fmt_u64(u64::from(ROUNDS), &mut b));
    sys::print(b" switches\n");
    sys::process_exit(0)
}
