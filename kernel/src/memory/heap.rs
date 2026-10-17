// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Kernel heap (`#[global_allocator]`) on top of `aricode-heap`, growing on
//! demand: whenever an allocation does not fit, one more chunk of the
//! reserved virtual region is mapped with frames from the PMM.

use super::{pmm, vmm};
use crate::sync::IrqLock;
use aricode_heap::{Heap, HeapStats};
use aricode_pmm::Frame;
use aricode_x86_paging::{GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE};
use core::alloc::{GlobalAlloc, Layout};
use log::{debug, info};

/// Virtual region reserved for the heap (PML4 entry 384).
pub const HEAP_BASE: u64 = 0xFFFF_C000_0000_0000;
/// Maximum size the heap can reach.
pub const HEAP_MAX: u64 = 64 * 1024 * 1024;
/// First chunk mapped in `init` and growth granularity.
pub const HEAP_CHUNK: u64 = 1024 * 1024;

struct KernelHeap {
    heap: IrqLock<Heap>,
    /// Bytes of `HEAP_BASE..` already mapped. Its lock serializes growth.
    mapped: IrqLock<u64>,
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap {
    heap: IrqLock::new(Heap::empty()),
    mapped: IrqLock::new(0),
};

pub fn init() {
    grow(HEAP_CHUNK).expect("heap: no memory for the first chunk");
    info!(
        "heap: {} KiB at {:#x}, grows in {} KiB chunks up to {} MiB",
        HEAP_CHUNK / 1024,
        HEAP_BASE,
        HEAP_CHUNK / 1024,
        HEAP_MAX >> 20
    );
}

pub fn stats() -> HeapStats {
    HEAP.heap.lock().stats()
}

pub fn mapped_bytes() -> u64 {
    *HEAP.mapped.lock()
}

/// Maps `bytes` more (rounded up to a page) and adds them to the heap.
///
/// It is transactional: if frames run out or a mapping fails, the pages
/// already mapped in this call are unmapped and their frames go back to the
/// PMM, and `mapped` does not advance, so the next attempt starts from the
/// same state and can succeed once memory is available again. Intermediate
/// tables the VMM created are kept: they will be reused.
fn grow(bytes: u64) -> Result<(), &'static str> {
    // The `mapped` lock is held for the whole growth: neither the PMM nor
    // the VMM calls into the heap, so there is no re-entry.
    let mut mapped = HEAP.mapped.lock();
    let bytes = bytes.next_multiple_of(PAGE_SIZE);
    if *mapped + bytes > HEAP_MAX {
        return Err("heap: limit reached");
    }
    let start = HEAP_BASE + *mapped;

    let mut done = 0u64;
    let result = map_pages(start, bytes, &mut done);
    if let Err(e) = result {
        unmap_pages(start, done);
        return Err(e);
    }

    *mapped += bytes;
    // SAFETY: freshly mapped RW region, exclusive to the heap, page-aligned.
    unsafe { HEAP.heap.lock().add_region(start as usize, bytes as usize) };
    debug!("heap: grown to {} KiB", *mapped / 1024);
    Ok(())
}

/// Maps page by page; `done` counts the bytes successfully mapped so the
/// caller can undo them.
fn map_pages(start: u64, bytes: u64, done: &mut u64) -> Result<(), &'static str> {
    while *done < bytes {
        let frame = pmm::with(|p| p.allocate()).ok_or("heap: out of frames")?;
        let virt = start + *done;
        if vmm::with(|m| m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL)).is_err() {
            pmm::with(|p| p.free(frame)).expect("heap: freshly allocated frame");
            return Err("heap: map");
        }
        *done += PAGE_SIZE;
    }
    Ok(())
}

/// Undoes `map_pages`: unmaps, invalidates the TLB and returns the frames.
fn unmap_pages(start: u64, len: u64) {
    for virt in (start..start + len).step_by(PAGE_SIZE as usize) {
        let phys = vmm::with(|m| m.unmap_4k(virt)).expect("heap: page mapped by map_pages");
        // SAFETY: invalidation of an entry we just removed.
        unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
        pmm::with(|p| p.free(Frame::from_addr(phys))).expect("heap: frame mapped by map_pages");
    }
}

