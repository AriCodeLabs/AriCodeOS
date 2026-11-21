// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Where a page a device faulted on lies in its domain (incident 0064):
//! inside a region still mapped, inside one unmapped earlier (and when),
//! or between the nearest regions below and above it. A fault record
//! only names the page (VT-d 4.0, 11.4.14: bits 63:12 of the request);
//! this is what lets a report tell a stale pointer from a read past the
//! end of a buffer or from memory the driver never had.

/// A region of a domain: identity-mapped `[start, start + len)`, what it
/// holds (`tag`, the caller's code) and when it was mapped and unmapped
/// (ms since boot; `unmapped_ms` 0 while it is mapped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    pub start: u64,
    pub len: u64,
    pub tag: u8,
    pub mapped_ms: u32,
    pub unmapped_ms: u32,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.len)
    }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end()
    }
}

/// The last `N` regions a domain unmapped, oldest overwritten first, and
/// how many it unmapped in all.
#[derive(Debug, Clone, Copy)]
pub struct History<const N: usize> {
    entries: [Region; N],
    next: usize,
    total: u32,
}

impl<const N: usize> Default for History<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> History<N> {
    pub const fn new() -> Self {
        Self {
            entries: [Region {
                start: 0,
                len: 0,
                tag: 0,
                mapped_ms: 0,
                unmapped_ms: 0,
            }; N],
            next: 0,
            total: 0,
        }
    }

    pub fn push(&mut self, r: Region) {
        if N == 0 {
            return;
        }
        self.entries[self.next % N] = r;
        self.next = (self.next + 1) % N;
        self.total = self.total.saturating_add(1);
    }

    /// Regions unmapped since the domain was made.
    pub fn total(&self) -> u32 {
        self.total
    }

    /// The kept entries, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Region> {
        let kept = (self.total as usize).min(N);
        let first = if kept < N { 0 } else { self.next };
        (0..kept).map(move |i| &self.entries[(first + i) % N])
    }
}

/// How the page relates to the domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Inside a region still mapped: the fault is not a missing page
    /// (a permission, or a table the unit read wrong).
    Mapped = 1,
    /// Inside a region the domain unmapped earlier and maps no more.
    Unmapped = 2,
    /// In no region the domain has or remembers having.
    Outside = 3,
    /// The domain has no region at all (or there is no domain).
    NoRegions = 4,
}

impl Relation {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Mapped),
            2 => Some(Self::Unmapped),
            3 => Some(Self::Outside),
            4 => Some(Self::NoRegions),
            _ => None,
        }
    }
}

/// Where the page lies: the region it is in (mapped, or the latest
/// unmapped one that held it) and the nearest live regions below and
/// above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site {
    pub relation: Relation,
    pub region: Option<Region>,
    pub below: Option<Region>,
    pub above: Option<Region>,
}

/// Locates `page` (page aligned) among the domain's `live` regions and the
/// `unmapped` ones it remembers, in any order: the one unmapped last that
/// held it wins (the later one on a tie).
pub fn locate<'a>(
    page: u64,
    live: impl Iterator<Item = Region> + Clone,
    unmapped: impl Iterator<Item = &'a Region>,
) -> Site {
    let mut below: Option<Region> = None;
    let mut above: Option<Region> = None;
    let mut any = false;
    for r in live.clone() {
        any = true;
        if r.contains(page) {
            return Site {
                relation: Relation::Mapped,
                region: Some(r),
                below: None,
                above: None,
            };
        }
        if r.end() <= page {
            if below.is_none_or(|b| r.end() > b.end()) {
                below = Some(r);
            }
        } else if r.start > page && above.is_none_or(|a| r.start < a.start) {
            above = Some(r);
        }
    }
    let gone = unmapped
        .filter(|r| r.contains(page))
        .max_by_key(|r| r.unmapped_ms)
        .copied();
    let relation = match (gone, any) {
        (Some(_), _) => Relation::Unmapped,
        (None, true) => Relation::Outside,
        (None, false) => Relation::NoRegions,
    };
    Site {
        relation,
        region: gone,
        below,
        above,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(start: u64, len: u64, tag: u8) -> Region {
        Region {
            start,
            len,
            tag,
            mapped_ms: 100,
            unmapped_ms: 0,
        }
    }

    #[test]
    fn inside_a_mapped_region() {
        let live = [r(0x10000, 0x3000, 1), r(0x20000, 0x1000, 2)];
        let s = locate(0x12000, live.iter().copied(), [].iter());
        assert_eq!(s.relation, Relation::Mapped);
        assert_eq!(s.region, Some(live[0]));
    }

    #[test]
    fn between_regions_names_the_nearest_on_each_side() {
        let live = [
            r(0x10000, 0x3000, 1),
            r(0x30000, 0x1000, 2),
            r(0x14000, 0x2000, 3),
            r(0x50000, 0x1000, 4),
        ];
        let s = locate(0x20000, live.iter().copied(), [].iter());
        assert_eq!(s.relation, Relation::Outside);
        assert_eq!(s.below, Some(live[2]));
        assert_eq!(s.above, Some(live[1]));
        // The page right after a region's end: that region is below it.
        let s = locate(0x16000, live.iter().copied(), [].iter());
        assert_eq!(s.below, Some(live[2]));
        // Below every region and above every region.
        let s = locate(0x1000, live.iter().copied(), [].iter());
        assert_eq!((s.below, s.above), (None, Some(live[0])));
        let s = locate(0x90000, live.iter().copied(), [].iter());
        assert_eq!((s.below, s.above), (Some(live[3]), None));
    }

    #[test]
    fn unmapped_regions_are_remembered_latest_first() {
        let mut h: History<3> = History::new();
        let mut gone = r(0x40000, 0x2000, 2);
        gone.unmapped_ms = 500;
        h.push(gone);
        let mut again = gone;
        again.mapped_ms = 600;
        again.unmapped_ms = 900;
        h.push(again);
        let live = [r(0x10000, 0x1000, 1)];
        let s = locate(0x41000, live.iter().copied(), h.iter());
        assert_eq!(s.relation, Relation::Unmapped);
        assert_eq!(s.region, Some(again));
        // In any order: the one unmapped last wins.
        let s = locate(0x41000, live.iter().copied(), [again, gone].iter());
        assert_eq!(s.region, Some(again));
        assert_eq!(s.below, Some(live[0]));
        // Without live regions the remembered one still answers.
        let s = locate(0x40000, [].iter().copied(), h.iter());
        assert_eq!(s.relation, Relation::Unmapped);
        let s = locate(0x80000, [].iter().copied(), h.iter());
        assert_eq!(s.relation, Relation::NoRegions);
    }

    #[test]
    fn history_is_bounded_and_counts_everything() {
        let mut h: History<2> = History::new();
        for i in 0..5u64 {
            h.push(r(i * 0x1000, 0x1000, i as u8));
        }
        assert_eq!(h.total(), 5);
        let kept: [u8; 2] = core::array::from_fn(|i| h.iter().nth(i).unwrap().tag);
        assert_eq!(kept, [3, 4], "oldest first, only the last two");
        assert_eq!(h.iter().count(), 2);
        let mut empty: History<0> = History::new();
        empty.push(r(0, 0x1000, 0));
        assert_eq!(empty.iter().count(), 0);
    }
}
