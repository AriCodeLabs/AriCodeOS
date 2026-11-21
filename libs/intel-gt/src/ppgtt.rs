// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The per-process GTT of the one client (ADR 0083; ADR 0080 decision 5):
//! two levels, 512 directory entries kept in the global GTT's own slots, each
//! naming a table of 1,024 entries in an ordinary page, 2 GiB in all (PRM
//! Vol 1 Part 2, 2.2; Linux `gen6_ppgtt.c`). Only the kernel writes either.
//! Tables are not snooped: the kernel flushes each one from its caches after
//! writing it (PRM Vol 1 Part 2, 2.2.2).

use crate::gtt::{Layout, PAGE};
use crate::regs;
use crate::Regs;

/// Entries in one table.
pub const TABLE_ENTRIES: u32 = 1024;
/// Bytes one directory entry covers.
pub const SPAN: u64 = TABLE_ENTRIES as u64 * PAGE;
/// The PPGTT's size.
pub const SIZE: u64 = SPAN * crate::gtt::DIRECTORY_ENTRIES as u64;

/// The directory entry naming the table at `table`: the address as in a GTT
/// entry and the valid bit, no cache bits (Linux `gen6_write_pde`).
pub fn directory_entry(table: u64) -> Option<u32> {
    (table.is_multiple_of(PAGE) && table < 1 << 40)
        .then_some((table as u32 & 0xFFFF_F000) | ((table >> 28) as u32 & 0xFF0) | 1)
}

/// The directory entry and the table entry an address falls in.
pub fn split(address: u32) -> (u32, u32) {
    (address >> 22, (address >> 12) & (TABLE_ENTRIES - 1))
}

/// Writes directory entry `index`; `None` past the directory.
pub fn set_directory<R: Regs + ?Sized>(
    regs: &mut R,
    layout: &Layout,
    index: u32,
    entry: u32,
) -> Option<()> {
    (index < layout.directory_entries).then(|| {
        regs.write(regs::GTT + (layout.directory + index) * 4, entry);
    })
}

/// After directory entries change: the read orders them, then the GPU drops
/// what it cached (Linux `gen6_flush_pd`).
pub fn flush_directory<R: Regs + ?Sized>(regs: &mut R) {
    crate::gtt::flush(regs);
}
