//! Heap del kernel (Fase 7): lista enlazada de bloques libres, first-fit,
//! con fusión de vecinos al liberar.
//!
//! Cada asignación lleva una cabecera de 16 bytes justo antes del puntero
//! devuelto con el bloque real que ocupa, de modo que `deallocate` no depende
//! del `Layout` que le pasen y las alineaciones grandes no pierden memoria.
//!
//! `no_std`, sin `alloc`. Se prueba en host sobre un buffer del heap del
//! proceso. Es el paso "linked-list allocator" de la sección 16 del documento
//! maestro; el slab/size-class llegará cuando haya métricas que lo pidan.

#![no_std]

use core::alloc::Layout;
use core::ptr::NonNull;

/// Granularidad de tamaños y alineación mínima del heap.
pub const GRANULE: usize = 16;
/// Bloque libre mínimo: debe albergar un `FreeBlock`.
pub const MIN_BLOCK: usize = 32;
const HEADER: usize = 16;

/// Nodo de la lista de bloques libres, situado al principio del bloque.
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

/// Cabecera de una asignación viva, justo antes del puntero devuelto.
#[repr(C)]
#[derive(Clone, Copy)]
struct AllocHeader {
    block_start: usize,
    block_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    /// Bytes administrados (suma de regiones añadidas).
    pub total: usize,
    /// Bytes en bloques libres.
    pub free: usize,
    /// Asignaciones vivas.
    pub allocations: usize,
}

pub struct Heap {
    head: Option<NonNull<FreeBlock>>,
    stats: HeapStats,
}

