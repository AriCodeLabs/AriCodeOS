// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A fixed-size set of physical ranges, kept sorted and coalesced: what the
//! firmware map called RAM, kept past boot (ADR 0051). No `alloc`: it is
//! filled before the kernel heap exists.

/// Up to `N` disjoint `[start, end)` ranges, sorted by address. Ranges that
/// touch or overlap are merged, whatever order they arrive in (the UEFI map
/// is not guaranteed sorted). With every slot taken, the two closest
/// neighbours are merged to make room: the set then covers a little more
/// than it was given, never less, and `approximated` says so.
pub struct RangeSet<const N: usize> {
    ranges: [(u64, u64); N],
    len: usize,
    approximated: bool,
}

impl<const N: usize> Default for RangeSet<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RangeSet<N> {
    pub const fn new() -> Self {
        Self {
            ranges: [(0, 0); N],
            len: 0,
            approximated: false,
        }
    }

    pub fn as_slice(&self) -> &[(u64, u64)] {
        &self.ranges[..self.len]
    }

    /// Whether some gap between ranges was filled to make room.
    pub fn approximated(&self) -> bool {
        self.approximated
    }

    pub fn add(&mut self, start: u64, end: u64) {
        if start >= end || N == 0 {
            return;
        }
        // First range ending at or after `start`: the new one merges from
        // there with every range it touches.
        let first = self.ranges[..self.len].partition_point(|r| r.1 < start);
        let mut last = first;
        let (mut s, mut e) = (start, end);
        while last < self.len && self.ranges[last].0 <= e {
            s = s.min(self.ranges[last].0);
            e = e.max(self.ranges[last].1);
            last += 1;
        }
        if last > first {
            // Replace ranges[first..last] with the merged one.
            self.ranges[first] = (s, e);
            self.ranges.copy_within(last..self.len, first + 1);
            self.len -= last - first - 1;
            return;
        }
        if self.len == N {
            self.merge_closest();
            return self.add(start, end);
        }
        self.ranges.copy_within(first..self.len, first + 1);
        self.ranges[first] = (s, e);
        self.len += 1;
    }

    /// Merges the two neighbours with the smallest gap between them.
    fn merge_closest(&mut self) {
        let Some(i) = (0..self.len.saturating_sub(1))
            .min_by_key(|&i| self.ranges[i + 1].0 - self.ranges[i].1)
        else {
            return;
        };
        self.ranges[i].1 = self.ranges[i + 1].1;
        self.ranges.copy_within(i + 2..self.len, i + 1);
        self.len -= 1;
        self.approximated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_and_merged_in_any_order() {
        let mut r = RangeSet::<8>::new();
        // The audit's case: high memory first, then low memory.
        r.add(0x1_0000_0000, 0x2_0000_0000);
        r.add(0, 0x9_F000);
        r.add(0x10_0000, 0xC000_0000);
        assert_eq!(
            r.as_slice(),
            [
                (0, 0x9_F000),
                (0x10_0000, 0xC000_0000),
                (0x1_0000_0000, 0x2_0000_0000)
            ]
        );
        // Touching and overlapping ranges merge; the PCI hole stays a hole.
        r.add(0x9_F000, 0xA_0000);
        r.add(0x8000_0000, 0xC000_1000);
        r.add(0x50_0000, 0x60_0000);
        assert_eq!(
            r.as_slice(),
            [
                (0, 0xA_0000),
                (0x10_0000, 0xC000_1000),
                (0x1_0000_0000, 0x2_0000_0000)
            ]
        );
        // One range bridging several.
        r.add(0x9_0000, 0x1_0000_0000);
        assert_eq!(r.as_slice(), [(0, 0x2_0000_0000)]);
        assert!(!r.approximated());
        r.add(5, 5);
        assert_eq!(r.as_slice().len(), 1);
    }

    #[test]
    fn full_set_fills_the_smallest_gap() {
        let mut r = RangeSet::<3>::new();
        r.add(0, 0x1000);
        r.add(0x2000, 0x3000);
        r.add(0x1_0000_0000, 0x1_0000_1000);
        r.add(0x5000, 0x6000);
        // The 0x1000 gap was filled, not the one across 4 GiB.
        assert_eq!(
            r.as_slice(),
            [
                (0, 0x3000),
                (0x5000, 0x6000),
                (0x1_0000_0000, 0x1_0000_1000)
            ]
        );
        assert!(r.approximated());
    }
}
