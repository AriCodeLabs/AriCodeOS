//! Physical memory access through the linear map (`PHYS_MAP_BASE`).
//!
//! AriBoot builds it before jumping and the kernel VMM reproduces it in its
//! own tables, so these translations are always valid.

use core::sync::atomic::{AtomicU64, Ordering};

static OFFSET: AtomicU64 = AtomicU64::new(0);
static LIMIT: AtomicU64 = AtomicU64::new(0);

/// Sets the offset and limit of the linear map. Once only.
/// `extra_end` allows including regions mapped outside RAM (framebuffer).
pub fn init(offset: u64, limit: u64, extra_end: u64) {
    OFFSET.store(offset, Ordering::Relaxed);
    LIMIT.store(limit.max(extra_end), Ordering::Relaxed);
}

pub fn offset() -> u64 {
    OFFSET.load(Ordering::Relaxed)
}

/// Exclusive end of the physical memory covered by the linear map.
pub fn limit() -> u64 {
    LIMIT.load(Ordering::Relaxed)
}

/// Virtual address of physical address `phys`.
///
/// # Panics
/// If `phys` lies outside the linear map.
pub fn phys_to_virt(phys: u64) -> u64 {
    assert!(
        phys < limit(),
        "physical {phys:#x} outside the linear map ({:#x})",
        limit()
    );
    phys + offset()
}
