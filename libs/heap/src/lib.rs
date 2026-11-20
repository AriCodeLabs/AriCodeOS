// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The heap of the kernel and of every user program: a two-level
//! segregated-fit allocator (TLSF, Masmano et al., "TLSF: a New Dynamic
//! Memory Allocator for Real-Time Systems", ECRTS 2004), with boundary tags
//! so a freed block finds its neighbours without searching.
//!
//! Incident 0066 replaced the first-fit, address-ordered free list this
//! crate had: its allocation and its free both walked that list, so a
//! program freeing many blocks out of address order (a database committing
//! a large transaction) paid O(free blocks) per operation, quadratic in
//! all. The design was chosen by measuring ours against the veterans (the
//! incident has the numbers): TLSF (`rlsf`) and `dlmalloc` were the
//! fastest, but both crates link `libc` on the user programs' Linux target,
//! which ADR 0065 refuses, so the winning design is implemented here and
//! measured against `rlsf`.
//!
//! - Free blocks sit in `FL_COUNT x SL_COUNT` lists by size: the first level
//!   is the power of two, the second splits it in `SL_COUNT` equal ranges
//!   (sizes under `SMALL` get one list per granule). Two bitmaps say which
//!   lists are not empty.
//! - `allocate` first looks at a bounded number of blocks (`PROBES`) of
//!   the list the request falls in, as Zephyr's `sys_heap` does, so a block
//!   of the size just freed is reused. Failing that it rounds the request
//!   up to the next list boundary ("good fit"), so the head of any
//!   non-empty list from there up fits: two bit scans and no search. A
//!   growing heap must ask for `region_size_for`, not the request. Large
//!   alignments split the leading gap off as a free block.
//! - Every block starts with a 16-byte header: its size with a `USED` bit,
//!   and the address of the block physically before it. `deallocate` merges
//!   with both neighbours in constant time and does not depend on the
//!   `Layout` it is given. Each region ends in a 16-byte used sentinel, so
//!   merging never leaves the region; a region that starts where the last
//!   one ended continues it.
//! - Every operation is O(1): no step depends on the number of blocks.
//!
//! `no_std`, no `alloc`. Tested on the host over buffers from the process
//! heap.

#![no_std]

use core::alloc::Layout;
use core::ptr::{self, NonNull};

/// Size granularity of blocks and the alignment every allocation gets.
pub const GRANULE: usize = 16;
const GRANULE_LOG2: u32 = GRANULE.trailing_zeros();
/// Header of every block: size and flags, previous physical block.
const HEADER: usize = 16;
/// Smallest block: a free one holds its header and two list links.
pub const MIN_BLOCK: usize = 32;
/// Smallest region `add_region` accepts: one block and the sentinel.
pub const MIN_REGION: usize = MIN_BLOCK + HEADER;

/// Second-level lists per power of two (log2). 32 is the paper's and
/// `rlsf`'s default; 64 measured no faster and no smaller on the incident's
/// workload (study 2). It bounds the waste of good fit at 1/32.
const SL_LOG2: u32 = 5;
const SL_COUNT: usize = 1 << SL_LOG2;
/// Sizes below this get one list per granule (first level 0).
const SMALL: usize = SL_COUNT << GRANULE_LOG2;
const FL_SHIFT: u32 = SL_LOG2 + GRANULE_LOG2;
/// First-level lists: level 0 for small sizes, then one per power of two
/// from `SMALL` up to the largest `usize`.
const FL_COUNT: usize = (usize::BITS - FL_SHIFT + 1) as usize;
const _: () = assert!(FL_COUNT < u64::BITS as usize && SL_COUNT <= u32::BITS as usize);

/// `Block::size` flag: the block is allocated (or is a region's sentinel).
const USED: usize = 1;
const SIZE_MASK: usize = !(GRANULE - 1);

/// Header at the start of every block, used or free.
#[repr(C)]
struct Block {
    /// Bytes of the whole block, header included, with `USED`.
    size: usize,
    /// The block physically before this one; null for a region's first.
    prev_phys: *mut Block,
}

/// A free block: the header and its links in its size list.
#[repr(C)]
struct FreeBlock {
    block: Block,
    next_free: *mut FreeBlock,
    prev_free: *mut FreeBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    /// Managed bytes (sum of added regions).
    pub total: usize,
    /// Bytes in free blocks, their headers included.
    pub free: usize,
    /// Live allocations.
    pub allocations: usize,
    /// Free blocks: how fragmented the free bytes are.
    pub free_blocks: usize,
}

