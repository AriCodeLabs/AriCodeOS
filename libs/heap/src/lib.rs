// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel heap (Phase 7): linked list of free blocks, first-fit,
//! coalescing neighbours on free.
//!
//! Every allocation carries a 16-byte header right before the returned
//! pointer with the actual block it occupies, so `deallocate` does not depend
//! on the `Layout` it is given and large alignments do not leak memory.
//!
//! `no_std`, no `alloc`. Tested on the host over a buffer from the process
//! heap. This is the "linked-list allocator" step of section 16 of the master
//! document; slab/size classes will come when metrics call for them.

#![no_std]

use core::alloc::Layout;
use core::ptr::NonNull;

/// Size granularity and minimum alignment of the heap.
pub const GRANULE: usize = 16;
/// Minimum free block: must hold a `FreeBlock`.
pub const MIN_BLOCK: usize = 32;
const HEADER: usize = 16;

/// Free-list node, placed at the start of the block.
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

/// Header of a live allocation, right before the returned pointer.
#[repr(C)]
#[derive(Clone, Copy)]
struct AllocHeader {
    block_start: usize,
    block_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    /// Managed bytes (sum of added regions).
    pub total: usize,
    /// Bytes in free blocks.
    pub free: usize,
    /// Live allocations.
    pub allocations: usize,
}

pub struct Heap {
    head: Option<NonNull<FreeBlock>>,
    stats: HeapStats,
}

