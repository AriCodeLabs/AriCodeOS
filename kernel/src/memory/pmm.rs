// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Global physical allocator (Phase 5), built on `aricode-pmm` from the UEFI
//! memory map in `BootInfo`.
//!
//! The bitmap is placed in the lowest free region with enough room and is
//! accessed through the linear physical map.

use super::phys;
use crate::sync::IrqLock;
use aricode_pmm::{BitmapAllocator, Frame, RangeSet, Stats, FRAME_SIZE};
use boot_protocol::{BootInfo, MemoryRegionKind};
use log::{debug, info, warn};

static PMM: IrqLock<Option<BitmapAllocator<'static>>> = IrqLock::new(None);

/// What the firmware map reported as RAM or firmware data (free, boot,
/// runtime services, ACPI), and as reserved: what no device BAR may cover
/// (ADR 0051). Kept past boot, when the map itself is gone.
static RAM: IrqLock<RangeSet<64>> = IrqLock::new(RangeSet::new());
static RESERVED: IrqLock<RangeSet<64>> = IrqLock::new(RangeSet::new());

/// The map's kinds one by one, also kept past boot: what a DMA fault's
/// page was to the firmware (incident 0064). The boot services' ranges are
/// the most scattered.
static FREE_AT_BOOT: IrqLock<RangeSet<64>> = IrqLock::new(RangeSet::new());
static BOOT: IrqLock<RangeSet<128>> = IrqLock::new(RangeSet::new());
static RUNTIME: IrqLock<RangeSet<32>> = IrqLock::new(RangeSet::new());
static ACPI: IrqLock<RangeSet<32>> = IrqLock::new(RangeSet::new());

/// What the firmware's map said `addr` was (`sysinfo::dma_site::MEM_*`,
/// with `MEM_APPROXIMATE` when the kept copy of that kind merged gaps).
/// The precise kinds first: an approximated set covers more than it was
/// given, never less.
pub fn memory_kind(addr: u64) -> u8 {
    use aricode_abi::sysinfo::dma_site as d;
    fn hit<const N: usize>(set: &IrqLock<RangeSet<N>>, addr: u64, kind: u8) -> Option<u8> {
        let set = set.lock();
        set.as_slice()
            .iter()
            .any(|&(s, e)| addr >= s && addr < e)
            .then(|| {
                kind | if set.approximated() {
                    d::MEM_APPROXIMATE
                } else {
                    0
                }
            })
    }
    hit(&RESERVED, addr, d::MEM_RESERVED)
        .or_else(|| hit(&RUNTIME, addr, d::MEM_FIRMWARE))
        .or_else(|| hit(&ACPI, addr, d::MEM_ACPI))
        .or_else(|| hit(&FREE_AT_BOOT, addr, d::MEM_FREE))
        .or_else(|| hit(&BOOT, addr, d::MEM_BOOT))
        .unwrap_or(d::MEM_NONE)
}

/// The allocator's view of the frame holding `addr` now
/// (`sysinfo::dma_site::FRAME_*`).
pub fn frame_state(addr: u64) -> u8 {
    use aricode_abi::sysinfo::dma_site as d;
    let f = Frame::from_addr(addr & !(FRAME_SIZE - 1));
    with(|p| {
        if p.is_free(f) {
            d::FRAME_FREE
        } else if (f.number() as usize) < p.frames() {
            d::FRAME_IN_USE
        } else {
            d::FRAME_UNMANAGED
        }
    })
}