/// The heap. `PROBES` is how many blocks `allocate` looks at in the list a
/// request falls in, whose blocks may or may not hold it, before it takes
/// the head of a list sure to fit. Good fit alone never reuses a block of
/// exactly the size freed when that size is not a list boundary (a
/// 4096-byte buffer needs a 4112-byte block, which good fit looks for among
/// blocks of 4224 or more), so a program freeing and allocating buffers of
/// one size keeps carving new ones. Bounded like Zephyr's `sys_heap`: the
/// cost stays constant, and 0 is pure good fit. A type parameter, not a
/// field, so an empty heap is all zeros and a `static` one costs no file
/// space (`.bss`).
pub struct Heap<const PROBES: usize = DEFAULT_PROBES> {
    /// Bit `fl` set: some list of first level `fl` is not empty.
    fl_bitmap: u64,
    /// Bit `sl` of `sl_bitmap[fl]` set: list `(fl, sl)` is not empty.
    sl_bitmap: [u32; FL_COUNT],
    heads: [[*mut FreeBlock; SL_COUNT]; FL_COUNT],
    /// End of the region added last: a region starting there continues it.
    last_end: usize,
    stats: HeapStats,
}

// SAFETY: the heap shares nothing by itself: its pointers lead only into
// regions it was given for its exclusive use; the caller serialises access
// (the kernel's `IrqLock`, the runtime's spinlock).
unsafe impl<const PROBES: usize> Send for Heap<PROBES> {}

/// The list a free block of `size` bytes belongs to.
fn class_floor(size: usize) -> (usize, usize) {
    if size < SMALL {
        (0, size >> GRANULE_LOG2)
    } else {
        let msb = usize::BITS - 1 - size.leading_zeros();
        let fl = (msb - FL_SHIFT + 1) as usize;
        let sl = (size >> (msb - SL_LOG2)) & (SL_COUNT - 1);
        (fl, sl)
    }
}

/// `size` rounded up to the smallest size of a list whose every block is
/// at least `size`; `None` when that is not representable.
fn round_to_class(size: usize) -> Option<usize> {
    if size < SMALL {
        return Some(size);
    }
    let msb = usize::BITS - 1 - size.leading_zeros();
    let step = 1usize << (msb - SL_LOG2);
    size.checked_add(step - 1).map(|s| s & !(step - 1))
}

/// Block size that serves `size` payload bytes right after the header.
fn block_size(size: usize) -> Option<usize> {
    let s = size.max(1).checked_add(HEADER + GRANULE - 1)? & !(GRANULE - 1);
    Some(s.max(MIN_BLOCK))
}

/// Size of free block an allocation of `size` bytes aligned to `align`
/// must find. Above `GRANULE` the header may have to move up to
/// `align + GRANULE` bytes into the block (`leading_gap`).
fn search_size(size: usize, align: usize) -> Option<usize> {
    let need = block_size(size)?;
    if align <= GRANULE {
        Some(need)
    } else {
        need.checked_add(align)?.checked_add(GRANULE)
    }
}

/// Bytes between a free block at `start` and the header of an allocation
/// aligned to `align` placed in it: zero, or a gap large enough to be a
/// free block of its own.
fn leading_gap(start: usize, align: usize) -> usize {
    let payload = (start + HEADER).next_multiple_of(align);
    let gap = payload - HEADER - start;
    if gap == 0 || gap >= MIN_BLOCK {
        gap
    } else {
        gap + align
    }
}

/// Blocks a heap probes by default (`Heap`'s `PROBES`): Zephyr's
/// `CONFIG_SYS_HEAP_ALLOC_LOOPS` default. On the incident's AriDB traces 1
/// to 8 map the same; on its fragmentation trace 3 mapped the least under
/// the programs' growth and 0 to 16 stay within 3% under the kernel's (the
/// incident has both sweeps).
pub const DEFAULT_PROBES: usize = 3;

/// The size of a fresh region that is sure to serve `layout`, for a heap
/// growing on demand: the allocator's own rounding (good fit) included.
/// `None` if no region could.
pub fn region_size_for(layout: Layout) -> Option<usize> {
    round_to_class(search_size(layout.size(), layout.align())?)?.checked_add(HEADER)
}

impl<const PROBES: usize> Heap<PROBES> {
    pub const fn empty() -> Self {
        Heap {
            fl_bitmap: 0,
            sl_bitmap: [0; FL_COUNT],
            heads: [[ptr::null_mut(); SL_COUNT]; FL_COUNT],
            last_end: 0,
            stats: HeapStats {
                total: 0,
                free: 0,
                allocations: 0,
                free_blocks: 0,
            },
        }
    }

    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Number of free blocks (diagnostics and tests).
    pub fn free_blocks(&self) -> usize {
        self.stats.free_blocks
    }