// SAFETY: el heap no comparte nada por sí mismo; el llamador serializa el
// acceso (en el kernel, `SingleCpu`/spinlock).
unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Heap {
            head: None,
            stats: HeapStats {
                total: 0,
                free: 0,
                allocations: 0,
            },
        }
    }

    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Añade `[start, start+size)` al heap como bloque libre.
    ///
    /// # Safety
    ///
    /// La región debe ser memoria válida, escribible, alineada a `GRANULE`,
    /// de tamaño múltiplo de `GRANULE` y >= `MIN_BLOCK`, de uso exclusivo del
    /// heap mientras viva, y no solapar regiones ya añadidas.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        assert!(start.is_multiple_of(GRANULE) && size.is_multiple_of(GRANULE) && size >= MIN_BLOCK);
        self.stats.total += size;
        self.insert_free(start, size);
    }

    /// Inserta un bloque libre manteniendo la lista ordenada por dirección y
    /// fusionándolo con sus vecinos.
    unsafe fn insert_free(&mut self, start: usize, size: usize) {
        self.stats.free += size;
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut cur = self.head;
        while let Some(c) = cur {
            if c.as_ptr() as usize > start {
                break;
            }
            prev = Some(c);
            cur = c.as_ref().next;
        }
        // Fusionar con el siguiente.
        let (mut size, next) = match cur {
            Some(n) if start + size == n.as_ptr() as usize => {
                (size + n.as_ref().size, n.as_ref().next)
            }
            _ => (size, cur),
        };
        // Fusionar con el anterior.
        match prev {
            Some(mut p) if p.as_ptr() as usize + p.as_ref().size == start => {
                size += p.as_ref().size;
                p.as_mut().size = size;
                p.as_mut().next = next;
            }
            _ => {
                let node = start as *mut FreeBlock;
                node.write(FreeBlock { size, next });
                let node = NonNull::new_unchecked(node);
                match prev {
                    Some(mut p) => p.as_mut().next = Some(node),
                    None => self.head = Some(node),
                }
            }
        }
    }

    fn round(layout: Layout) -> (usize, usize) {
        let size = layout.size().max(1).next_multiple_of(GRANULE);
        let align = layout.align().max(GRANULE);
        (size, align)
    }

    pub fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let (size, align) = Self::round(layout);
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut cur = self.head;
        while let Some(c) = cur {
            // SAFETY: `c` es un nodo válido de nuestra lista.
            let (block_start, block_size, next) =
                unsafe { (c.as_ptr() as usize, c.as_ref().size, c.as_ref().next) };
            let block_end = block_start + block_size;
            let user = (block_start + HEADER).next_multiple_of(align);
            let used_end = user + size;
            if used_end <= block_end {
                let tail = block_end - used_end;
                let used_end = if tail >= MIN_BLOCK {
                    used_end
                } else {
                    block_end
                };
                // Sacar el bloque de la lista.
                // SAFETY: `prev`, si existe, es un nodo válido.
                unsafe {
                    match prev {
                        Some(mut p) => p.as_mut().next = next,
                        None => self.head = next,
                    }
                }
                self.stats.free -= block_size;
                if used_end < block_end {
                    // SAFETY: resto del bloque, >= MIN_BLOCK y alineado.
                    unsafe { self.insert_free(used_end, block_end - used_end) };
                }
                let header = AllocHeader {
                    block_start,
                    block_size: used_end - block_start,
                };
                // SAFETY: `user - HEADER >= block_start`, dentro del bloque.
                unsafe { ((user - HEADER) as *mut AllocHeader).write(header) };
                self.stats.allocations += 1;
                return NonNull::new(user as *mut u8);
            }
            prev = cur;
            cur = next;
        }
        None
    }

    /// # Safety
    ///
    /// `ptr` debe provenir de `allocate` de este heap y no haberse liberado.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, _layout: Layout) {
        let header = ((ptr.as_ptr() as usize - HEADER) as *const AllocHeader).read();
        self.stats.allocations -= 1;
        self.insert_free(header.block_start, header.block_size);
    }

    /// Número de bloques libres (para tests y diagnóstico).
    pub fn free_blocks(&self) -> usize {
        let mut n = 0;
        let mut cur = self.head;
        while let Some(c) = cur {
            n += 1;
            // SAFETY: nodo válido de la lista.
            cur = unsafe { c.as_ref().next };
        }
        n
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc, dealloc};
    use std::vec::Vec;

    struct Region {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Region {
        fn new(size: usize) -> Self {
            let layout = Layout::from_size_align(size, 4096).unwrap();
            Self {
                ptr: unsafe { alloc(layout) },
                layout,
            }
        }
        fn heap(&self) -> Heap {
            let mut h = Heap::empty();
            unsafe { h.add_region(self.ptr as usize, self.layout.size()) };
            h
        }
    }
    impl Drop for Region {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    fn l(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn alloc_free_reuse_and_coalesce() {
        let r = Region::new(4096);
        let mut h = r.heap();
        assert_eq!(h.stats().free, 4096);
        let a = h.allocate(l(100, 8)).unwrap();
        let b = h.allocate(l(200, 8)).unwrap();
        assert_eq!(h.stats().allocations, 2);
        assert!(a.as_ptr() < b.as_ptr());
        unsafe { h.deallocate(a, l(100, 8)) };
        unsafe { h.deallocate(b, l(200, 8)) };
        assert_eq!(h.stats().allocations, 0);
        assert_eq!(h.stats().free, 4096, "todo vuelve al heap");
        assert_eq!(h.free_blocks(), 1, "los bloques se fusionan");
        // Se reutiliza la misma direccion.
        assert_eq!(h.allocate(l(100, 8)).unwrap(), a);
    }

    #[test]
    fn respects_large_alignment() {
        let r = Region::new(16384);
        let mut h = r.heap();
        let _pad = h.allocate(l(24, 8)).unwrap();
        let p = h.allocate(l(64, 1024)).unwrap();
        assert_eq!(p.as_ptr() as usize % 1024, 0);
        let q = h.allocate(l(8, 4096)).unwrap();
        assert_eq!(q.as_ptr() as usize % 4096, 0);
        unsafe {
            h.deallocate(p, l(64, 1024));
            h.deallocate(q, l(8, 4096));
            h.deallocate(_pad, l(24, 8));
        }
        assert_eq!(h.stats().free, 16384, "sin fugas con alineaciones grandes");
        assert_eq!(h.free_blocks(), 1);
    }

    #[test]
    fn out_of_memory_returns_none_and_keeps_state() {
        let r = Region::new(1024);
        let mut h = r.heap();
        assert!(h.allocate(l(2000, 8)).is_none());
        let a = h.allocate(l(900, 8)).unwrap();
        assert!(h.allocate(l(200, 8)).is_none());
        unsafe { h.deallocate(a, l(900, 8)) };
        assert_eq!(h.stats().free, 1024);
    }

    #[test]
    fn many_small_allocations_in_random_order() {
        let r = Region::new(64 * 1024);
        let mut h = r.heap();
        let mut live: Vec<(NonNull<u8>, Layout)> = Vec::new();
        for i in 0..500usize {
            let size = 1 + (i * 37) % 200;
            let p = h.allocate(l(size, 8)).unwrap();
            unsafe { core::ptr::write_bytes(p.as_ptr(), i as u8, size) };
            live.push((p, l(size, 8)));
        }
        // Liberar en orden pseudoaleatorio y comprobar que no hay solapes.
        for (i, (p, lay)) in live.iter().enumerate() {
            let expect = i as u8;
            let bytes = unsafe { core::slice::from_raw_parts(p.as_ptr(), lay.size()) };
            assert!(bytes.iter().all(|&b| b == expect), "asignacion {i} pisada");
        }
        let mut order: Vec<usize> = (0..live.len()).collect();
        order.sort_by_key(|&i| (i * 7919) % 500);
        for i in order {
            let (p, lay) = live[i];
            unsafe { h.deallocate(p, lay) };
        }
        assert_eq!(h.stats().free, 64 * 1024);
        assert_eq!(h.free_blocks(), 1);
    }

    #[test]
    fn multiple_regions_and_small_tail_absorbed() {
        let r1 = Region::new(4096);
        let r2 = Region::new(4096);
        let mut h = r1.heap();
        unsafe { h.add_region(r2.ptr as usize, 4096) };
        assert_eq!(h.stats().total, 8192);
        // Asignacion que deja una cola menor que MIN_BLOCK: se absorbe.
        let a = h.allocate(l(4096 - HEADER - 8, 8)).unwrap();
        assert_eq!(h.free_blocks(), 1, "la cola pequena no crea un bloque");
        unsafe { h.deallocate(a, l(1, 1)) }; // el layout no importa: la cabecera manda
        assert_eq!(h.stats().free, 8192);
    }
}
