//! Bitmap allocator of 4 KiB physical frames (Phase 5).
//!
//! One bit per frame, 1 = used. The bitmap storage is provided by the
//! caller (in the kernel, a free region of the UEFI map), so the crate
//! needs no `alloc` and is tested on the host with a `Vec<u64>`.
//!
//! Policy: everything starts used; the kernel explicitly frees the
//! `Free` regions of the map and reserves again what must not be handed out
//! (frame 0, low memory, the bitmap itself).

#![no_std]

pub const FRAME_SIZE: u64 = 4096;

/// A physical frame, identified by its base address (a multiple of 4 KiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame(u64);

impl Frame {
    /// # Panics
    /// If `addr` is not frame aligned.
    pub const fn from_addr(addr: u64) -> Self {
        assert!(addr.is_multiple_of(FRAME_SIZE), "address not frame aligned");
        Frame(addr)
    }
    pub const fn containing(addr: u64) -> Self {
        Frame(addr & !(FRAME_SIZE - 1))
    }
    pub const fn from_number(n: u64) -> Self {
        Frame(n * FRAME_SIZE)
    }
    pub const fn addr(self) -> u64 {
        self.0
    }
    pub const fn number(self) -> u64 {
        self.0 / FRAME_SIZE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmmError {
    /// The frame is outside the managed range.
    OutOfRange,
    /// The frame was already free.
    DoubleFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Addressable frames (bitmap size, holes included).
    pub total_frames: usize,
    /// RAM frames handed to the allocator with `free_range`.
    pub usable_frames: usize,
    pub free_frames: usize,
}

impl Stats {
    /// Usable RAM frames currently in use.
    pub const fn used_frames(&self) -> usize {
        self.usable_frames - self.free_frames
    }
    pub const fn free_bytes(&self) -> u64 {
        self.free_frames as u64 * FRAME_SIZE
    }
    pub const fn total_bytes(&self) -> u64 {
        self.total_frames as u64 * FRAME_SIZE
    }
}

pub struct BitmapAllocator<'a> {
    /// 1 = used. Bits beyond `frames` are always 1.
    bits: &'a mut [u64],
    frames: usize,
    /// RAM frames handed over with `free_range` (for statistics).
    usable: usize,
    free: usize,
    /// First word that may contain a free bit (heuristic, never greater
    /// than the real first one).
    hint: usize,
}

impl<'a> BitmapAllocator<'a> {
    /// 64-bit words needed for `frames` frames.
    pub const fn words_for(frames: usize) -> usize {
        frames.div_ceil(64)
    }

    /// Bytes needed for the bitmap of `frames` frames.
    pub const fn bytes_for(frames: usize) -> usize {
        Self::words_for(frames) * 8
    }

