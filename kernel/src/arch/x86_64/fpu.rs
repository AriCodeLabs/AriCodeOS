// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! x87, SSE and AVX state of user threads (ADR 0065), saved eagerly on
//! every switch away from a user thread and restored before one runs.
//!
//! - The kernel is built soft-float (`x86_64-unknown-none`) and never
//!   touches these registers, so they hold the last user thread's state
//!   while the kernel runs, and kernel threads need no area.
//! - Nothing is lazy: `CR0.TS` stays clear and `#NM` never happens. Lazy
//!   switching let one process read another's registers through
//!   speculation (CVE-2018-3665, LazyFP).
//! - A CPU that still holds a thread's registers (the thread ran last
//!   there and only kernel threads ran since) skips the restore, as Linux
//!   does. The save is never skipped: the thread may run elsewhere next.
//! - `XSAVE` when the CPU has it, with x87, SSE and AVX enabled in `XCR0`;
//!   `FXSAVE` otherwise. AVX-512 and AMX stay disabled.
//! - AMD and Hygon processors without `XSAVEERPTR` save the x87 error pointers only
//!   when an exception is pending, so the next thread would see the last
//!   one's: they are reset to a fixed value before each restore, as Linux
//!   does (`X86_BUG_FXSAVE_LEAK`, CVE-2006-1056).

use core::alloc::Layout;
use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR0_NE: u64 = 1 << 5;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;
const CR4_OSXSAVE: u64 = 1 << 18;

/// `XCR0` components AriCodeOS enables when the CPU has them.
const XCR0_X87: u64 = 1 << 0;
const XCR0_SSE: u64 = 1 << 1;
const XCR0_AVX: u64 = 1 << 2;
const XCR0_WANTED: u64 = XCR0_X87 | XCR0_SSE | XCR0_AVX;

/// Size of the `FXSAVE` area, and of the legacy part of an `XSAVE` one.
const LEGACY_SIZE: usize = 512;
/// `XSAVE` header that follows the legacy area.
const XSAVE_HEADER: usize = 64;
/// x87 control word after `FNINIT`: every exception masked, 64-bit precision.
const FCW_INIT: u16 = 0x037F;
/// MXCSR at reset: every SIMD exception masked, round to nearest.
const MXCSR_INIT: u32 = 0x1F80;

/// `XCR0` value in use; 0 means `FXSAVE`.
static XSAVE_MASK: AtomicU64 = AtomicU64::new(0);
/// `XSAVEOPT` is available (only meaningful with `XSAVE`).
static XSAVEOPT: AtomicBool = AtomicBool::new(false);
/// Bytes of one thread's area.
static AREA_SIZE: AtomicUsize = AtomicUsize::new(LEGACY_SIZE);
/// The x87 error pointers must be reset before a restore.
static ERROR_POINTER_LEAK: AtomicBool = AtomicBool::new(false);

/// No thread's registers are loaded on that CPU.
const NO_OWNER: u32 = 0;
/// Per CPU: the user thread whose registers it holds.
static OWNER: [AtomicU32; crate::cpu::MAX_CPUS] =
    [const { AtomicU32::new(NO_OWNER) }; crate::cpu::MAX_CPUS];

/// What `init_bsp` found, for the boot log.
pub struct Summary {
    pub xsave_mask: u64,
    pub area_size: usize,
    pub error_pointer_leak: bool,
}

/// Chooses the save mechanism and enables it on the BSP. Called once,
/// before any user thread exists.
pub fn init_bsp() -> Summary {
    let (leaf1, max_leaf) = (__cpuid(1), __cpuid(0).eax);
    let has_xsave = leaf1.ecx & (1 << 26) != 0;
    let mask = if has_xsave && max_leaf >= 0xD {
        let supported = __cpuid_count(0xD, 0);
        let supported = (supported.eax as u64) | ((supported.edx as u64) << 32);
        let mut mask = supported & (XCR0_X87 | XCR0_SSE);
        // AVX needs its CPUID bit as well as the component.
        if leaf1.ecx & (1 << 28) != 0 {
            mask |= supported & XCR0_AVX;
        }
        mask
    } else {
        0
    };
    if mask & (XCR0_X87 | XCR0_SSE) == XCR0_X87 | XCR0_SSE {
        XSAVE_MASK.store(mask & XCR0_WANTED, Ordering::Relaxed);
        let opt = __cpuid_count(0xD, 1).eax & 1 != 0;
        XSAVEOPT.store(opt, Ordering::Relaxed);
    }
    ERROR_POINTER_LEAK.store(amd_error_pointer_leak(), Ordering::Relaxed);
    init_cpu();
    if XSAVE_MASK.load(Ordering::Relaxed) != 0 {
        // Leaf 0xD, sub-leaf 0: EBX is the size the components now
        // enabled in XCR0 need.
        let size = __cpuid_count(0xD, 0).ebx as usize;
        AREA_SIZE.store(size.max(LEGACY_SIZE + XSAVE_HEADER), Ordering::Relaxed);
    }
    Summary {
        xsave_mask: XSAVE_MASK.load(Ordering::Relaxed),
        area_size: AREA_SIZE.load(Ordering::Relaxed),
        error_pointer_leak: ERROR_POINTER_LEAK.load(Ordering::Relaxed),
    }
}

