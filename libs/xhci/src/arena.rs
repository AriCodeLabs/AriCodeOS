// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The driver's DMA arena: where each structure goes, so that none spans
//! a boundary the controller may not cross, and a log of what went where,
//! so that the page of a DMA fault can be named (incident 0064).
//!
//! xHCI 1.2, chapter 6 before Table 6-1: no structure of at most 64 KiB
//! may span a 64 KiB boundary, and none of at most PAGESIZE may span a
//! PAGESIZE boundary; Table 6-1 gives the Device Context (2048 bytes) and
//! each context of an Input Context the PAGESIZE boundary, and the ring
//! segments the 64 KiB one. A controller may count addresses within those
//! boundaries only (Linux allocates its contexts from a pool with a
//! PAGESIZE boundary and whole-page ring segments; FreeBSD gives every
//! context its own page), so a structure across one is read wrapped,
//! from memory it does not hold.

/// The page size every structure here is placed for: the controller's
/// PAGESIZE must include it (`page_size_supported`).
pub const PAGE: u64 = 4096;
/// The other boundary of xHCI 1.2, chapter 6.
pub const BOUNDARY_64K: u64 = 64 * 1024;

/// Whether PAGESIZE (operational register 08h, xHCI 1.2 5.4.3: bit n set
/// means pages of 2^(n+12) bytes) includes the 4 KiB pages the arena is
/// laid out for.
pub fn page_size_supported(pagesize: u32) -> bool {
    pagesize & 1 != 0
}

/// Whether `[phys, phys + len)` spans no boundary it must not span.
pub fn within_boundaries(phys: u64, len: u64) -> bool {
    let fits = |b: u64| len > b || phys % b + len <= b;
    len == 0 || (fits(PAGE) && fits(BOUNDARY_64K))
}

/// The first offset at or after `next`, aligned to `align` (a power of two
/// of at most a page), where `len` bytes of a buffer at physical `base`
/// span no boundary they must not span.
pub fn place(base: u64, next: u64, len: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two() && align <= PAGE);
    let mut off = next.next_multiple_of(align);
    for b in [PAGE, BOUNDARY_64K] {
        let phys = base + off;
        if len <= b && phys % b + len > b {
            // The next boundary: a multiple of `align`, so still aligned.
            off = (phys / b + 1) * b - base;
        }
    }
    off
}

/// A structure placed in the arena: its offset, length and what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub off: u64,
    pub len: u64,
    pub label: &'static str,
}

impl Span {
    pub fn end(&self) -> u64 {
        self.off + self.len
    }
}

/// The arena's structures in the order they were placed, up to `N`
/// (contiguous ones with the same label, such as the scratchpad pages,
/// make one); `dropped` counts those that did not fit in the log.
#[derive(Debug, Clone, Copy)]
pub struct SpanLog<const N: usize> {
    spans: [Option<Span>; N],
    len: usize,
    pub dropped: u32,
}

impl<const N: usize> Default for SpanLog<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a page lies against the arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Located {
    /// The page ends `distance` bytes before the arena starts.
    Before { distance: u64, first: Option<Span> },
    /// The page starts `distance` bytes past the arena's end; `last` is
    /// the structure placed highest.
    After { distance: u64, last: Option<Span> },
    /// Inside the arena, `offset` bytes from its start: `first` the lowest
    /// structure on the page and `count` how many share it; none if the
    /// page holds no logged structure (`used` says whether it is past the
    /// part handed out).
    Inside {
        offset: u64,
        first: Option<Span>,
        count: usize,
        used: bool,
    },
}

impl<const N: usize> SpanLog<N> {
    pub const fn new() -> Self {
        Self {
            spans: [None; N],
            len: 0,
            dropped: 0,
        }
    }

