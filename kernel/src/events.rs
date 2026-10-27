// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Recent kernel events (stage 5, queryable diagnostics): a bounded ring
//! with what a problem report needs to know without reading the serial
//! console: processes created and exited (with the reason if the kernel
//! killed them), IOMMU DMA faults and exhausted quotas. Published by
//! `sysinfo::EVENTS` to whoever holds `READ` on `SYSTEM`; it never carries
//! message or file contents.

use crate::sync::IrqLock;
use aricode_abi::sysinfo::{EventEntry, EventKind};
use aricode_faults::SharedRing;

const CAPACITY: usize = 64;

/// Entries the events one process causes may hold at once (incident 0040):
/// an eighth of the ring, so a loop, however slow, overwrites only its own.
pub const SHARE: usize = 8;

static RING: IrqLock<SharedRing<EventEntry, CAPACITY>> = IrqLock::new(SharedRing::new());

/// Records an event of the kernel's own; if the ring is full the oldest one
/// is dropped.
pub fn record(kind: EventKind, pid: u32, code: u64, name: &str) {
    let entry = EventEntry::new(crate::time::uptime_us(), kind, pid, code, name);
    RING.lock().push(entry, None, SHARE);
}

/// Records an event process `owner` caused (`event_record`, a quota it hit,
/// a DMA fault of its device): within its share of the ring, replacing its
/// own oldest one past it.
pub fn record_caused(owner: u32, kind: EventKind, pid: u32, code: u64, name: &str) {
    let entry = EventEntry::new(crate::time::uptime_us(), kind, pid, code, name);
    RING.lock().push(entry, Some(owner), SHARE);
}

/// The last `max` events, oldest first.
pub fn recent(max: usize) -> alloc::vec::Vec<EventEntry> {
    RING.lock().recent(max).collect()
}
