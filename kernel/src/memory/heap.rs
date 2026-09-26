//! Heap del kernel (`#[global_allocator]`) sobre `aricode-heap`, con
//! crecimiento bajo demanda: cada vez que una asignación no cabe se mapea un
//! trozo más de la región virtual reservada con frames del PMM.

use super::{pmm, vmm};
use crate::sync::IrqLock;
use aricode_heap::{Heap, HeapStats};
use aricode_pmm::Frame;
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
    heap: IrqLock<Heap>,
    /// Bytes de `HEAP_BASE..` ya mapeados. Su lock serializa los crecimientos.
    mapped: IrqLock<u64>,
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap {
    heap: IrqLock::new(Heap::empty()),
    mapped: IrqLock::new(0),
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
    HEAP.heap.lock().stats()
}

pub fn mapped_bytes() -> u64 {
    *HEAP.mapped.lock()
}

/// Mapea `bytes` más (redondeado a página) y los añade al heap.
///
/// Es transaccional: si faltan frames o falla un mapeo, las páginas ya
/// mapeadas en esta llamada se desmapean y sus frames vuelven al PMM, y
/// `mapped` no avanza, de modo que el siguiente intento parte del mismo
/// estado y puede tener éxito si vuelve a haber memoria. Las tablas
/// intermedias que el VMM haya creado se conservan: se reutilizarán.
fn grow(bytes: u64) -> Result<(), &'static str> {
    // El lock de `mapped` se mantiene durante todo el crecimiento: ni el PMM
    // ni el VMM llaman al heap, así que no hay reentrada.
    let mut mapped = HEAP.mapped.lock();
    let bytes = bytes.next_multiple_of(PAGE_SIZE);
    if *mapped + bytes > HEAP_MAX {
        return Err("heap: limite alcanzado");
    }
    let start = HEAP_BASE + *mapped;

    let mut done = 0u64;
    let result = map_pages(start, bytes, &mut done);
    if let Err(e) = result {
        unmap_pages(start, done);
        return Err(e);
    }

    *mapped += bytes;
    // SAFETY: región recién mapeada RW, exclusiva del heap, alineada a página.
    unsafe { HEAP.heap.lock().add_region(start as usize, bytes as usize) };
    debug!("heap: crecido a {} KiB", *mapped / 1024);
    Ok(())
}

/// Mapea página a página; `done` cuenta los bytes ya mapeados con éxito para
/// que el llamador pueda deshacerlos.
fn map_pages(start: u64, bytes: u64, done: &mut u64) -> Result<(), &'static str> {
    while *done < bytes {
        let frame = pmm::with(|p| p.allocate()).ok_or("heap: sin frames")?;
        let virt = start + *done;
        if vmm::with(|m| m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL)).is_err() {
            pmm::with(|p| p.free(frame)).expect("heap: frame recien asignado");
            return Err("heap: map");
        }
        *done += PAGE_SIZE;
    }
    Ok(())
}

/// Deshace `map_pages`: desmapea, invalida el TLB y devuelve los frames.
fn unmap_pages(start: u64, len: u64) {
    for virt in (start..start + len).step_by(PAGE_SIZE as usize) {
        let phys = vmm::with(|m| m.unmap_4k(virt)).expect("heap: pagina mapeada por map_pages");
        // SAFETY: invalidación de una entrada que acabamos de retirar.
        unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
        pmm::with(|p| p.free(Frame::from_addr(phys))).expect("heap: frame mapeado por map_pages");
    }
}

// SAFETY: cada acceso al heap toma su `IrqLock` durante una sola operación;
// `dealloc` solo recibe punteros de `alloc` por contrato de `GlobalAlloc`.
// `grow` toma el lock por su cuenta, así que no puede haber un guard vivo al
// llamarlo.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let first = self.heap.lock().allocate(layout);
        if let Some(p) = first {
            return p.as_ptr();
        }
        // No cabe: crecer lo suficiente para esta petición (con margen) y
        // reintentar con un lock nuevo.
        let need = (layout.size() as u64)
            .saturating_add(layout.align() as u64)
            .saturating_add(64)
            .max(HEAP_CHUNK);
        if grow(need).is_err() {
            return core::ptr::null_mut();
        }
        self.heap
            .lock()
            .allocate(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = core::ptr::NonNull::new(ptr) {
            self.heap.lock().deallocate(p, layout);
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
        // Peticiones imposibles: fallan sin pánico. 1 << 46 desborda la
        // aritmética de direcciones en la mitad alta (auditoría 9d8c7e5, 1).
        let mut w: Vec<u8> = Vec::new();
        assert!(w.try_reserve(1 << 40).is_err());
        assert!(w.try_reserve(1 << 46).is_err());
        let mut z: Vec<[u8; 4096]> = Vec::new();
        assert!(z.try_reserve(1 << 34).is_err());
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

/// Autoprueba de agotamiento (`test=heap_oom`, auditoría 9d8c7e5, hallazgo 2):
/// un crecimiento que falla por falta de frames no debe perder páginas ni
/// dejar el heap incapaz de crecer cuando vuelva a haber memoria.
pub fn oom_selftest() {
    use alloc::vec::Vec;
    const KEEP: usize = 8;

    // Capacidad reservada de antemano: el drenado no debe pedir heap.
    let mut held: Vec<Frame> = Vec::with_capacity(pmm::stats().free_frames + 16);
    loop {
        let f = pmm::with(|p| {
            if p.stats().free_frames > KEEP {
                p.allocate()
            } else {
                None
            }
        });
        match f {
            Some(f) => held.push(f),
            None => break,
        }
    }
    let mapped_before = mapped_bytes();
    let free_before = pmm::stats().free_frames;
    assert_eq!(free_before, KEEP);

    let mut big: Vec<u8> = Vec::new();
    assert!(
        big.try_reserve(8 << 20).is_err(),
        "con {KEEP} frames libres no puede crecer 8 MiB"
    );
    assert_eq!(
        mapped_bytes(),
        mapped_before,
        "un crecimiento fallido no debe avanzar mapped"
    );
    let free_after = pmm::stats().free_frames;
    // Solo pueden haberse consumido tablas intermedias (una por cada 2 MiB nuevos).
    let lost = free_before - free_after;
    assert!(
        lost <= 4,
        "frames perdidos en el crecimiento fallido: {lost}"
    );

    for f in held.drain(..) {
        pmm::with(|p| p.free(f)).expect("devolver frame retenido");
    }
    assert!(
        big.try_reserve(8 << 20).is_ok(),
        "con memoria de nuevo el heap debe crecer"
    );
    assert!(mapped_bytes() >= mapped_before + (8 << 20));
    drop(big);
    info!(
        "heap oom selftest ok (fallo con {KEEP} frames, {lost} tablas, recuperacion a {} KiB)",
        mapped_bytes() / 1024
    );
}
