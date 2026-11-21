// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A copy of the firmware's memory map kept past boot, exact: what an
//! address was to the firmware (incident 0064, where a DMA fault's page
//! must be told apart as free RAM, boot services memory or reserved).
//! Unlike [`crate::RangeSet`] it never merges a gap to make room: an entry
//! that does not fit is counted, and a lookup that misses says so.

/// Up to `N` `[start, end)` ranges with the caller's kind code. A range
/// that touches or overlaps one of the same kind joins it.
pub struct KindMap<const N: usize> {
    ranges: [(u64, u64, u8); N],
    len: usize,
    dropped: u32,
}

impl<const N: usize> Default for KindMap<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// What a lookup found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The kind of the range holding the address.
    Of(u8),
    /// No kept range holds it, and none was dropped: the map left it out.
    Outside,
    /// No kept range holds it, but some did not fit: unknown.
    Unknown,
}

impl<const N: usize> KindMap<N> {
    pub const fn new() -> Self {
        Self {
            ranges: [(0, 0, 0); N],
            len: 0,
            dropped: 0,
        }
    }

    /// Ranges that did not fit.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    pub fn add(&mut self, start: u64, end: u64, kind: u8) {
        if start >= end {
            return;
        }
        if let Some(r) = self.ranges[..self.len]
            .iter_mut()
            .find(|r| r.2 == kind && start <= r.1 && end >= r.0)
        {
            r.0 = r.0.min(start);
            r.1 = r.1.max(end);
            return;
        }
        if self.len == N {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.ranges[self.len] = (start, end, kind);
        self.len += 1;
    }

    pub fn lookup(&self, addr: u64) -> Kind {
        match self.ranges[..self.len]
            .iter()
            .find(|r| addr >= r.0 && addr < r.1)
        {
            Some(r) => Kind::Of(r.2),
            None if self.dropped > 0 => Kind::Unknown,
            None => Kind::Outside,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_kinds_and_joins() {
        let mut m: KindMap<3> = KindMap::new();
        m.add(0x10_0000, 0x20_0000, 1);
        // A short boot services range between free ones keeps its kind.
        m.add(0x20_0000, 0x20_1000, 2);
        m.add(0x20_1000, 0x40_0000, 1);
        m.add(0x40_0000, 0x40_0000, 9);
        assert_eq!(m.lookup(0x20_0800), Kind::Of(2));
        assert_eq!(m.lookup(0x30_0000), Kind::Of(1));
        assert_eq!(m.lookup(0x50_0000), Kind::Outside);
        // Joined to a range of its kind that it touches, not appended.
        m.add(0x40_0000, 0x50_0000, 1);
        assert_eq!(m.lookup(0x48_0000), Kind::Of(1));
        assert_eq!(m.dropped(), 0);
        // The map is full: a new range is counted, and a miss is unknown.
        m.add(0x90_0000, 0x91_0000, 5);
        assert_eq!(m.dropped(), 1);
        assert_eq!(m.lookup(0x90_0000), Kind::Unknown);
        assert_eq!(m.lookup(0x20_0800), Kind::Of(2));
    }
}
