//! Recent kernel events (stage 5, queryable diagnostics): a bounded ring
//! with what a problem report needs to know without reading the serial
//! console: processes created and exited (with the reason if the kernel
//! killed them), IOMMU DMA faults and exhausted quotas. Published by
//! `sysinfo::EVENTS` to whoever holds `READ` on `SYSTEM`; it never carries
//! message or file contents.

use crate::sync::IrqLock;
use aricode_abi::sysinfo::{EventEntry, EventKind};

const CAPACITY: usize = 64;

struct Ring {
    entries: [EventEntry; CAPACITY],
    next: usize,
    len: usize,
}

static RING: IrqLock<Ring> = IrqLock::new(Ring {
    entries: [EventEntry::EMPTY; CAPACITY],
    next: 0,
    len: 0,
});

/// Records an event; if the ring is full the oldest one is dropped.
pub fn record(kind: EventKind, pid: u32, code: u64, name: &str) {
    let entry = EventEntry::new(crate::time::uptime_us(), kind, pid, code, name);
    let mut r = RING.lock();
    let at = r.next;
    r.entries[at] = entry;
    r.next = (at + 1) % CAPACITY;
    r.len = (r.len + 1).min(CAPACITY);
}

/// The last `max` events, oldest first.
pub fn recent(max: usize) -> alloc::vec::Vec<EventEntry> {
    let r = RING.lock();
    let n = r.len.min(max);
    let first = (r.next + CAPACITY - n) % CAPACITY;
    (0..n).map(|i| r.entries[(first + i) % CAPACITY]).collect()
}