    /// Manages frames `0..frames`, all marked as used.
    ///
    /// # Panics
    /// If `bits` is too small.
    pub fn new(bits: &'a mut [u64], frames: usize) -> Self {
        assert!(bits.len() >= Self::words_for(frames), "bitmap too small");
        bits.fill(u64::MAX);
        Self {
            bits,
            frames,
            usable: 0,
            free: 0,
            hint: 0,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn stats(&self) -> Stats {
        Stats {
            total_frames: self.frames,
            usable_frames: self.usable,
            free_frames: self.free,
        }
    }

    fn in_range(&self, f: Frame) -> bool {
        (f.number() as usize) < self.frames
    }

    fn get(&self, n: usize) -> bool {
        self.bits[n / 64] & (1 << (n % 64)) != 0
    }

    fn set(&mut self, n: usize, used: bool) {
        let (w, b) = (n / 64, 1u64 << (n % 64));
        let was = self.bits[w] & b != 0;
        if used {
            self.bits[w] |= b;
        } else {
            self.bits[w] &= !b;
            if w < self.hint {
                self.hint = w;
            }
        }
        match (was, used) {
            (true, false) => self.free += 1,
            (false, true) => self.free -= 1,
            _ => {}
        }
    }

    /// Marks as free the frames fully contained in `[start, end)`.
    /// Partial frames at the edges stay used. Anything out of range is ignored.
    pub fn free_range(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let first = start.div_ceil(FRAME_SIZE) as usize;
        let last = (end / FRAME_SIZE) as usize; // exclusive
        for n in first..last.min(self.frames) {
            if self.get(n) {
                self.usable += 1;
            }
            self.set(n, false);
        }
    }

    /// Marks as used every frame touching `[start, end)`.
    pub fn reserve_range(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let first = (start / FRAME_SIZE) as usize;
        let last = end.div_ceil(FRAME_SIZE) as usize;
        for n in first..last.min(self.frames) {
            self.set(n, true);
        }
    }

    pub fn is_free(&self, f: Frame) -> bool {
        self.in_range(f) && !self.get(f.number() as usize)
    }

    /// Hands out the lowest free frame.
    pub fn allocate(&mut self) -> Option<Frame> {
        let words = self.bits.len();
        for w in self.hint..words {
            let word = self.bits[w];
            if word != u64::MAX {
                let bit = (!word).trailing_zeros() as usize;
                let n = w * 64 + bit;
                if n >= self.frames {
                    return None;
                }
                self.hint = w;
                self.set(n, true);
                return Some(Frame::from_number(n as u64));
            }
        }
        None
    }

    /// `count` consecutive frames whose first one is aligned to `align_frames`
    /// (a power of two, >= 1). Linear search: for large, rare allocations
    /// (DMA, initial tables), not for the fast path.
    pub fn allocate_contiguous(&mut self, count: usize, align_frames: usize) -> Option<Frame> {
        assert!(count > 0 && align_frames.is_power_of_two());
        let mut start = self.hint * 64;
        start = start.next_multiple_of(align_frames);
        while start + count <= self.frames {
            match (start..start + count).find(|&n| self.get(n)) {
                None => {
                    for n in start..start + count {
                        self.set(n, true);
                    }
                    return Some(Frame::from_number(start as u64));
                }
                Some(busy) => start = (busy + 1).next_multiple_of(align_frames),
            }
        }
        None
    }

    pub fn free(&mut self, f: Frame) -> Result<(), PmmError> {
        if !self.in_range(f) {
            return Err(PmmError::OutOfRange);
        }
        let n = f.number() as usize;
        if !self.get(n) {
            return Err(PmmError::DoubleFree);
        }
        self.set(n, false);
        Ok(())
    }

    /// Frees `count` consecutive frames starting at `first`.
    pub fn free_contiguous(&mut self, first: Frame, count: usize) -> Result<(), PmmError> {
        for i in 0..count as u64 {
            self.free(Frame::from_number(first.number() + i))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn pmm(frames: usize) -> (Vec<u64>, usize) {
        (vec![0u64; BitmapAllocator::words_for(frames)], frames)
    }

    #[test]
    fn starts_fully_reserved_then_frees_whole_frames_only() {
        let (mut bits, n) = pmm(100);
        let mut p = BitmapAllocator::new(&mut bits, n);
        assert_eq!(p.stats().free_frames, 0);
        assert!(p.allocate().is_none());
        // [0x1800, 0x8800): whole frames 2..=7 (6 frames); partial edges left out.
        p.free_range(0x1800, 0x8800);
        assert_eq!(p.stats().free_frames, 6);
        assert_eq!(p.stats().usable_frames, 6);
        assert_eq!(p.stats().total_frames, 100);
        // Repeating the range does not count usable RAM twice.
        p.free_range(0x1800, 0x8800);
        assert_eq!(p.stats().usable_frames, 6);
        assert!(!p.is_free(Frame::from_number(1)));
        assert!(p.is_free(Frame::from_number(2)));
        assert!(p.is_free(Frame::from_number(7)));
        assert!(!p.is_free(Frame::from_number(8)));
    }

    #[test]
    fn allocate_returns_lowest_and_unique_frames() {
        let (mut bits, n) = pmm(64 * 3);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, n as u64 * FRAME_SIZE);
        let a = p.allocate().unwrap();
        let b = p.allocate().unwrap();
        assert_eq!(a, Frame::from_number(0));
        assert_eq!(b, Frame::from_number(1));
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(a);
        seen.insert(b);
        while let Some(f) = p.allocate() {
            assert!(seen.insert(f), "repeated frame {f:?}");
        }
        assert_eq!(seen.len(), n);
        assert_eq!(p.stats().free_frames, 0);
    }

    #[test]
    fn free_and_double_free() {
        let (mut bits, n) = pmm(10);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, 10 * FRAME_SIZE);
        let f = p.allocate().unwrap();
        assert!(p.free(f).is_ok());
        assert_eq!(p.free(f), Err(PmmError::DoubleFree));
        assert_eq!(p.free(Frame::from_number(99)), Err(PmmError::OutOfRange));
        // After freeing, the same frame is the first one again.
        assert_eq!(p.allocate(), Some(f));
    }

    #[test]
    fn reserve_range_rounds_outward_and_hint_moves_back_on_free() {
        let (mut bits, n) = pmm(200);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, 200 * FRAME_SIZE);
        p.reserve_range(0x1001, 0x2FFF); // touches frames 1 and 2
        assert!(p.is_free(Frame::from_number(0)));
        assert!(!p.is_free(Frame::from_number(1)));
        assert!(!p.is_free(Frame::from_number(2)));
        assert!(p.is_free(Frame::from_number(3)));
        // Exhausting the first 130 frames moves the hint; freeing a low one brings it back.
        let first = p.allocate().unwrap();
        for _ in 0..129 {
            p.allocate().unwrap();
        }
        p.free(first).unwrap();
        assert_eq!(p.allocate(), Some(first));
    }

    #[test]
    fn contiguous_allocation_respects_alignment_and_gaps() {
        let (mut bits, n) = pmm(64);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, 64 * FRAME_SIZE);
        p.reserve_range(3 * FRAME_SIZE, 4 * FRAME_SIZE); // hole at frame 3
        let f = p.allocate_contiguous(4, 4).unwrap();
        assert_eq!(f, Frame::from_number(4), "0..4 has frame 3 in use");
        let g = p.allocate_contiguous(2, 1).unwrap();
        assert_eq!(g, Frame::from_number(0));
        assert_eq!(p.stats().used_frames(), 7);
        assert!(p.allocate_contiguous(100, 1).is_none());
        p.free_contiguous(f, 4).unwrap();
        assert!(p.is_free(Frame::from_number(7)));
    }

    #[test]
    fn sizes_and_frame_helpers() {
        assert_eq!(BitmapAllocator::words_for(1), 1);
        assert_eq!(BitmapAllocator::words_for(64), 1);
        assert_eq!(BitmapAllocator::words_for(65), 2);
        assert_eq!(BitmapAllocator::bytes_for(1 << 20), 128 * 1024); // 4 GiB -> 128 KiB
        assert_eq!(Frame::containing(0x1FFF), Frame::from_number(1));
        assert_eq!(Frame::from_number(5).addr(), 5 * FRAME_SIZE);
    }

    #[test]
    #[should_panic(expected = "too small")]
    fn rejects_small_bitmap() {
        let mut bits = [0u64; 1];
        BitmapAllocator::new(&mut bits, 65);
    }
}
