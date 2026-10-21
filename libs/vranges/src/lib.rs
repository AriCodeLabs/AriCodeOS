// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Virtual address ranges handed out from `[base, end)` and given back
//! (ADR 0038). A freed range is reused first-fit (and split), adjacent free
//! ranges merge, and a free range that reaches the top of what was ever
//! handed out lowers it again. A region mapped and unmapped over and over
//! therefore keeps landing on the same addresses, so the page tables that
//! cover them are reused instead of new ones being taken for every cycle:
//! what the tables cost is bounded by the largest set of ranges in use at
//! once, not by how many were ever handed out.
//!
//! Sizes and addresses are whatever unit the caller uses (pages in bytes
//! for the kernel); the allocator never rounds.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtRanges {
    /// Lowest address never handed out (everything above is free).
    next: u64,
    end: u64,
    /// Free ranges below `next`: `(start, len)`, sorted, never adjacent.
    free: Vec<(u64, u64)>,
}

impl VirtRanges {
    pub const fn new(base: u64, end: u64) -> Self {
        Self {
            next: base,
            end,
            free: Vec::new(),
        }
    }

    /// Start of a free range of `len`, or `None` if none fits.
    pub fn alloc(&mut self, len: u64) -> Option<u64> {
        if len == 0 {
            return None;
        }
        if let Some(i) = self.free.iter().position(|&(_, l)| l >= len) {
            let (start, l) = self.free[i];
            if l == len {
                self.free.remove(i);
            } else {
                self.free[i] = (start + len, l - len);
            }
            return Some(start);
        }
        let start = self.next;
        let top = start.checked_add(len).filter(|&t| t <= self.end)?;
        self.next = top;
        Some(start)
    }

    /// Gives back `[start, start+len)`, which `alloc` handed out.
    pub fn free(&mut self, start: u64, len: u64) {
        if len == 0 {
            return;
        }
        let i = self.free.partition_point(|&(s, _)| s < start);
        self.free.insert(i, (start, len));
        // Merge with the next one, then with the previous one.
        if i + 1 < self.free.len() && start + len == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == start {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
        // A free range that ends at the top lowers it.
        if let Some(&(s, l)) = self.free.last() {
            if s + l == self.next {
                self.next = s;
                self.free.pop();
            }
        }
    }

    /// Lowest address never handed out: the high-water mark of the ranges
    /// in use.
    pub fn top(&self) -> u64 {
        self.next
    }

    /// Free ranges below the top (for tests and diagnostics).
    pub fn holes(&self) -> usize {
        self.free.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = 0x1000;

    #[test]
    fn map_unmap_cycles_reuse_the_same_range() {
        let mut r = VirtRanges::new(0x1000_0000, 0x2000_0000);
        let keep = r.alloc(4 * P).unwrap();
        for _ in 0..1_000_000 {
            let a = r.alloc(16 * P).unwrap();
            assert_eq!(a, keep + 4 * P);
            r.free(a, 16 * P);
        }
        assert_eq!(r.top(), keep + 4 * P, "the top never moved");
        assert_eq!(r.holes(), 0);
    }

    #[test]
    fn first_fit_splits_and_merges() {
        let mut r = VirtRanges::new(0, 100 * P);
        let a = r.alloc(4 * P).unwrap();
        let b = r.alloc(4 * P).unwrap();
        let c = r.alloc(4 * P).unwrap();
        assert_eq!((a, b, c), (0, 4 * P, 8 * P));
        r.free(a, 4 * P);
        // Smaller request splits the hole at a.
        assert_eq!(r.alloc(P), Some(0));
        assert_eq!(r.alloc(3 * P), Some(P));
        assert_eq!(r.holes(), 0);
        // Freeing b and the two pieces of a merges them into one hole.
        r.free(b, 4 * P);
        r.free(0, P);
        assert_eq!(r.holes(), 2);
        r.free(P, 3 * P);
        assert_eq!(r.holes(), 1);
        assert_eq!(r.alloc(8 * P), Some(0));
        // Freeing the topmost range lowers the top, and with it the holes
        // right below it.
        r.free(0, 8 * P);
        r.free(c, 4 * P);
        assert_eq!((r.top(), r.holes()), (0, 0));
    }

    #[test]
    fn the_end_is_respected() {
        let mut r = VirtRanges::new(0, 8 * P);
        assert_eq!(r.alloc(8 * P), Some(0));
        assert_eq!(r.alloc(P), None);
        assert_eq!(r.alloc(0), None);
        r.free(0, 8 * P);
        assert_eq!(r.alloc(9 * P), None);
        assert_eq!(r.alloc(8 * P), Some(0));
        let mut wrap = VirtRanges::new(u64::MAX - P, u64::MAX);
        assert_eq!(wrap.alloc(2 * P), None, "no overflow");
    }
}
