// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The global GTT (ADR 0083). The laptop's firmware maps its 64 MiB of
//! stolen memory, the screen included, and leaves the rest of the table as
//! whatever the memory held: entries marked valid that point anywhere. So
//! nothing runs on the GPU before [`clear`] has made every other entry point
//! at a page the kernel chose.

use crate::regs;
use crate::{Refusal, Regs};

/// Bytes the GPU sees through one entry.
pub const PAGE: u64 = 4096;

/// Entries in the global GTT, from the GTT size in MGGC0 bits 9:8 (PRM
/// Vol 3 Part 2, MGGC0: 1 MiB or 2 MiB of entries of 4 bytes).
pub fn entries(mggc0: u32) -> Option<u32> {
    match (mggc0 >> 8) & 3 {
        1 => Some((1 << 20) / 4),
        2 => Some((2 << 20) / 4),
        _ => None,
    }
}

/// How the GPU caches a page (Linux `ivb_pte_encode`; the PRM's own table
/// for the global GTT disagrees with its PPGTT table, Vol 1 Part 2 2.1.2 and
/// 2.2.2, and Linux's encoding is the one run on this hardware for years).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cache {
    Uncached = 1 << 1,
    Llc = 2 << 1,
    L3Llc = 3 << 1,
}

const VALID: u32 = 1;
/// Physical addresses an entry can hold: 40 bits (PRM Vol 1 Part 2, 2.1.2).
const PHYS_LIMIT: u64 = 1 << 40;

/// The entry for the page at `phys`: address bits 31:12 in place, 39:32 in
/// bits 11:4, the cache bits, valid. `None` for an address it cannot hold.
/// The same form serves PPGTT entries and page directory entries (Linux
/// `GEN6_GTT_ADDR_ENCODE`).
pub fn encode(phys: u64, cache: Cache) -> Option<u32> {
    (phys.is_multiple_of(PAGE) && phys < PHYS_LIMIT).then_some(
        (phys as u32 & 0xFFFF_F000) | ((phys >> 28) as u32 & 0xFF0) | cache as u32 | VALID,
    )
}

/// The page a valid entry points at.
pub fn decode(entry: u32) -> Option<u64> {
    (entry & VALID != 0).then_some((entry as u64 & 0xFFFF_F000) | ((entry as u64 & 0xFF0) << 28))
}

/// Counts the entries from 0 that map `stolen` page after page, as the
/// firmware leaves them for the screen, and checks they cover the screen,
/// which ends `screen_end` bytes into the aperture (its offset there plus
/// its size: it need not start at 0). Reads only.
pub fn firmware_run<R: Regs + ?Sized>(
    regs: &R,
    entries: u32,
    stolen: u64,
    screen_end: u64,
) -> Result<u32, Refusal> {
    let mut n = 0;
    while n < entries && decode(regs.read(regs::GTT + n * 4)) == Some(stolen + n as u64 * PAGE) {
        n += 1;
    }
    if (n as u64) * PAGE < screen_end {
        return Err(Refusal::GttSize);
    }
    Ok(n)
}

/// Points entries `[from, to)` at `scratch`, then has the GPU drop what it
/// cached of the table (Linux `gen6_ggtt_clear_range`,
/// `gen6_ggtt_invalidate`).
pub fn clear<R: Regs + ?Sized>(regs: &mut R, from: u32, to: u32, scratch: u32) {
    for i in from..to {
        regs.write(regs::GTT + i * 4, scratch);
    }
    flush(regs);
}

/// Writes one entry and flushes.
pub fn set<R: Regs + ?Sized>(regs: &mut R, index: u32, entry: u32) {
    regs.write(regs::GTT + index * 4, entry);
    flush(regs);
}

/// Makes entries written through BAR 0 visible to the GPU: the read orders
/// them before the flush, which the GPU then performs.
pub fn flush<R: Regs + ?Sized>(regs: &mut R) {
    regs.read(regs::GTT);
    regs.write(regs::GFX_FLSH_CNTL, 1);
    regs.read(regs::GFX_FLSH_CNTL);
}

/// Where the kernel's own pages sit in the global GTT's address space, past
/// the firmware's run and a guard page. Each is a parameter: the defaults
/// follow Linux (a 16 KiB ring, never at address 0, the page directory
/// high and aligned to 16 pages: `intel_ring_pin`, `GEN6_PD_ALIGN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The blitter's status page.
    pub status: u32,
    /// The blitter's ring's first page, and each ring's pages.
    pub ring: u32,
    pub ring_pages: u32,
    /// The render engine's status page and ring (ADR 0086).
    pub render_status: u32,
    pub render_ring: u32,
    /// The first of the entries that hold the PPGTT's page directory, and
    /// the directory's entries, each covering 4 MiB.
    pub directory: u32,
    pub directory_entries: u32,
    /// Entries `[0, kept)` stay as the firmware set them. The status page and
    /// the ring sit past both those and the CPU's aperture (a guard page
    /// before them), so no window onto the aperture ever reaches the kernel's
    /// GPU pages (audit M1); they end at `end`. Every other entry below the
    /// directory points at the scratch page.
    pub kept: u32,
    pub end: u32,
}

/// Page directory entries: 512 cover the PPGTT's 2 GiB (Linux `I915_PDES`).
pub const DIRECTORY_ENTRIES: u32 = 512;
/// The ring may have 1 to 512 pages (PRM Vol 1 Part 4, 2.1.9).
pub const RING_PAGES_MAX: u32 = 512;

impl Layout {
    /// `kept`: entries left as the firmware set them; `entries`: the table's;
    /// `aperture`: entries the CPU's aperture (BAR 2) shows.
    pub fn new(kept: u32, entries: u32, ring_pages: u32, aperture: u32) -> Option<Self> {
        if ring_pages == 0 || ring_pages > RING_PAGES_MAX {
            return None;
        }
        let status = kept.max(aperture).checked_add(1)?;
        let ring = status + 1;
        let render_status = ring.checked_add(ring_pages)?;
        let render_ring = render_status + 1;
        let end = render_ring.checked_add(ring_pages)?;
        let directory = entries.checked_sub(DIRECTORY_ENTRIES)? & !15;
        // A ring's start must sit below 512 MiB (PRM Vol 1 Part 4, 2.1.9).
        if end > directory || end as u64 * PAGE > 512 << 20 {
            return None;
        }
        Some(Layout {
            status,
            ring,
            ring_pages,
            render_status,
            render_ring,
            directory,
            directory_entries: DIRECTORY_ENTRIES,
            kept,
            end,
        })
    }

    /// `engine`'s status page and ring, as entries of the table.
    pub fn placement(&self, engine: &crate::regs::Engine) -> (u32, u32) {
        if *engine == crate::regs::RENDER {
            (self.render_status, self.render_ring)
        } else {
            (self.status, self.ring)
        }
    }

    /// The value of `PP_DIR_BASE`: the directory's byte offset in the table,
    /// shifted as Linux does (`pd_vma_bind`).
    pub fn pp_dir_base(&self) -> u32 {
        (self.directory * 4) << 10
    }
}