// SAFETY: every heap access takes its `IrqLock` for a single operation;
// `dealloc` only receives pointers from `alloc` per the `GlobalAlloc`
// contract. `grow` takes the lock on its own, so no guard can be alive when
// calling it.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let first = self.heap.lock().allocate(layout);
        if let Some(p) = first {
            return p.as_ptr();
        }
        // Does not fit: grow enough for this request (with some slack) and
        // retry with a fresh lock.
        let need = (layout.size() as u64)
            .saturating_add(layout.align() as u64)
            .saturating_add(64)
            .max(HEAP_CHUNK);
        if grow(need).is_err() {
            return core::ptr::null_mut();
        }
        self.heap
            .lock()
            .allocate(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = core::ptr::NonNull::new(ptr) {
            self.heap.lock().deallocate(p, layout);
        }
    }
}

/// Self-test (`test=heap`): `alloc` collections, heap growth, controlled
/// failure on exhaustion and absence of leaks.
pub fn selftest() {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    let before = stats();
    {
        let mut v: Vec<u64> = Vec::new();
        for i in 0..100_000u64 {
            v.push(i * 3);
        }
        assert_eq!(v.iter().sum::<u64>(), 3 * (99_999 * 100_000 / 2));
        let b = Box::new([0xA5u8; 4096]);
        assert!(b.iter().all(|&x| x == 0xA5));
        let s: String = (0..500)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        assert_eq!(s.len(), 500);
        let msg = alloc::format!("heap {} {}", s.len(), v.len());
        assert_eq!(msg, "heap 500 100000");
        // Something big that forces growth beyond the first chunk.
        let big: Vec<u8> = alloc::vec![7u8; 3 * 1024 * 1024];
        assert_eq!(big[big.len() - 1], 7);
        assert!(
            mapped_bytes() >= 4 * 1024 * 1024,
            "the heap should have grown"
        );
        // Impossible requests: they fail without panicking. 1 << 46 overflows
        // address arithmetic in the higher half (audit 9d8c7e5, 1).
        let mut w: Vec<u8> = Vec::new();
        assert!(w.try_reserve(1 << 40).is_err());
        assert!(w.try_reserve(1 << 46).is_err());
        let mut z: Vec<[u8; 4096]> = Vec::new();
        assert!(z.try_reserve(1 << 34).is_err());
    }
    let after = stats();
    assert_eq!(
        after.allocations, before.allocations,
        "live allocations after freeing everything"
    );
    assert_eq!(
        after.total - after.free,
        before.total - before.free,
        "byte leak"
    );
    info!(
        "heap selftest ok (Vec/Box/String/format, grown to {} KiB, try_reserve fails cleanly)",
        mapped_bytes() / 1024
    );
}

/// Exhaustion self-test (`test=heap_oom`, audit 9d8c7e5, finding 2): a growth
/// that fails for lack of frames must not leak pages nor leave the heap
/// unable to grow once memory is available again.
pub fn oom_selftest() {
    use alloc::vec::Vec;
    const KEEP: usize = 8;

    // Capacity reserved up front: draining must not request heap memory.
    let mut held: Vec<Frame> = Vec::with_capacity(pmm::stats().free_frames + 16);
    loop {
        let f = pmm::with(|p| {
            if p.stats().free_frames > KEEP {
                p.allocate()
            } else {
                None
            }
        });
        match f {
            Some(f) => held.push(f),
            None => break,
        }
    }
    let mapped_before = mapped_bytes();
    let free_before = pmm::stats().free_frames;
    assert_eq!(free_before, KEEP);

    let mut big: Vec<u8> = Vec::new();
    assert!(
        big.try_reserve(8 << 20).is_err(),
        "with {KEEP} free frames it cannot grow 8 MiB"
    );
    assert_eq!(
        mapped_bytes(),
        mapped_before,
        "a failed growth must not advance mapped"
    );
    let free_after = pmm::stats().free_frames;
    // Only intermediate tables can have been consumed (one per new 2 MiB).
    let lost = free_before - free_after;
    assert!(lost <= 4, "frames lost in the failed growth: {lost}");

    for f in held.drain(..) {
        pmm::with(|p| p.free(f)).expect("return held frame");
    }
    assert!(
        big.try_reserve(8 << 20).is_ok(),
        "with memory available again the heap must grow"
    );
    assert!(mapped_bytes() >= mapped_before + (8 << 20));
    drop(big);
    info!(
        "heap oom selftest ok (failure with {KEEP} frames, {lost} tables, recovery to {} KiB)",
        mapped_bytes() / 1024
    );
}