/// Enables x87, SSE and, when chosen, `XSAVE` on the calling CPU, with the
/// BSP's choice. Each AP calls it before it can run a user thread.
pub fn init_cpu() {
    let mask = XSAVE_MASK.load(Ordering::Relaxed);
    // SAFETY: CR0/CR4 updates that only enable the FPU, SSE and XSAVE the
    // CPU reported; XCR0 holds components CPUID listed as supported.
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 = (cr0 & !(CR0_EM | CR0_TS)) | CR0_MP | CR0_NE;
        asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags));
        let mut cr4: u64;
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
        if mask != 0 {
            cr4 |= CR4_OSXSAVE;
        }
        asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
        if mask != 0 {
            asm!(
                "xsetbv",
                in("ecx") 0u32,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nomem, nostack, preserves_flags)
            );
        }
        asm!("fninit", options(nomem, nostack, preserves_flags));
    }
}

/// AMD processors, and Hygon's Zen-derived ones, leak the x87 error
/// pointers through `FXSAVE`/`XSAVE` unless they report `XSAVEERPTR`
/// (CPUID 0x8000_0008, EBX bit 2).
fn amd_error_pointer_leak() -> bool {
    // "AuthenticAMD" or "HygonGenuine" in EBX, EDX, ECX.
    let v = __cpuid(0);
    let vendor = (v.ebx, v.edx, v.ecx);
    if vendor != (0x6874_7541, 0x6974_6E65, 0x444D_4163)
        && vendor != (0x6F67_7948, 0x6E65_476E, 0x656E_6975)
    {
        return false;
    }
    let max_ext = __cpuid(0x8000_0000).eax;
    max_ext < 0x8000_0008 || __cpuid(0x8000_0008).ebx & (1 << 2) == 0
}

/// One user thread's saved registers, in the format of the instruction in
/// use, aligned for `XSAVE`.
pub struct FpuArea {
    ptr: NonNull<u8>,
    layout: Layout,
    /// CPU index whose registers were last loaded from this area.
    last_cpu: usize,
}

// SAFETY: the area is plain memory owned by one thread and only touched
// with the scheduler lock held.
unsafe impl Send for FpuArea {}

impl FpuArea {
    /// The state of a new thread: x87 after `FNINIT`, MXCSR at reset,
    /// every vector register zero.
    pub fn new() -> Result<Self, &'static str> {
        let layout = Layout::from_size_align(AREA_SIZE.load(Ordering::Relaxed), 64)
            .map_err(|_| "FPU area layout")?;
        // SAFETY: non-zero size.
        let raw = unsafe { alloc::alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).ok_or("no memory for the FPU area")?;
        // SAFETY: the area holds at least the 512-byte legacy region; the
        // XSAVE header stays zero, so every component starts in its init
        // state.
        unsafe {
            ptr.as_ptr().cast::<u16>().write(FCW_INIT);
            ptr.as_ptr().add(24).cast::<u32>().write(MXCSR_INIT);
        }
        Ok(Self {
            ptr,
            layout,
            last_cpu: usize::MAX,
        })
    }
}

impl Drop for FpuArea {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { alloc::alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

/// Saves the registers of the user thread leaving the CPU into `prev`.
///
/// # Safety
///
/// The registers must belong to `prev`'s thread: it is the one running
/// on this CPU, and kernel code does not touch them.
pub unsafe fn save(prev: &mut FpuArea) {
    let mask = XSAVE_MASK.load(Ordering::Relaxed);
    let p = prev.ptr.as_ptr();
    if mask == 0 {
        asm!("fxsave64 [{}]", in(reg) p, options(nostack, preserves_flags));
    } else if XSAVEOPT.load(Ordering::Relaxed) {
        asm!(
            "xsaveopt64 [{}]",
            in(reg) p,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack, preserves_flags)
        );
    } else {
        asm!(
            "xsave64 [{}]",
            in(reg) p,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack, preserves_flags)
        );
    }
}