    pub fn record(&mut self, off: u64, len: u64, label: &'static str) {
        if let Some(Some(last)) = self.len.checked_sub(1).map(|i| &mut self.spans[i]) {
            if last.label == label && last.end() == off {
                last.len += len;
                return;
            }
        }
        if self.len == N {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.spans[self.len] = Some(Span { off, len, label });
        self.len += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = &Span> {
        self.spans[..self.len].iter().flatten()
    }

    /// Locates the page starting `page_off` bytes from the arena's start
    /// (negative: before it) in an arena of `arena_len` bytes of which
    /// `used` were handed out.
    pub fn locate(&self, page_off: i64, arena_len: u64, used: u64) -> Located {
        if page_off < 0 {
            let start = page_off.unsigned_abs();
            return Located::Before {
                distance: start.saturating_sub(PAGE),
                first: self.iter().min_by_key(|s| s.off).copied(),
            };
        }
        let off = page_off as u64;
        if off >= arena_len {
            return Located::After {
                distance: off - arena_len,
                last: self.iter().max_by_key(|s| s.end()).copied(),
            };
        }
        let on_page = || self.iter().filter(|s| s.off < off + PAGE && s.end() > off);
        Located::Inside {
            offset: off,
            first: on_page().min_by_key(|s| s.off).copied(),
            count: on_page().count(),
            used: off < used,
        }
    }
}

/// An event name for `located` (at most 24 bytes): `usb dma before
/// arena`, `usb dma past arena`, `usb dma in LABEL` or `usb dma arena
/// unused`.
pub fn event_name<'a>(located: &Located, out: &'a mut [u8; 24]) -> &'a str {
    let text: &[&str] = match located {
        Located::Before { .. } => &["usb dma before arena"],
        Located::After { .. } => &["usb dma past arena"],
        Located::Inside { first: Some(s), .. } => &["usb dma in ", s.label],
        Located::Inside { first: None, .. } => &["usb dma arena unused"],
    };
    let mut n = 0;
    for part in text {
        for &b in part.as_bytes() {
            if n == out.len() {
                break;
            }
            out[n] = b;
            n += 1;
        }
    }
    // Labels are ASCII: a cut never splits a character.
    core::str::from_utf8(&out[..n]).unwrap_or("usb dma")
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn placement_keeps_contexts_in_a_page_and_rings_off_64k() {
        let base = 0x43B7_7000;
        // A 2048-byte Device Context at offset 3008 would cross the page.
        assert_eq!(place(base, 3008, 2048, 64), 4096);
        // One that fits stays where it is.
        assert_eq!(place(base, 1344, 1024, 64), 1344);
        // A 256-byte ring ending exactly on the page boundary is fine.
        assert_eq!(place(base, 3840, 256, 64), 3840);
        assert_eq!(place(base, 3904, 256, 64), 4096);
        // The Input Context of 64-byte contexts (2112 bytes) fits a page.
        let off = place(base, 2100, 2112, 64);
        assert!(off % PAGE + 2112 <= PAGE, "{off}");
        // Alignment first, then the boundaries.
        assert_eq!(place(base, 1, 16, 64), 64);
        // A structure larger than a page but under 64 KiB only keeps off
        // the 64 KiB boundary: the arena at 0x43b77000 meets one 0x9000 in.
        assert_eq!(place(base, 0x8000, 0x2000, 4096), 0x9000);
        assert_eq!(place(base, 0x6000, 0x2000, 4096), 0x6000);
        // Larger than 64 KiB: nothing to keep off.
        assert_eq!(place(base, 0x100, 0x20000, 64), 0x100);
    }

    #[test]
    fn every_placement_respects_the_boundaries() {
        for phase in (0..BOUNDARY_64K).step_by(PAGE as usize) {
            let base = 0x4000_0000 + phase;
            let mut next = 0;
            for (len, align) in [
                (520, 64),
                (2048, 64),
                (2112, 64),
                (256, 64),
                (512, 64),
                (64, 64),
                (1056, 64),
                (4096, 4096),
                (8184, 64),
            ]
            .iter()
            .cycle()
            .take(200)
            {
                let off = place(base, next, *len, *align);
                assert!(off >= next && off % align == 0);
                assert!(
                    within_boundaries(base + off, *len),
                    "{phase:#x} {off:#x} {len}"
                );
                next = off + len;
            }
        }
    }