// SAFETY: the heap shares nothing by itself; the caller serialises
// access (in the kernel, `SingleCpu`/spinlock).
unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Heap {
            head: None,
            stats: HeapStats {
                total: 0,
                free: 0,
                allocations: 0,
            },
        }
    }

    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Adds `[start, start+size)` to the heap as a free block.
    ///
    /// # Safety
    ///
    /// The region must be valid, writable memory, aligned to `GRANULE`,
    /// with a size that is a multiple of `GRANULE` and >= `MIN_BLOCK`, used
    /// exclusively by the heap while it lives, and not overlapping regions already added.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        assert!(start.is_multiple_of(GRANULE) && size.is_multiple_of(GRANULE) && size >= MIN_BLOCK);
        self.stats.total += size;
        self.insert_free(start, size);
    }

    /// Inserts a free block keeping the list sorted by address and
    /// coalescing it with its neighbours.
    unsafe fn insert_free(&mut self, start: usize, size: usize) {
        self.stats.free += size;
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut cur = self.head;
        while let Some(c) = cur {
            if c.as_ptr() as usize > start {
                break;
            }
            prev = Some(c);
            cur = c.as_ref().next;
        }
        // Coalesce with the next one.
        let (mut size, next) = match cur {
            Some(n) if start + size == n.as_ptr() as usize => {
                (size + n.as_ref().size, n.as_ref().next)
            }
            _ => (size, cur),
        };
        // Coalesce with the previous one.
        match prev {
            Some(mut p) if p.as_ptr() as usize + p.as_ref().size == start => {
                size += p.as_ref().size;
                p.as_mut().size = size;
                p.as_mut().next = next;
            }
            _ => {
                let node = start as *mut FreeBlock;
                node.write(FreeBlock { size, next });
                let node = NonNull::new_unchecked(node);
                match prev {
                    Some(mut p) => p.as_mut().next = Some(node),
                    None => self.head = Some(node),
                }
            }
        }
    }

    /// Effective size and alignment. `None` if the rounding is not representable.
    fn round(layout: Layout) -> Option<(usize, usize)> {
        let size = layout.size().max(1).checked_next_multiple_of(GRANULE)?;
        let align = layout.align().max(GRANULE);
        Some((size, align))
    }

    /// `None` if there is no suitable block or if the request is not
    /// representable in the address space. Never modifies the list before
    /// checking that the allocation fits.
    pub fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let (size, align) = Self::round(layout)?;
        self.allocate_rounded(size, align)
    }

    /// Core of `allocate` with `size` a multiple of `GRANULE` and `align` a
    /// power of two >= `GRANULE`. Split out to test the arithmetic
    /// with values `Layout` does not accept but that do occur with valid layouts
    /// in the upper half of the kernel virtual address space.
    fn allocate_rounded(&mut self, size: usize, align: usize) -> Option<NonNull<u8>> {
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut cur = self.head;
        while let Some(c) = cur {
            // SAFETY: `c` is a valid node of our list.
            let (block_start, block_size, next) =
                unsafe { (c.as_ptr() as usize, c.as_ref().size, c.as_ref().next) };
            let block_end = block_start + block_size;
            // Checked arithmetic: in the upper half of the virtual address space a
            // large request or a huge alignment overflows `usize`, and a
            // wrapped value would pass the `used_end <= block_end` check.
            let fits = (block_start + HEADER)
                .checked_next_multiple_of(align)
                .and_then(|user| user.checked_add(size).map(|end| (user, end)))
                .filter(|&(_, end)| end <= block_end);
            if let Some((user, used_end)) = fits {
                let tail = block_end - used_end;
                let used_end = if tail >= MIN_BLOCK {
                    used_end
                } else {
                    block_end
                };
                // Unlink the block from the list.
                // SAFETY: `prev`, if present, is a valid node.
                unsafe {
                    match prev {
                        Some(mut p) => p.as_mut().next = next,
                        None => self.head = next,
                    }
                }
                self.stats.free -= block_size;
                if used_end < block_end {
                    // SAFETY: remainder of the block, >= MIN_BLOCK and aligned.
                    unsafe { self.insert_free(used_end, block_end - used_end) };
                }
                let header = AllocHeader {
                    block_start,
                    block_size: used_end - block_start,
                };
                // SAFETY: `user - HEADER >= block_start`, inside the block.
                unsafe { ((user - HEADER) as *mut AllocHeader).write(header) };
                self.stats.allocations += 1;
                return NonNull::new(user as *mut u8);
            }
            prev = cur;
            cur = next;
        }
        None
    }

    /// # Safety
    ///
    /// `ptr` must come from `allocate` of this heap and not have been freed.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, _layout: Layout) {
        let header = ((ptr.as_ptr() as usize - HEADER) as *const AllocHeader).read();
        self.stats.allocations -= 1;
        self.insert_free(header.block_start, header.block_size);
    }

    /// Number of free blocks (for tests and diagnostics).
    pub fn free_blocks(&self) -> usize {
        let mut n = 0;
        let mut cur = self.head;
        while let Some(c) = cur {
            n += 1;
            // SAFETY: valid list node.
            cur = unsafe { c.as_ref().next };
        }
        n
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc, dealloc};
    use std::vec::Vec;

    struct Region {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Region {
        fn new(size: usize) -> Self {
            let layout = Layout::from_size_align(size, 4096).unwrap();
            Self {
                ptr: unsafe { alloc(layout) },
                layout,
            }
        }
        fn heap(&self) -> Heap {
            let mut h = Heap::empty();
            unsafe { h.add_region(self.ptr as usize, self.layout.size()) };
            h
        }
    }
    impl Drop for Region {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    fn l(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn alloc_free_reuse_and_coalesce() {
        let r = Region::new(4096);
        let mut h = r.heap();
        assert_eq!(h.stats().free, 4096);
        let a = h.allocate(l(100, 8)).unwrap();
        let b = h.allocate(l(200, 8)).unwrap();
        assert_eq!(h.stats().allocations, 2);
        assert!(a.as_ptr() < b.as_ptr());
        unsafe { h.deallocate(a, l(100, 8)) };
        unsafe { h.deallocate(b, l(200, 8)) };
        assert_eq!(h.stats().allocations, 0);
        assert_eq!(h.stats().free, 4096, "everything returns to the heap");
        assert_eq!(h.free_blocks(), 1, "blocks are coalesced");
        // The same address is reused.
        assert_eq!(h.allocate(l(100, 8)).unwrap(), a);
    }

    #[test]
    fn respects_large_alignment() {
        let r = Region::new(16384);
        let mut h = r.heap();
        let _pad = h.allocate(l(24, 8)).unwrap();
        let p = h.allocate(l(64, 1024)).unwrap();
        assert_eq!(p.as_ptr() as usize % 1024, 0);
        let q = h.allocate(l(8, 4096)).unwrap();
        assert_eq!(q.as_ptr() as usize % 4096, 0);
        unsafe {
            h.deallocate(p, l(64, 1024));
            h.deallocate(q, l(8, 4096));
            h.deallocate(_pad, l(24, 8));
        }
        assert_eq!(h.stats().free, 16384, "no leaks with large alignments");
        assert_eq!(h.free_blocks(), 1);
    }

    #[test]
    fn out_of_memory_returns_none_and_keeps_state() {
        let r = Region::new(1024);
        let mut h = r.heap();
        assert!(h.allocate(l(2000, 8)).is_none());
        let a = h.allocate(l(900, 8)).unwrap();
        assert!(h.allocate(l(200, 8)).is_none());
        unsafe { h.deallocate(a, l(900, 8)) };
        assert_eq!(h.stats().free, 1024);
    }

    #[test]
    fn unrepresentable_requests_return_none_and_leave_list_intact() {
        // Audit 9d8c7e5, finding 1: without checked arithmetic a huge size
        // wraps around and writes outside the heap.
        let r = Region::new(4096);
        let mut h = r.heap();
        // In the kernel the heap lives at 0xFFFF_C000_..., where a valid
        // 64 TiB `Layout` already overflows `user + size`. On the host the region
        // is at low addresses, so `allocate_rounded` is exercised with
        // sizes `Layout` does not accept: the arithmetic must reject them without
        // touching the list.
        assert!(
            h.allocate_rounded(usize::MAX - 15, GRANULE).is_none(),
            "user + size overflows"
        );
        assert!(
            h.allocate_rounded(GRANULE, 1 << 63).is_none(),
            "alignment outside the block"
        );
        // Rounding of the `Layout` itself cannot overflow either.
        assert!(Heap::round(l(isize::MAX as usize, 1)).is_some());
        // Huge alignment with a valid layout: the next multiple does not fit.
        assert!(h.allocate(l(16, 1 << 62)).is_none());
        assert_eq!(h.stats().free, 4096);
        assert_eq!(h.free_blocks(), 1);
        assert_eq!(h.stats().allocations, 0);
        // And the heap is still usable.
        assert!(h.allocate(l(64, 8)).is_some());
    }

    #[test]
    fn many_small_allocations_in_random_order() {
        let r = Region::new(64 * 1024);
        let mut h = r.heap();
        let mut live: Vec<(NonNull<u8>, Layout)> = Vec::new();
        for i in 0..500usize {
            let size = 1 + (i * 37) % 200;
            let p = h.allocate(l(size, 8)).unwrap();
            unsafe { core::ptr::write_bytes(p.as_ptr(), i as u8, size) };
            live.push((p, l(size, 8)));
        }
        // Free in pseudo-random order and check there are no overlaps.
        for (i, (p, lay)) in live.iter().enumerate() {
            let expect = i as u8;
            let bytes = unsafe { core::slice::from_raw_parts(p.as_ptr(), lay.size()) };
            assert!(
                bytes.iter().all(|&b| b == expect),
                "allocation {i} overwritten"
            );
        }
        let mut order: Vec<usize> = (0..live.len()).collect();
        order.sort_by_key(|&i| (i * 7919) % 500);
        for i in order {
            let (p, lay) = live[i];
            unsafe { h.deallocate(p, lay) };
        }
        assert_eq!(h.stats().free, 64 * 1024);
        assert_eq!(h.free_blocks(), 1);
    }

    #[test]
    fn multiple_regions_and_small_tail_absorbed() {
        let r1 = Region::new(4096);
        let r2 = Region::new(4096);
        let mut h = r1.heap();
        unsafe { h.add_region(r2.ptr as usize, 4096) };
        assert_eq!(h.stats().total, 8192);
        // Allocation leaving a tail smaller than MIN_BLOCK: it is absorbed.
        let a = h.allocate(l(4096 - HEADER - 8, 8)).unwrap();
        assert_eq!(h.free_blocks(), 1, "the small tail does not create a block");
        unsafe { h.deallocate(a, l(1, 1)) }; // the layout does not matter: the header rules
        assert_eq!(h.stats().free, 8192);
    }
}
