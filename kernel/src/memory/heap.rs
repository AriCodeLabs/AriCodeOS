//! Heap del kernel (`#[global_allocator]`) sobre `aricode-heap`, con
//! crecimiento bajo demanda: cada vez que una asignación no cabe se mapea un
//! trozo más de la región virtual reservada con frames del PMM.

use super::{pmm, vmm};
use crate::sync::SingleCpu;
use aricode_heap::{Heap, HeapStats};
use aricode_x86_paging::{GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE};
use core::alloc::{GlobalAlloc, Layout};
use log::{debug, info};

/// Región virtual reservada para el heap (entrada 384 del PML4).
pub const HEAP_BASE: u64 = 0xFFFF_C000_0000_0000;
/// Tamaño máximo que puede alcanzar el heap.
pub const HEAP_MAX: u64 = 64 * 1024 * 1024;
/// Primer trozo mapeado en `init` y granularidad de crecimiento.
pub const HEAP_CHUNK: u64 = 1024 * 1024;

struct KernelHeap {
    heap: SingleCpu<Heap>,
    /// Bytes de `HEAP_BASE..` ya mapeados.
    mapped: SingleCpu<u64>,
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap {
    heap: SingleCpu::new(Heap::empty()),
    mapped: SingleCpu::new(0),
};

pub fn init() {
    grow(HEAP_CHUNK).expect("heap: sin memoria para el primer trozo");
    info!(
        "heap: {} KiB en {:#x}, crece por trozos de {} KiB hasta {} MiB",
        HEAP_CHUNK / 1024,
        HEAP_BASE,
        HEAP_CHUNK / 1024,
        HEAP_MAX >> 20
    );
}

pub fn stats() -> HeapStats {
    // SAFETY: una CPU sin interrupciones (ver `SingleCpu`).
    unsafe { (*HEAP.heap.get()).stats() }
}

pub fn mapped_bytes() -> u64 {
    // SAFETY: ídem.
    unsafe { *HEAP.mapped.get() }
}

/// Mapea `bytes` más (redondeado a página) y los añade al heap.
fn grow(bytes: u64) -> Result<(), &'static str> {
    // SAFETY: una CPU sin interrupciones; no se reentra desde el VMM ni el PMM.
    let mapped = unsafe { &mut *HEAP.mapped.get() };
    let bytes = bytes.next_multiple_of(PAGE_SIZE);
    if *mapped + bytes > HEAP_MAX {
        return Err("heap: limite alcanzado");
    }
    let start = HEAP_BASE + *mapped;
    for virt in (start..start + bytes).step_by(PAGE_SIZE as usize) {
        let frame = pmm::with(|p| p.allocate()).ok_or("heap: sin frames")?;
        vmm::with(|m| m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL))
            .map_err(|_| "heap: map")?;
    }
    *mapped += bytes;
    // SAFETY: región recién mapeada RW, exclusiva del heap, alineada a página.
    unsafe { (*HEAP.heap.get()).add_region(start as usize, bytes as usize) };
    debug!("heap: crecido a {} KiB", *mapped / 1024);
    Ok(())
}

// SAFETY: el acceso es secuencial (una CPU, sin interrupciones); `dealloc`
// solo recibe punteros de `alloc` por contrato de `GlobalAlloc`.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let heap = &mut *self.heap.get();
        if let Some(p) = heap.allocate(layout) {
            return p.as_ptr();
        }
        // No cabe: crecer lo suficiente para esta petición (con margen) y reintentar.
        let need = (layout.size() as u64 + layout.align() as u64 + 64).max(HEAP_CHUNK);
        if grow(need).is_err() {
            return core::ptr::null_mut();
        }
        heap.allocate(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = core::ptr::NonNull::new(ptr) {
            (*self.heap.get()).deallocate(p, layout);
        }
    }
}

/// Autoprueba (`test=heap`): colecciones de `alloc`, crecimiento del heap,
/// fallo controlado por agotamiento y ausencia de fugas.
pub fn selftest() {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    let before = stats();
    {
        let mut v: Vec<u64> = Vec::new();
        for i in 0..100_000u64 {
            v.push(i * 3);
        }
        assert_eq!(v.iter().sum::<u64>(), 3 * (99_999 * 100_000 / 2));
        let b = Box::new([0xA5u8; 4096]);
        assert!(b.iter().all(|&x| x == 0xA5));
        let s: String = (0..500)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        assert_eq!(s.len(), 500);
        let msg = alloc::format!("heap {} {}", s.len(), v.len());
        assert_eq!(msg, "heap 500 100000");
        // Algo grande que obliga a crecer más allá del primer trozo.
        let big: Vec<u8> = alloc::vec![7u8; 3 * 1024 * 1024];
        assert_eq!(big[big.len() - 1], 7);
        assert!(mapped_bytes() >= 4 * 1024 * 1024, "el heap debia crecer");
        // Petición imposible: falla sin pánico.
        let mut w: Vec<u8> = Vec::new();
        assert!(w.try_reserve(1 << 40).is_err());
    }
    let after = stats();
    assert_eq!(
        after.allocations, before.allocations,
        "asignaciones vivas tras liberar todo"
    );
    assert_eq!(
        after.total - after.free,
        before.total - before.free,
        "fuga de bytes"
    );
    info!(
        "heap selftest ok (Vec/Box/String/format, crecido a {} KiB, try_reserve falla limpio)",
        mapped_bytes() / 1024
    );
}
