//! Acceso a memoria física a través del mapa lineal (`PHYS_MAP_BASE`).
//!
//! AriBoot lo construye antes de saltar y el VMM del kernel lo reproduce en
//! sus propias tablas, así que estas traducciones son válidas siempre.

use core::sync::atomic::{AtomicU64, Ordering};

static OFFSET: AtomicU64 = AtomicU64::new(0);
static LIMIT: AtomicU64 = AtomicU64::new(0);

/// Fija el desplazamiento y el límite del mapa lineal. Una sola vez.
/// `extra_end` permite incluir regiones mapeadas fuera de la RAM (framebuffer).
pub fn init(offset: u64, limit: u64, extra_end: u64) {
    OFFSET.store(offset, Ordering::Relaxed);
    LIMIT.store(limit.max(extra_end), Ordering::Relaxed);
}

pub fn offset() -> u64 {
    OFFSET.load(Ordering::Relaxed)
}

/// Fin exclusivo de la memoria física cubierta por el mapa lineal.
pub fn limit() -> u64 {
    LIMIT.load(Ordering::Relaxed)
}

/// Dirección virtual de la física `phys`.
///
/// # Panics
/// Si `phys` queda fuera del mapa lineal.
pub fn phys_to_virt(phys: u64) -> u64 {
    assert!(
        phys < limit(),
        "fisica {phys:#x} fuera del mapa lineal ({:#x})",
        limit()
    );
    phys + offset()
}
