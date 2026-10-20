// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Virtual memory manager (Phase 6): the kernel's own page tables.
//!
//! When activated they replace the loader's tables:
//! - the kernel image is mapped per linker-script section with W^X
//!   (.text RX, .rodata R, .data/.bss RW, everything NX except .text);
//! - the linear physical map (`PHYS_MAP_BASE`) is rebuilt RW+NX;
//! - the framebuffer is covered even if it lies outside RAM;
//! - the identity map goes away;
//! - a new stack with a guard page replaces the loader's;
//! - boot memory (`Boot`) goes back to the PMM.

use super::phys;
use super::pmm;
use crate::sync::IrqLock;
use aricode_pmm::{Frame, FRAME_SIZE};
use aricode_x86_paging::{
    FrameAlloc, MapError, Mapper, OffsetMap, GLOBAL, HUGE_2M, NO_EXECUTE, PAGE_SIZE, WRITABLE,
};
use boot_protocol::{BootInfo, MemoryRegionKind, KERNEL_VIRT_BASE};
use log::{debug, info};

/// Main kernel stack: 64 KiB right below the image, with an unmapped guard
/// page below it.
pub const KERNEL_STACK_SIZE: u64 = 64 * 1024;
pub const KERNEL_STACK_TOP: u64 = KERNEL_VIRT_BASE - PAGE_SIZE;
pub const KERNEL_STACK_BOTTOM: u64 = KERNEL_STACK_TOP - KERNEL_STACK_SIZE;
pub const KERNEL_STACK_GUARD: u64 = KERNEL_STACK_BOTTOM - PAGE_SIZE;

/// Frames for page tables from the PMM.
pub struct PmmFrames;

impl FrameAlloc for PmmFrames {
    fn alloc_frame(&mut self) -> Option<u64> {
        pmm::with(|p| p.allocate()).map(Frame::addr)
    }
}

pub type KernelMapper = Mapper<PmmFrames, OffsetMap>;

static VMM: IrqLock<Option<KernelMapper>> = IrqLock::new(None);

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __bss_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// Builds and activates the kernel's own tables. Returns the top of the new stack.
///
/// # Panics
/// If there are not enough frames or the kernel layout is not as expected.
pub fn init(info: &BootInfo) -> u64 {
    let offset = phys::offset();
    let limit = phys::limit();
    let mut m = Mapper::new(PmmFrames, OffsetMap(offset)).expect("vmm: no frames for the PML4");
    // The whole higher half (256 PDPTs, 1 MiB) exists from the start:
    // processes share it and see every future kernel mapping.
    m.preallocate_kernel_half()
        .expect("vmm: no frames for the higher half");

    // Linear physical map.
    m.map_range_2m(offset, 0, limit, WRITABLE | NO_EXECUTE | GLOBAL)
        .expect("vmm: physical map");
    let fb = &info.framebuffer;
    if fb.address != 0 {
        let start = (fb.address & !(HUGE_2M - 1)).max(limit);
        let end = (fb.address + fb.size).next_multiple_of(HUGE_2M);
        if start < end {
            m.map_range_2m(
                offset + start,
                start,
                end - start,
                WRITABLE | NO_EXECUTE | GLOBAL,
            )
            .expect("vmm: framebuffer");
        }
    }

    // Kernel image per section (W^X).
    // SAFETY: symbols defined by kernel.ld; only their address is taken.
    let (text, rodata, data) = unsafe {
        (
            (sym(&__text_start), sym(&__text_end)),
            (sym(&__rodata_start), sym(&__rodata_end)),
            (sym(&__data_start), sym(&__bss_end)),
        )
    };
    let kernel_phys = |virt: u64| info.kernel_phys_base + (virt - KERNEL_VIRT_BASE);
    for (name, (start, end), flags) in [
        (".text", text, GLOBAL),
        (".rodata", rodata, GLOBAL | NO_EXECUTE),
        (".data/.bss", data, GLOBAL | NO_EXECUTE | WRITABLE),
    ] {
        // Start symbols are aligned by the linker script; end symbols mark
        // the last byte used and the next section starts on the next page,
        // so rounding up overlaps nothing.
        assert!(start.is_multiple_of(PAGE_SIZE), "{name} not aligned");
        let end = end.next_multiple_of(PAGE_SIZE);
        m.map_range_4k(start, kernel_phys(start), end - start, flags)
            .unwrap_or_else(|e| panic!("vmm: {name}: {e:?}"));
        debug!("vmm: {name:<11} {start:#x}..{end:#x} {}", describe(flags));
    }

    // New stack with a guard page: the guard is simply not mapped.
    for virt in (KERNEL_STACK_BOTTOM..KERNEL_STACK_TOP).step_by(PAGE_SIZE as usize) {
        let frame = pmm::with(|p| p.allocate()).expect("vmm: no frames for the stack");
        m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL)
            .expect("vmm: stack");
    }
    assert_eq!(
        m.translate(KERNEL_STACK_GUARD),
        None,
        "the guard page must not be mapped"
    );

    let root = m.root_phys();
    // SAFETY: the new tables map the code being executed, the current stack
    // (physical map) and all kernel data; `root` is a complete PML4 built
    // above.
    unsafe { core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags)) };
    *VMM.lock() = Some(m);
    info!(
        "vmm: own tables active (pml4 {root:#x}), W^X on the image, stack {:#x}..{:#x} with guard {:#x}",
        KERNEL_STACK_BOTTOM, KERNEL_STACK_TOP, KERNEL_STACK_GUARD
    );

    reclaim_boot_memory(info);
    KERNEL_STACK_TOP
}