    /// The driver's heaviest use, placed off the boundaries at every phase
    /// of the arena against 64 KiB, fits `Params::dma_arena_bytes`: 255
    /// slots in the DCBAA, 64-byte contexts, ten slots' memory (Device
    /// Context, EP0 ring, Input Context) with four HID endpoints each
    /// (ring and four 64-byte reports) and two storage devices' memory.
    #[test]
    fn the_heaviest_use_fits_the_arena() {
        let params = crate::Params::decode(255, 0, 1 << 2);
        let ctx = params.context_bytes() as u64;
        let mut seq = std::vec::Vec::new();
        seq.extend([(256 * 8, 64), (256, 64), (512, 64), (16, 64), (512, 64)]);
        for _ in 0..10 {
            seq.extend([(32 * ctx, 64), (256, 64), (33 * ctx, 64)]);
            for _ in 0..4 {
                seq.extend([(256, 64), (64, 64), (64, 64), (64, 64), (64, 64)]);
            }
        }
        for _ in 0..2 {
            seq.extend([(256, 64), (256, 64), (64, 64), (64, 64), (64, 64)]);
        }
        for phase in (0..BOUNDARY_64K).step_by(PAGE as usize) {
            let base = 0x4000_0000 + phase;
            let end = seq
                .iter()
                .fold(0, |next, &(len, align)| place(base, next, len, align) + len);
            assert!(end <= params.dma_arena_bytes(), "phase {phase:#x}: {end}");
        }
    }

    #[test]
    fn boundary_check() {
        assert!(within_boundaries(0x1000, 4096));
        assert!(!within_boundaries(0x1800, 4096));
        assert!(!within_boundaries(0xFFF0, 32));
        assert!(within_boundaries(0x1FF0, 16));
        assert!(!within_boundaries(0x1FF0, 32));
        assert!(within_boundaries(0x1FF0, 0));
        assert!(page_size_supported(1));
        assert!(!page_size_supported(2));
    }

    #[test]
    fn the_log_merges_runs_and_names_pages() {
        let mut log: SpanLog<4> = SpanLog::new();
        log.record(0, 520, "dcbaa");
        log.record(4096, 4096, "scratchpad");
        log.record(8192, 4096, "scratchpad");
        log.record(12288, 256, "cmd ring");
        log.record(12544, 512, "event ring");
        log.record(13056, 16, "erst");
        assert_eq!(log.dropped, 1, "a fifth span does not fit");
        assert_eq!(log.iter().count(), 4);
        assert_eq!(log.iter().nth(1).map(|s| s.len), Some(8192));

        let arena = 0x12000;
        assert_eq!(
            log.locate(12288, arena, 13056),
            Located::Inside {
                offset: 12288,
                first: log.iter().nth(2).copied(),
                count: 2,
                used: true,
            }
        );
        assert_eq!(
            log.locate(0x11000, arena, 13056),
            Located::Inside {
                offset: 0x11000,
                first: None,
                count: 0,
                used: false,
            }
        );
        assert_eq!(
            log.locate(0x13000, arena, 13056),
            Located::After {
                distance: 0x1000,
                last: log.iter().nth(3).copied(),
            }
        );
        assert_eq!(
            log.locate(-0x1000, arena, 13056),
            Located::Before {
                distance: 0,
                first: log.iter().next().copied(),
            }
        );
        assert_eq!(
            log.locate(-0x3000, arena, 13056),
            Located::Before {
                distance: 0x2000,
                first: log.iter().next().copied(),
            }
        );
    }

    #[test]
    fn event_names_fit() {
        let mut out = [0u8; 24];
        let span = Span {
            off: 0,
            len: 1,
            label: "a very long label indeed",
        };
        let inside = Located::Inside {
            offset: 0,
            first: Some(span),
            count: 1,
            used: true,
        };
        assert_eq!(event_name(&inside, &mut out), "usb dma in a very long l");
        let after = Located::After {
            distance: 0,
            last: None,
        };
        assert_eq!(event_name(&after, &mut out), "usb dma past arena");
        let unused = Located::Inside {
            offset: 0,
            first: None,
            count: 0,
            used: false,
        };
        assert_eq!(event_name(&unused, &mut out), "usb dma arena unused");
    }
}