    /// Adds `[start, start+size)` to the heap. A region that starts where
    /// the previous one ended continues it: their free space merges.
    ///
    /// # Safety
    ///
    /// The region must be valid, writable memory, aligned to `GRANULE`,
    /// with a size that is a multiple of `GRANULE` and >= `MIN_REGION`, used
    /// exclusively by the heap while it lives, and not overlapping regions
    /// already added.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        assert!(
            start != 0
                && start.is_multiple_of(GRANULE)
                && size.is_multiple_of(GRANULE)
                && size >= MIN_REGION
                && start.checked_add(size).is_some()
        );
        self.stats.total += size;
        let end = start + size;
        let sentinel = (end - HEADER) as *mut Block;
        // SAFETY: every address written is inside this region or, when it
        // continues the last one, that region's sentinel, which becomes
        // part of the new free block; a free block before it is ours.
        unsafe {
            let (mut block, mut bsize, prev_phys) = if start == self.last_end {
                // The old sentinel opens the new free block.
                let old = (start - HEADER) as *mut Block;
                (old, size, (*old).prev_phys)
            } else {
                (start as *mut Block, size - HEADER, ptr::null_mut())
            };
            if !prev_phys.is_null() && (*prev_phys).size & USED == 0 {
                self.unlink(prev_phys.cast());
                bsize += (*prev_phys).size;
                block = prev_phys;
            } else {
                (*block).prev_phys = prev_phys;
            }
            (*block).size = bsize;
            sentinel.write(Block {
                size: HEADER | USED,
                prev_phys: block,
            });
            self.link(block.cast());
        }
        self.last_end = end;
    }

    /// Puts free `block` (its size already written) at the head of its list.
    ///
    /// # Safety
    ///
    /// `block` is a free block of ours, in no list.
    unsafe fn link(&mut self, block: *mut FreeBlock) {
        // SAFETY: `block` and the current head are free blocks of ours.
        unsafe {
            let size = (*block).block.size;
            let (fl, sl) = class_floor(size);
            let head = self.heads[fl][sl];
            (*block).next_free = head;
            (*block).prev_free = ptr::null_mut();
            if !head.is_null() {
                (*head).prev_free = block;
            }
            self.heads[fl][sl] = block;
            self.fl_bitmap |= 1 << fl;
            self.sl_bitmap[fl] |= 1 << sl;
            self.stats.free += size;
            self.stats.free_blocks += 1;
        }
    }

    /// Takes free `block` out of its list.
    ///
    /// # Safety
    ///
    /// `block` is a free block of ours, in the list of its size.
    unsafe fn unlink(&mut self, block: *mut FreeBlock) {
        // SAFETY: `block` and its list neighbours are free blocks of ours.
        unsafe {
            let size = (*block).block.size;
            let (next, prev) = ((*block).next_free, (*block).prev_free);
            if !next.is_null() {
                (*next).prev_free = prev;
            }
            if !prev.is_null() {
                (*prev).next_free = next;
            } else {
                let (fl, sl) = class_floor(size);
                self.heads[fl][sl] = next;
                if next.is_null() {
                    self.sl_bitmap[fl] &= !(1 << sl);
                    if self.sl_bitmap[fl] == 0 {
                        self.fl_bitmap &= !(1 << fl);
                    }
                }
            }
            self.stats.free -= size;
            self.stats.free_blocks -= 1;
        }
    }

    /// The first non-empty list whose blocks all hold `size` bytes.
    fn find(&self, size: usize) -> Option<(usize, usize)> {
        let (fl, sl) = class_floor(round_to_class(size)?);
        let here = self.sl_bitmap[fl] & (u32::MAX << sl);
        if here != 0 {
            return Some((fl, here.trailing_zeros() as usize));
        }
        // `fl + 1 < 64`: `FL_COUNT` is below 64.
        let above = self.fl_bitmap & (u64::MAX << (fl + 1));
        if above == 0 {
            return None;
        }
        let fl = above.trailing_zeros() as usize;
        Some((fl, self.sl_bitmap[fl].trailing_zeros() as usize))
    }

    /// Up to `PROBES` blocks of the list `size` falls in: the first that
    /// holds `size` bytes.
    fn probe(&self, size: usize) -> Option<*mut FreeBlock> {
        let (fl, sl) = class_floor(size);
        let mut b = self.heads[fl][sl];
        for _ in 0..PROBES {
            if b.is_null() {
                return None;
            }
            // SAFETY: `b` is a free block of ours, in that list.
            let (size_b, next) = unsafe { ((*b).block.size, (*b).next_free) };
            if size_b >= size {
                return Some(b);
            }
            b = next;
        }
        None
    }

    /// Makes the start of free `block` (in no list) of `bsize` bytes a used
    /// block of `need` bytes; the rest, if it can be a block, goes back free.
    ///
    /// # Safety
    ///
    /// `block` is ours, `need <= bsize`, both multiples of `GRANULE`, and
    /// the block after it is used (no two free blocks are ever adjacent).
    unsafe fn take(&mut self, block: *mut Block, bsize: usize, need: usize) {
        // SAFETY: every address is inside `block` or is its successor's
        // header, which exists because each region ends in a sentinel.
        unsafe {
            let used = if bsize - need >= MIN_BLOCK {
                let rest = block.byte_add(need);
                (*rest).size = bsize - need;
                (*rest).prev_phys = block;
                (*rest.byte_add(bsize - need)).prev_phys = rest;
                self.link(rest.cast());
                need
            } else {
                (*block.byte_add(bsize)).prev_phys = block;
                bsize
            };
            (*block).size = used | USED;
        }
    }

    /// `None` if there is no suitable block or the request is not
    /// representable in the address space; the heap is unchanged then.
    /// Constant time.
    pub fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let align = layout.align();
        let need = block_size(layout.size())?;
        let search = search_size(layout.size(), align)?;
        let free = match self.probe(search) {
            Some(b) => b,
            None => {
                let (fl, sl) = self.find(search)?;
                self.heads[fl][sl]
            }
        };
        let mut block: *mut Block = free.cast();
        // SAFETY: `block` is a free block of ours in a list, at least
        // `search` bytes long, so the gap and `need` fit in it;
        // the blocks before and after it are used (free neighbours are
        // always merged).
        unsafe {
            self.unlink(block.cast());
            let mut bsize = (*block).size;
            if align > GRANULE {
                let gap = leading_gap(block as usize, align);
                if gap > 0 {
                    let rest = block.byte_add(gap);
                    (*rest).prev_phys = block;
                    (*block).size = gap;
                    self.link(block.cast());
                    block = rest;
                    bsize -= gap;
                }
            }
            self.take(block, bsize, need);
            self.stats.allocations += 1;
            NonNull::new(block.byte_add(HEADER).cast())
        }
    }

    /// Frees the allocation at `ptr`, merging it with free neighbours.
    /// Constant time; the header rules, not `_layout`.
    ///
    /// # Safety
    ///
    /// `ptr` must come from `allocate` of this heap and not have been freed.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, _layout: Layout) {
        // SAFETY: per the contract, a used block of ours starts `HEADER`
        // bytes before `ptr`; its neighbours are blocks of ours (the
        // sentinel is never free, so `next` is always a block).
        unsafe {
            let mut block = ptr.as_ptr().byte_sub(HEADER).cast::<Block>();
            assert!(
                (*block).size & USED != 0,
                "heap: free of a block that is not allocated"
            );
            let mut size = (*block).size & SIZE_MASK;
            // Free from here on, also when it merges into the block before:
            // its stale header then makes a second free of `ptr` fail the
            // assert above instead of corrupting the lists.
            (*block).size = size;
            let next = block.byte_add(size);
            if (*next).size & USED == 0 {
                self.unlink(next.cast());
                size += (*next).size;
            }
            let prev = (*block).prev_phys;
            if !prev.is_null() && (*prev).size & USED == 0 {
                self.unlink(prev.cast());
                size += (*prev).size;
                block = prev;
            }
            (*block).size = size;
            (*block.byte_add(size)).prev_phys = block;
            self.link(block.cast());
        }
        self.stats.allocations -= 1;
    }

    /// Resizes the allocation at `ptr` to `new_size` bytes without moving
    /// it: shrinking always succeeds, growing when the block after it is
    /// free and large enough. `false` leaves everything unchanged; the
    /// caller then allocates, copies and frees. Constant time.
    ///
    /// # Safety
    ///
    /// `ptr` must come from `allocate` of this heap and not have been freed.
    pub unsafe fn resize_in_place(&mut self, ptr: NonNull<u8>, new_size: usize) -> bool {
        let Some(need) = block_size(new_size) else {
            return false;
        };
        // SAFETY: as in `deallocate`; every address written is inside the
        // block or the free block after it, or is the header of the block
        // that follows them.
        unsafe {
            let block = ptr.as_ptr().byte_sub(HEADER).cast::<Block>();
            assert!(
                (*block).size & USED != 0,
                "heap: resize of a block that is not allocated"
            );
            let size = (*block).size & SIZE_MASK;
            let next = block.byte_add(size);
            let next_free = (*next).size & USED == 0;
            let room = if next_free { size + (*next).size } else { size };
            if need > room {
                return false;
            }
            if next_free {
                self.unlink(next.cast());
            } else if size - need < MIN_BLOCK {
                // Same size, or shrinking by less than a block.
                return true;
            }
            self.take(block, room, need);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc, dealloc};
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};
    use std::vec::Vec;

    struct Region {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Region {
        fn new(size: usize) -> Self {
            let layout = Layout::from_size_align(size, 4096).unwrap();
            let ptr = unsafe { alloc(layout) };
            assert!(!ptr.is_null());
            Self { ptr, layout }
        }
        fn start(&self) -> usize {
            self.ptr as usize
        }
        fn size(&self) -> usize {
            self.layout.size()
        }
        fn heap(&self) -> Heap {
            let mut h: Heap = Heap::empty();
            unsafe { h.add_region(self.start(), self.size()) };
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

    /// Walks the blocks of the regions starting at `starts` and checks
    /// every invariant the constant-time operations rely on: sizes tile
    /// each region up to its sentinel, `prev_phys` links, no two adjacent
    /// free blocks, each free block in the list of its size, and the
    /// bitmaps and counters matching the lists.
    fn check<const P: usize>(h: &Heap<P>, starts: &[usize]) {
        let mut free_seen = 0;
        let mut free_bytes = 0;
        let mut used_seen = 0;
        for &start in starts {
            let mut prev: *mut Block = ptr::null_mut();
            let mut b = start as *mut Block;
            let mut prev_free = false;
            loop {
                let (size, prev_phys) = unsafe { ((*b).size, (*b).prev_phys) };
                assert_eq!(prev_phys, prev, "prev_phys of block {b:p}");
                if size == HEADER | USED {
                    break; // the sentinel
                }
                let len = size & SIZE_MASK;
                assert!(len >= MIN_BLOCK && len.is_multiple_of(GRANULE));
                if size & USED == 0 {
                    assert!(!prev_free, "two adjacent free blocks at {b:p}");
                    let (fl, sl) = class_floor(len);
                    let mut f = h.heads[fl][sl];
                    while !f.is_null() && f != b.cast() {
                        f = unsafe { (*f).next_free };
                    }
                    assert!(!f.is_null(), "free block {b:p} not in its list");
                    free_seen += 1;
                    free_bytes += len;
                    prev_free = true;
                } else {
                    used_seen += 1;
                    prev_free = false;
                }
                prev = b;
                b = unsafe { b.byte_add(len) };
            }
        }
        let mut listed = 0;
        for fl in 0..FL_COUNT {
            assert_eq!(h.fl_bitmap & (1 << fl) != 0, h.sl_bitmap[fl] != 0);
            for sl in 0..SL_COUNT {
                let head = h.heads[fl][sl];
                assert_eq!(h.sl_bitmap[fl] & (1 << sl) != 0, !head.is_null());
                let mut f = head;
                let mut back = ptr::null_mut();
                while !f.is_null() {
                    unsafe {
                        assert_eq!((*f).prev_free, back);
                        assert_eq!(class_floor((*f).block.size), (fl, sl));
                        back = f;
                        f = (*f).next_free;
                    }
                    listed += 1;
                }
            }
        }
        assert_eq!(listed, free_seen, "listed blocks that are in no region");
        assert_eq!(h.stats.free_blocks, free_seen);
        assert_eq!(h.stats.free, free_bytes);
        assert_eq!(h.stats.allocations, used_seen);
    }

    #[test]
    fn alloc_free_reuse_and_coalesce() {
        let r = Region::new(4096);
        let mut h = r.heap();
        assert_eq!(h.stats().free, 4096 - HEADER, "all but the sentinel");
        let a = h.allocate(l(100, 8)).unwrap();
        let b = h.allocate(l(200, 8)).unwrap();
        assert_eq!(h.stats().allocations, 2);
        assert!(a.as_ptr() < b.as_ptr());
        check(&h, &[r.start()]);
        unsafe { h.deallocate(a, l(100, 8)) };
        check(&h, &[r.start()]);
        unsafe { h.deallocate(b, l(200, 8)) };
        assert_eq!(h.stats().allocations, 0);
        assert_eq!(
            h.stats().free,
            4096 - HEADER,
            "everything returns to the heap"
        );
        assert_eq!(h.free_blocks(), 1, "blocks are coalesced");
        check(&h, &[r.start()]);
        // The same address is reused.
        assert_eq!(h.allocate(l(100, 8)).unwrap(), a);
    }

    #[test]
    fn respects_alignment_and_frees_without_the_layout() {
        let r = Region::new(64 * 1024);
        let mut h = r.heap();
        let mut live = Vec::new();
        for (i, align) in [1, 2, 8, 16, 32, 64, 256, 1024, 4096]
            .into_iter()
            .enumerate()
        {
            let pad = h.allocate(l(24 + i * 8, 8)).unwrap();
            let p = h.allocate(l(40, align)).unwrap();
            assert_eq!(p.as_ptr() as usize % align.max(GRANULE), 0, "align {align}");
            live.push(pad);
            live.push(p);
            check(&h, &[r.start()]);
        }
        for p in live {
            // The layout does not matter: the header rules.
            unsafe { h.deallocate(p, l(1, 1)) };
        }
        check(&h, &[r.start()]);
        assert_eq!(
            h.stats().free,
            64 * 1024 - HEADER,
            "no leaks with large alignments"
        );
        assert_eq!(h.free_blocks(), 1);
    }

    #[test]
    #[should_panic(expected = "free of a block that is not allocated")]
    fn a_second_free_is_caught_after_merging_backwards() {
        let r = Region::new(4096);
        let mut h = r.heap();
        let a = h.allocate(l(64, 8)).unwrap();
        let b = h.allocate(l(64, 8)).unwrap();
        let _pin = h.allocate(l(64, 8)).unwrap();
        unsafe {
            h.deallocate(a, l(64, 8));
            // `b` merges into the free `a` before it.
            h.deallocate(b, l(64, 8));
            h.deallocate(b, l(64, 8));
        }
    }

    #[test]
    fn zero_sized_allocations_are_distinct_and_aligned() {
        let r = Region::new(4096);
        let mut h = r.heap();
        let a = h.allocate(l(0, 1)).unwrap();
        let b = h.allocate(l(0, 1)).unwrap();
        let c = h.allocate(l(0, 256)).unwrap();
        assert!(a != b && b != c && a != c);
        assert_eq!(c.as_ptr() as usize % 256, 0);
        unsafe {
            h.deallocate(a, l(0, 1));
            h.deallocate(b, l(0, 1));
            h.deallocate(c, l(0, 256));
        }
        assert_eq!(h.free_blocks(), 1);
        check(&h, &[r.start()]);
    }

    #[test]
    fn out_of_memory_returns_none_and_keeps_state() {
        let r = Region::new(1024);
        let mut h = r.heap();
        assert!(h.allocate(l(2000, 8)).is_none());
        let a = h.allocate(l(800, 8)).unwrap();
        let before = h.stats();
        assert!(h.allocate(l(200, 8)).is_none());
        assert_eq!(h.stats(), before);
        check(&h, &[r.start()]);
        unsafe { h.deallocate(a, l(800, 8)) };
        assert_eq!(h.stats().free, 1024 - HEADER);
    }

    #[test]
    fn unrepresentable_requests_return_none_and_leave_the_heap_intact() {
        // Audit 9d8c7e5, finding 1: in the kernel's upper half a large
        // request or a huge alignment overflows `usize`; a wrapped value
        // would pass a fit check and write outside the heap.
        let r = Region::new(4096);
        let mut h = r.heap();
        assert!(search_size(usize::MAX - 15, GRANULE).is_none());
        assert!(search_size(usize::MAX / 2, 1 << 63).is_none());
        assert!(round_to_class(usize::MAX - 15).is_none());
        // A region for the largest layout does not wrap to a small one.
        assert!(region_size_for(l(isize::MAX as usize, 1)).unwrap() > isize::MAX as usize);
        let before = h.stats();
        assert!(h.allocate(l(isize::MAX as usize - 4096, 1)).is_none());
        assert!(h.allocate(l(16, 1 << 62)).is_none());
        assert_eq!(h.stats(), before);
        check(&h, &[r.start()]);
        // And the heap is still usable.
        assert!(h.allocate(l(64, 8)).is_some());
    }

    #[test]
    fn a_freed_block_of_the_requested_size_is_reused() {
        // Incident 0066: a 4096-byte buffer needs a 4112-byte block, which
        // good fit alone looks for among blocks of 4224 or more; probing
        // the list it falls in finds the block just freed.
        fn reused<const P: usize>() -> bool {
            let r = Region::new(64 * 1024);
            let page = l(4096, 8);
            let mut h = Heap::<P>::empty();
            unsafe { h.add_region(r.start(), r.size()) };
            let a = h.allocate(page).unwrap();
            let pin = h.allocate(l(8, 8)).unwrap();
            unsafe { h.deallocate(a, page) };
            let b = h.allocate(page).unwrap();
            check(&h, &[r.start()]);
            unsafe {
                h.deallocate(b, page);
                h.deallocate(pin, l(8, 8));
            }
            assert_eq!(h.free_blocks(), 1);
            a == b
        }
        assert!(!reused::<0>(), "pure good fit skips it");
        assert!(reused::<1>());
        assert!(reused::<DEFAULT_PROBES>());
    }

    #[test]
    fn region_size_for_is_enough_for_a_fresh_region() {
        for size in (1..70_000)
            .step_by(997)
            .chain([4096, 1 << 20, (1 << 20) + 1])
        {
            for align in [1, 16, 64, 4096] {
                let layout = l(size, align);
                let need = region_size_for(layout).unwrap().next_multiple_of(4096);
                let r = Region::new(need);
                let mut h = r.heap();
                assert!(h.allocate(layout).is_some(), "size {size} align {align}");
            }
        }
    }

    #[test]
    fn contiguous_regions_merge_and_separate_ones_do_not() {
        let r = Region::new(3 * 4096);
        let mut h: Heap = Heap::empty();
        unsafe {
            h.add_region(r.start(), 4096);
            h.add_region(r.start() + 4096, 4096);
        }
        assert_eq!(h.free_blocks(), 1, "the second region continues the first");
        assert_eq!(h.stats().free, 2 * 4096 - HEADER);
        // A block larger than either region fits.
        let big = h.allocate(l(6000, 8)).unwrap();
        check(&h, &[r.start()]);
        // More space after the free tail: it merges with that tail.
        unsafe { h.add_region(r.start() + 2 * 4096, 4096) };
        check(&h, &[r.start()]);
        assert_eq!(h.free_blocks(), 1);
        unsafe { h.deallocate(big, l(6000, 8)) };
        assert_eq!(h.stats().free, 3 * 4096 - HEADER);

        let other = Region::new(4096);
        unsafe { h.add_region(other.start(), 4096) };
        assert_eq!(h.free_blocks(), 2, "separate regions stay apart");
        assert_eq!(h.stats().total, 4 * 4096);
        check(&h, &[r.start(), other.start()]);
    }

    #[test]
    fn small_tail_is_absorbed() {
        // A free block of 4048 bytes; a request of 4032 leaves 16 after it,
        // less than a block: the allocation takes it.
        let r = Region::new(4064);
        let mut h = r.heap();
        let a = h.allocate(l(4032 - HEADER, 8)).unwrap();
        assert_eq!(h.free_blocks(), 0);
        check(&h, &[r.start()]);
        unsafe { h.deallocate(a, l(1, 1)) };
        assert_eq!(h.stats().free, 4064 - HEADER);
    }

    #[test]
    fn resize_in_place_grows_into_free_space_and_shrinks() {
        let r = Region::new(8192);
        let mut h = r.heap();
        let a = h.allocate(l(100, 8)).unwrap();
        unsafe { ptr::write_bytes(a.as_ptr(), 0xA5, 100) };
        // Grows into the free space after it, keeping its contents.
        assert!(unsafe { h.resize_in_place(a, 3000) });
        let kept = unsafe { core::slice::from_raw_parts(a.as_ptr(), 100) };
        assert!(kept.iter().all(|&b| b == 0xA5));
        check(&h, &[r.start()]);
        // Shrinks, giving the tail back.
        let free = h.stats().free;
        assert!(unsafe { h.resize_in_place(a, 200) });
        assert!(h.stats().free > free);
        check(&h, &[r.start()]);
        // A used block after it: it cannot grow, and nothing changes.
        let b = h.allocate(l(64, 8)).unwrap();
        let before = h.stats();
        assert!(!unsafe { h.resize_in_place(a, 1000) });
        assert_eq!(h.stats(), before);
        // Shrinking by less than a block, and growing within its own
        // rounding, succeed without changes.
        assert!(unsafe { h.resize_in_place(a, 190) });
        assert!(unsafe { h.resize_in_place(a, 200) });
        assert_eq!(h.stats(), before);
        check(&h, &[r.start()]);
        unsafe {
            h.deallocate(a, l(200, 8));
            h.deallocate(b, l(64, 8));
        }
        assert_eq!(h.free_blocks(), 1);
        check(&h, &[r.start()]);
    }

    /// xorshift64*, enough for test shuffles without a dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Random allocations, frees and resizes against a reference model of
    /// the live blocks: every block is aligned, inside a region, overlaps no
    /// other and keeps its contents; the invariants hold throughout and
    /// everything coalesces back at the end. Out of memory returns `None`
    /// with the heap unchanged.
    #[test]
    fn randomized_against_a_model() {
        for seed in 1..=4u64 {
            let a = Region::new(256 * 1024);
            let b = Region::new(256 * 1024);
            let mut h: Heap = Heap::empty();
            unsafe { h.add_region(a.start(), 128 * 1024) };
            let mut starts = std::vec![a.start()];
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            // address -> (size, align, fill byte)
            let mut model: BTreeMap<usize, (usize, usize, u8)> = BTreeMap::new();
            let in_regions = |p: usize, n: usize| {
                (p >= a.start() && p + n <= a.start() + a.size())
                    || (p >= b.start() && p + n <= b.start() + b.size())
            };
            for step in 0..20_000u32 {
                if step == 5_000 {
                    // Contiguous growth, then a separate region.
                    unsafe { h.add_region(a.start() + 128 * 1024, 128 * 1024) };
                }
                if step == 10_000 {
                    unsafe { h.add_region(b.start(), b.size()) };
                    starts.push(b.start());
                }
                let op = rng.below(10);
                if op < 5 || model.is_empty() {
                    let size = match rng.below(4) {
                        0 => rng.below(17) as usize,
                        1 | 2 => 1 + rng.below(512) as usize,
                        _ => 1 + rng.below(16 * 1024) as usize,
                    };
                    let align = 1 << rng.below(13);
                    let before = h.stats();
                    match h.allocate(l(size, align)) {
                        Some(p) => {
                            let p = p.as_ptr() as usize;
                            assert_eq!(p % align, 0);
                            assert!(in_regions(p, size));
                            if let Some((&q, &(n, _, _))) = model.range(..=p).next_back() {
                                assert!(q + n.max(1) <= p, "overlaps the block before");
                            }
                            if let Some((&q, _)) = model.range(p..).next() {
                                assert!(p + size.max(1) <= q, "overlaps the block after");
                            }
                            let fill = step as u8;
                            unsafe { ptr::write_bytes(p as *mut u8, fill, size) };
                            model.insert(p, (size, align, fill));
                        }
                        None => {
                            assert_eq!(h.stats(), before, "a failed allocation changed the heap")
                        }
                    }
                } else {
                    let i = rng.below(model.len() as u64) as usize;
                    let (&p, &(size, align, fill)) = model.iter().nth(i).unwrap();
                    let bytes = unsafe { core::slice::from_raw_parts(p as *const u8, size) };
                    assert!(bytes.iter().all(|&x| x == fill), "block {p:#x} overwritten");
                    let ptr = NonNull::new(p as *mut u8).unwrap();
                    if op < 8 {
                        unsafe { h.deallocate(ptr, l(size, align)) };
                        model.remove(&p);
                    } else {
                        let new = 1 + rng.below(8 * 1024) as usize;
                        let before = h.stats();
                        if unsafe { h.resize_in_place(ptr, new) } {
                            if let Some((&q, _)) = model.range(p + 1..).next() {
                                assert!(p + new <= q, "resized over the next block");
                            }
                            assert!(in_regions(p, new));
                            unsafe { ptr::write_bytes(ptr.as_ptr(), fill, new) };
                            model.insert(p, (new, align, fill));
                        } else {
                            assert_eq!(h.stats(), before, "a failed resize changed the heap");
                        }
                    }
                }
                if step % 997 == 0 {
                    check(&h, &starts);
                }
            }
            check(&h, &starts);
            for (&p, &(size, align, _)) in &model {
                unsafe { h.deallocate(NonNull::new(p as *mut u8).unwrap(), l(size, align)) };
            }
            check(&h, &starts);
            assert_eq!(h.free_blocks(), 2, "one free block per separate region");
            assert_eq!(h.stats().free, h.stats().total - 2 * HEADER);
        }
    }

    /// Incident 0066: with `holes` free blocks kept apart by live ones
    /// (what a large transaction leaves while it is being freed), the best
    /// time of a fixed batch of allocations, then frees in scrambled order.
    /// The batch's blocks are of a size no hole can serve.
    fn batch_time(holes: usize) -> Duration {
        const BATCH: usize = 1000;
        let r = Region::new(holes * 128 + BATCH * 512 + (1 << 20));
        let mut h = r.heap();
        let small = l(48, 8);
        let mut kept = Vec::with_capacity(holes);
        let mut freed = Vec::with_capacity(holes);
        for _ in 0..holes {
            freed.push(h.allocate(small).unwrap());
            kept.push(h.allocate(small).unwrap());
        }
        // Descending order keeps the old list allocator's setup linear.
        for &p in freed.iter().rev() {
            unsafe { h.deallocate(p, small) };
        }
        assert!(h.free_blocks() >= holes, "the holes are there");
        let big = l(256, 8);
        let mut order: Vec<usize> = (0..BATCH).collect();
        let mut rng = Rng(holes as u64 | 1);
        for i in (1..BATCH).rev() {
            order.swap(i, rng.below(i as u64 + 1) as usize);
        }
        let mut best = Duration::MAX;
        let mut batch = Vec::with_capacity(BATCH);
        for _ in 0..7 {
            let t = Instant::now();
            for _ in 0..BATCH {
                batch.push(h.allocate(big).unwrap());
            }
            for &i in &order {
                unsafe { h.deallocate(batch[i], big) };
            }
            best = best.min(t.elapsed());
            batch.clear();
        }
        for p in kept {
            unsafe { h.deallocate(p, small) };
        }
        assert_eq!(h.free_blocks(), 1);
        best
    }

    #[test]
    fn cost_does_not_grow_with_the_number_of_free_blocks() {
        let few = batch_time(1_000);
        let many = batch_time(100_000);
        // The list allocator this replaced walked the holes on each
        // operation: about 100x here. A bound this loose fails only for a
        // cost that grows with the holes, not for noise.
        assert!(
            many < few * 8 + Duration::from_millis(2),
            "1000 holes: {few:?}, 100000 holes: {many:?}"
        );
    }
}