/// Returns the `Boot` regions of the UEFI map (loader, boot services and
/// AriBoot's now inactive tables) to the PMM, except the pages the kernel
/// still uses (`BootInfo::kernel_ranges`: image, initial stack, `BootInfo`,
/// initrd and cmdline), which are reserved again. The memory map buffer
/// itself is freed too and never read again.
fn reclaim_boot_memory(info: &BootInfo) {
    let before = pmm::stats().free_bytes();
    // SAFETY: physical map active (rebuilt above) and buffer intact.
    for d in unsafe { info.memory_regions() }.filter(|d| d.kind() == MemoryRegionKind::Boot) {
        // Low memory stays reserved (SMP trampoline, legacy devices).
        let start = d.phys_start.max(pmm::LOW_RESERVED_END);
        if start < d.end() {
            pmm::with(|p| p.free_range(start, d.end()));
        }
    }
    let mut kept = 0u64;
    for (start, end) in info.kernel_ranges() {
        let end = end.next_multiple_of(PAGE_SIZE);
        pmm::with(|p| p.reserve_range(start, end));
        kept += end.saturating_sub(start);
    }
    let s = pmm::stats();
    info!(
        "vmm: {} MiB of boot memory returned to the PMM ({} KiB kept for the kernel, {} MiB free)",
        s.free_bytes().saturating_sub(before) >> 20,
        kept >> 10,
        s.free_bytes() >> 20
    );
}

fn describe(flags: u64) -> &'static str {
    match (flags & WRITABLE != 0, flags & NO_EXECUTE == 0) {
        (false, true) => "R-X",
        (false, false) => "R--",
        (true, false) => "RW-",
        (true, true) => "RWX",
    }
}

/// Exclusive access to the active tables.
///
/// # Panics
/// If `init` has not run.
pub fn with<R>(f: impl FnOnce(&mut KernelMapper) -> R) -> R {
    let mut guard = VMM.lock();
    f(guard.as_mut().expect("VMM not initialized"))
}

/// Invalidates the translation of `virt` on this CPU and, via IPI, on the
/// others (Phase 32). For kernel-half pages another CPU may have used. Must
/// be called with interrupts enabled and no locks held.
pub fn flush_page(virt: u64) {
    // SAFETY: invalidation of a TLB entry; no memory effects.
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
    crate::arch::x86_64::smp::flush_others(Some(virt));
}

/// Invalidates the user pages `[virt, virt+len)` of the current address
/// space on this CPU and, with one whole-TLB IPI, on the others, before
/// the frames they pointed at may be reused (ADR 0038). Must be called with
/// interrupts enabled and no locks held.
pub fn flush_user_range(virt: u64, len: u64) {
    for page in (virt..virt + len).step_by(PAGE_SIZE as usize) {
        // SAFETY: invalidation of a TLB entry; no memory effects.
        unsafe { core::arch::asm!("invlpg [{}]", in(reg) page, options(nostack, preserves_flags)) };
    }
    crate::arch::x86_64::smp::flush_others(None);
}

/// `true` if `addr` falls in the kernel stack guard page.
pub fn in_stack_guard(addr: u64) -> bool {
    (KERNEL_STACK_GUARD..KERNEL_STACK_BOTTOM).contains(&addr)
}

/// Self-test (`test=vmm`): translations consistent with what was mapped,
/// mapping and unmapping a new page, and expected errors.
pub fn selftest() {
    let _probe_virt = with(|m| {
        // .text is executable and not writable; .data the other way round.
        // SAFETY: only symbol addresses.
        let (text, data) = unsafe { (sym(&__text_start), sym(&__data_start)) };
        let (_, tf) = m.translate(text).expect("text mapped");
        assert!(
            tf & NO_EXECUTE == 0 && tf & WRITABLE == 0,
            ".text must be R-X"
        );
        let (_, df) = m.translate(data).expect("data mapped");
        assert!(
            df & NO_EXECUTE != 0 && df & WRITABLE != 0,
            ".data must be RW-"
        );
        // The physical map translates by offset.
        let probe = 0x20_0000u64;
        assert_eq!(
            m.translate(phys::phys_to_virt(probe)).map(|t| t.0),
            Some(probe)
        );
        // The identity map no longer exists.
        assert_eq!(m.translate(probe), None, "the identity map must be gone");
        assert_eq!(m.translate(KERNEL_STACK_GUARD), None);
        // Map, write, read and unmap a new page.
        let frame = pmm::with(|p| p.allocate()).expect("frame");
        let virt = 0xFFFF_9000_0000_0000u64;
        m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE)
            .expect("map");
        // SAFETY: freshly mapped RW page, exclusive to this test.
        unsafe {
            (virt as *mut u64).write_volatile(0x5EA5_0DE5);
            assert_eq!(
                (phys::phys_to_virt(frame.addr()) as *const u64).read_volatile(),
                0x5EA5_0DE5
            );
        }
        assert_eq!(
            m.map_4k(virt, frame.addr(), 0),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(m.unmap_4k(virt), Ok(frame.addr()));
        pmm::with(|p| p.free(frame)).expect("free");
        assert_eq!(m.translate(virt), None);
        let _ = FRAME_SIZE;
        virt
    });
    // Outside the VMM lock: the invalidation notifies the other CPUs.
    flush_page(0xFFFF_9000_0000_0000u64);
    info!("vmm selftest ok (W^X, physical map, no identity map, map/unmap)");
}
