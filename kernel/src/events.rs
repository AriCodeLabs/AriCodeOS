//! Sucesos recientes del kernel (etapa 5, diagnóstico consultable): un
//! anillo acotado con lo que un informe de problema necesita saber sin
//! leer la consola serie: procesos creados y terminados (con el motivo si
//! los mató el kernel), fallos de DMA del IOMMU y cuotas agotadas. Lo
//! publica `sysinfo::EVENTS` a quien tenga `READ` sobre `SYSTEM`; nunca
//! lleva contenido de mensajes ni de ficheros.

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

/// Anota un suceso; si el anillo está lleno se pierde el más antiguo.
pub fn record(kind: EventKind, pid: u32, code: u64, name: &str) {
    let entry = EventEntry::new(crate::time::uptime_us(), kind, pid, code, name);
    let mut r = RING.lock();
    let at = r.next;
    r.entries[at] = entry;
    r.next = (at + 1) % CAPACITY;
    r.len = (r.len + 1).min(CAPACITY);
}

/// Los últimos `max` sucesos, del más antiguo al más reciente.
pub fn recent(max: usize) -> alloc::vec::Vec<EventEntry> {
    let r = RING.lock();
    let n = r.len.min(max);
    let first = (r.next + CAPACITY - n) % CAPACITY;
    (0..n).map(|i| r.entries[(first + i) % CAPACITY]).collect()
}