/// Calls `f` with each range a device BAR must not cover, and what it is.
pub fn for_each_firmware_range(mut f: impl FnMut(u64, u64, &'static str)) {
    for &(start, end) in RAM.lock().as_slice() {
        f(start, end, "RAM");
    }
    for &(start, end) in RESERVED.lock().as_slice() {
        f(start, end, "firmware-reserved memory");
    }
}

/// Low memory that is never handed out: frame 0 (null) and the first MiB,
/// needed by the SMP trampoline and some legacy devices.
pub const LOW_RESERVED_END: u64 = 1024 * 1024;

/// Builds the allocator. Must be called once, while the map is still accessible.
///
/// # Panics
/// If the map contains no free region able to hold the bitmap.
pub fn init(info: &BootInfo) {
    // SAFETY: the map lives in boot memory reachable through the physical map.
    let regions = || unsafe { info.memory_regions() };

    // Managed extent: up to the end of the last non-reserved region.
    let phys_end = regions()
        .filter(|d| d.kind() != MemoryRegionKind::Reserved)
        .map(|d| d.end())
        .max()
        .expect("empty memory map");
    let frames = (phys_end / FRAME_SIZE) as usize;
    let bitmap_bytes = BitmapAllocator::bytes_for(frames);
    let bitmap_frames = bitmap_bytes.div_ceil(FRAME_SIZE as usize) as u64;

    // Lowest free region, above the reserved low memory, that can hold
    // the whole bitmap.
    let bitmap_addr = regions()
        .filter(|d| d.kind() == MemoryRegionKind::Free)
        .map(|d| (d.phys_start.max(LOW_RESERVED_END), d.end()))
        .filter(|(start, end)| end.saturating_sub(*start) >= bitmap_frames * FRAME_SIZE)
        .map(|(start, _)| start)
        .min()
        .expect("no free region for the PMM bitmap");

    // SAFETY: free region according to the firmware, reachable through the
    // physical map, frame-aligned and owned exclusively by the PMM from now on.
    let words: &'static mut [u64] = unsafe {
        core::slice::from_raw_parts_mut(
            phys::phys_to_virt(bitmap_addr) as *mut u64,
            BitmapAllocator::words_for(frames),
        )
    };
    let mut pmm = BitmapAllocator::new(words, frames);
    for d in regions().filter(|d| d.kind() == MemoryRegionKind::Free) {
        pmm.free_range(d.phys_start, d.end());
        debug!(
            "pmm: free {:#012x}..{:#012x} ({} KiB)",
            d.phys_start,
            d.end(),
            d.size_bytes() / 1024
        );
    }
    {
        let (mut ram, mut reserved) = (RAM.lock(), RESERVED.lock());
        let (mut free, mut boot, mut runtime, mut acpi) = (
            FREE_AT_BOOT.lock(),
            BOOT.lock(),
            RUNTIME.lock(),
            ACPI.lock(),
        );
        for d in regions() {
            match d.kind() {
                MemoryRegionKind::Free
                | MemoryRegionKind::Boot
                | MemoryRegionKind::Firmware
                | MemoryRegionKind::Acpi => ram.add(d.phys_start, d.end()),
                MemoryRegionKind::Reserved => reserved.add(d.phys_start, d.end()),
                MemoryRegionKind::Mmio => {}
            }
            match d.kind() {
                MemoryRegionKind::Free => free.add(d.phys_start, d.end()),
                MemoryRegionKind::Boot => boot.add(d.phys_start, d.end()),
                MemoryRegionKind::Firmware => runtime.add(d.phys_start, d.end()),
                MemoryRegionKind::Acpi => acpi.add(d.phys_start, d.end()),
                MemoryRegionKind::Reserved | MemoryRegionKind::Mmio => {}
            }
        }
        if ram.approximated() || reserved.approximated() {
            warn!("pmm: memory map with too many ranges: small gaps counted as memory");
        }
    }
    pmm.reserve_range(0, LOW_RESERVED_END);
    pmm.reserve_range(bitmap_addr, bitmap_addr + bitmap_frames * FRAME_SIZE);

    let stats = pmm.stats();
    info!(
        "pmm: {} MiB free of {} MiB addressable, bitmap {} KiB @ {:#x}",
        stats.free_bytes() >> 20,
        stats.total_bytes() >> 20,
        bitmap_bytes.div_ceil(1024),
        bitmap_addr
    );
    *PMM.lock() = Some(pmm);
}

/// Exclusive access to the allocator.
///
/// # Panics
/// If `init` has not run.
pub fn with<R>(f: impl FnOnce(&mut BitmapAllocator<'static>) -> R) -> R {
    let mut guard = PMM.lock();
    f(guard.as_mut().expect("PMM not initialized"))
}

pub fn stats() -> Stats {
    with(|p| p.stats())
}

/// Self-test on the real machine (`test=pmm`): uniqueness, freeing and
/// contiguity on the real bitmap, with QEMU's memory map.
pub fn selftest() {
    const N: usize = 2048;
    let before = stats();
    with(|p| {
        let mut frames = [Frame::from_number(0); N];
        for (i, slot) in frames.iter_mut().enumerate() {
            let f = p.allocate().expect("out of frames");
            assert!(f.addr().is_multiple_of(FRAME_SIZE), "frame {i} not aligned");
            assert!(f.addr() >= LOW_RESERVED_END, "frame {i} in low memory");
            assert!(!p.is_free(f), "frame {i} still free after allocation");
            *slot = f;
        }
        for w in frames.windows(2) {
            assert!(w[0] < w[1], "frames not increasing: {:?} {:?}", w[0], w[1]);
        }
        assert_eq!(p.stats().free_frames, before.free_frames - N);
        for f in frames {
            p.free(f).expect("free");
        }
        assert_eq!(p.free(frames[0]), Err(aricode_pmm::PmmError::DoubleFree));
        let big = p.allocate_contiguous(64, 16).expect("contiguous");
        assert!(big.number().is_multiple_of(16));
        p.free_contiguous(big, 64).expect("free contiguous");
    });
    assert_eq!(stats(), before, "stats do not return to the initial state");
    info!("pmm selftest ok ({N} frames, contiguous 64x aligned 16)");
}