/// Loads the registers of user thread `tid`, about to run on CPU `cpu`,
/// unless this CPU already holds them.
///
/// # Safety
///
/// `next` must be `tid`'s area; interrupts disabled, scheduler lock held.
pub unsafe fn restore(next: &mut FpuArea, tid: u32, cpu: usize) {
    let owner = &OWNER[cpu];
    if owner.load(Ordering::Relaxed) == tid && next.last_cpu == cpu {
        return;
    }
    let p = next.ptr.as_ptr();
    if ERROR_POINTER_LEAK.load(Ordering::Relaxed) {
        // Linux's sequence: clear pending exceptions and the tag word,
        // then a harmless x87 load sets the pointers to a fixed value
        // the restore does not overwrite.
        asm!(
            "fnclex",
            "emms",
            "fild dword ptr [{}]",
            in(reg) p,
            options(nostack, preserves_flags)
        );
    }
    let mask = XSAVE_MASK.load(Ordering::Relaxed);
    if mask == 0 {
        asm!("fxrstor64 [{}]", in(reg) p, options(nostack, preserves_flags));
    } else {
        asm!(
            "xrstor64 [{}]",
            in(reg) p,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack, preserves_flags)
        );
    }
    next.last_cpu = cpu;
    owner.store(tid, Ordering::Relaxed);
}

/// `test=fpu` (ADR 0065): instances of `fpu_probe` run at once, each
/// checking that it starts with clean registers and keeps its own x87,
/// SSE and AVX state over hundreds of switches; then an x87 and a SIMD
/// division by zero, unmasked, end only their process.
pub fn selftest() {
    use crate::user::process::{self, ExitStatus};
    let code = crate::user::programs::by_name("fpu_probe").expect("fpu_probe in the initrd");
    let cpus = crate::cpu::online_count();
    let instances: u64 = if cpus > 1 { 6 } else { 3 };
    let pids: alloc::vec::Vec<_> = (1..=instances)
        .map(|k| {
            process::spawn_with_handles_and_arg("fpu_probe", code, alloc::vec::Vec::new(), k, 0)
                .expect("spawn fpu_probe")
        })
        .collect();
    for (k, pid) in (1..).zip(pids) {
        let status = process::wait(pid);
        assert_eq!(status, ExitStatus::Exited(0), "fpu_probe {k}");
    }
    // Fewer sleepers than CPUs: CPUs go idle, a woken probe runs on
    // another CPU and later returns to one that ran only its idle thread
    // meanwhile, still holding the probe's older registers. Only the
    // area's `last_cpu` makes that CPU load them again.
    if cpus > 2 {
        let sleepers: alloc::vec::Vec<_> = (17..=18)
            .map(|k| {
                process::spawn_with_handles_and_arg("fpu_probe", code, alloc::vec::Vec::new(), k, 0)
                    .expect("spawn fpu_probe")
            })
            .collect();
        for (k, pid) in (17..).zip(sleepers) {
            assert_eq!(process::wait(pid), ExitStatus::Exited(0), "fpu_probe {k}");
        }
    }
    let spawn = |mode: u64| {
        let pid =
            process::spawn_with_handles_and_arg("fpu_probe", code, alloc::vec::Vec::new(), mode, 0)
                .expect("spawn fpu_probe");
        process::wait(pid)
    };
    assert_eq!(spawn(101), ExitStatus::Killed("#MF"), "fpu_probe 101");
    // QEMU's TCG records an unmasked SIMD exception in MXCSR without
    // raising #XM (`fpu_probe` exits with `NO_XM` after checking the flag);
    // hardware raises it.
    let xm = match spawn(100) {
        ExitStatus::Killed("#XM") => "#XM ended only its process",
        ExitStatus::Exited(NO_XM) => "this CPU set the SIMD flag without raising #XM",
        other => panic!("fpu_probe 100: {other:?}"),
    };
    log::info!(
        "fpu selftest ok ({instances} processes kept their own registers on {cpus} CPU(s); #MF ended only its process; {xm})"
    );
}

/// `fpu_probe`'s exit code when the CPU flagged the unmasked SIMD division
/// by zero in MXCSR but raised no #XM.
const NO_XM: u64 = 31;
