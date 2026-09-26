//! Construcción de tablas de páginas x86-64 de 4 niveles sobre un pool de
//! páginas físicas identidad-mapeadas. Lo usa AriBoot para el mapa inicial.
//!
//! `no_std`. Se prueba en host con `cargo test -p aricode-x86-paging`: allí
//! las "direcciones físicas" son punteros del heap del proceso.
//!
//! Estrategia M0:
//! - identidad para toda la memoria física con páginas de 2 MiB (RWX; la
//!   restricción W^X del mapa físico llega con el VMM del kernel);
//! - kernel en `KERNEL_VIRT_BASE` con páginas de 4 KiB y permisos por
//!   segmento ELF (NX salvo .text, RO salvo .data/.bss).
//!
//! Todas las páginas de tablas provienen de un pool reservado ANTES de
//! `ExitBootServices`, porque después ya no se puede asignar memoria.

#![no_std]

use core::ptr::NonNull;

pub const PAGE_SIZE: u64 = 4096;
pub const HUGE_2M: u64 = 2 * 1024 * 1024;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Pool de páginas físicas cero-inicializadas, identidad-mapeadas.
pub struct PagePool {
    next: u64,
    end: u64,
}

impl PagePool {
    pub fn new(base: NonNull<u8>, pages: usize) -> Self {
        let base = base.as_ptr() as u64;
        // SAFETY: `base` apunta a `pages` páginas recién reservadas para nosotros.
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, pages * PAGE_SIZE as usize) };
        Self {
            next: base,
            end: base + pages as u64 * PAGE_SIZE,
        }
    }

    fn alloc_table(&mut self) -> &'static mut [u64; 512] {
        assert!(self.next < self.end, "page table pool agotado");
        let addr = self.next;
        self.next += PAGE_SIZE;
        // SAFETY: página del pool, ya en cero, alineada a 4 KiB, exclusiva.
        unsafe { &mut *(addr as *mut [u64; 512]) }
    }

    pub fn used_pages(&self, base: u64) -> u64 {
        (self.next - base) / PAGE_SIZE
    }
}

pub struct PageTables {
    pub pml4: &'static mut [u64; 512],
}

fn table_of(entry: u64) -> &'static mut [u64; 512] {
    // SAFETY: las tablas están identidad-mapeadas y las creamos nosotros.
    unsafe { &mut *((entry & ADDR_MASK) as *mut [u64; 512]) }
}

impl PageTables {
    pub fn new(pool: &mut PagePool) -> Self {
        Self {
            pml4: pool.alloc_table(),
        }
    }

    pub fn root_phys(&self) -> u64 {
        self.pml4.as_ptr() as u64
    }

    fn next_level(
        pool: &mut PagePool,
        table: &mut [u64; 512],
        idx: usize,
    ) -> &'static mut [u64; 512] {
        if table[idx] & PRESENT == 0 {
            let t = pool.alloc_table();
            table[idx] = (t.as_ptr() as u64) | PRESENT | WRITABLE;
        }
        table_of(table[idx])
    }

    /// Mapea una página de 4 KiB. `flags` sin PRESENT (se añade).
    pub fn map_4k(&mut self, pool: &mut PagePool, virt: u64, phys: u64, flags: u64) {
        debug_assert_eq!(virt % PAGE_SIZE, 0);
        debug_assert_eq!(phys % PAGE_SIZE, 0);
        let pml4_i = ((virt >> 39) & 0x1FF) as usize;
        let pdpt_i = ((virt >> 30) & 0x1FF) as usize;
        let pd_i = ((virt >> 21) & 0x1FF) as usize;
        let pt_i = ((virt >> 12) & 0x1FF) as usize;
        let pdpt = Self::next_level(pool, self.pml4, pml4_i);
        let pd = Self::next_level(pool, pdpt, pdpt_i);
        assert!(pd[pd_i] & HUGE == 0, "colision con pagina 2M en {virt:#x}");
        let pt = Self::next_level(pool, pd, pd_i);
        pt[pt_i] = (phys & ADDR_MASK) | flags | PRESENT;
    }

    /// Mapea identidad [0, limit) con páginas de 2 MiB, RWX.
    pub fn identity_map_2m(&mut self, pool: &mut PagePool, limit: u64) {
        let limit = (limit + HUGE_2M - 1) & !(HUGE_2M - 1);
        let mut addr = 0u64;
        while addr < limit {
            let pml4_i = ((addr >> 39) & 0x1FF) as usize;
            let pdpt_i = ((addr >> 30) & 0x1FF) as usize;
            let pd_i = ((addr >> 21) & 0x1FF) as usize;
            let pdpt = Self::next_level(pool, self.pml4, pml4_i);
            let pd = Self::next_level(pool, pdpt, pdpt_i);
            pd[pd_i] = addr | PRESENT | WRITABLE | HUGE;
            addr += HUGE_2M;
        }
    }
}

/// Páginas de tabla necesarias, con margen, para identidad de `limit` bytes
/// más `kernel_pages` páginas de 4 KiB en la mitad alta.
pub fn pool_size_for(limit: u64, kernel_pages: u64) -> usize {
    let gib = (limit + (1 << 30) - 1) >> 30;
    // PML4 + PDPT(baja) + un PD por GiB + PDPT(alta) + PD(alta) + PTs + margen
    (1 + 1 + gib + 1 + 1 + kernel_pages.div_ceil(512) + 8) as usize
}

/// Activa EFER.NXE para que el bit NX sea válido (OVMF suele hacerlo, no confiar).
///
/// # Safety
///
/// Debe ejecutarse en ring 0 sobre x86-64; modifica un MSR de la CPU actual.
#[cfg(target_arch = "x86_64")]
pub unsafe fn enable_nxe() {
    const IA32_EFER: u32 = 0xC000_0080;
    let (lo, hi): (u32, u32);
    core::arch::asm!("rdmsr", in("ecx") IA32_EFER, out("eax") lo, out("edx") hi, options(nomem, nostack));
    let lo = lo | (1 << 11);
    core::arch::asm!("wrmsr", in("ecx") IA32_EFER, in("eax") lo, in("edx") hi, options(nomem, nostack));
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    struct Arena {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Arena {
        fn new(pages: usize) -> Self {
            let layout =
                Layout::from_size_align(pages * PAGE_SIZE as usize, PAGE_SIZE as usize).unwrap();
            let ptr = unsafe { alloc_zeroed(layout) };
            Self { ptr, layout }
        }
        fn pool(&self, pages: usize) -> PagePool {
            PagePool::new(NonNull::new(self.ptr).unwrap(), pages)
        }
    }
    impl Drop for Arena {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    fn walk(root: u64, virt: u64) -> Option<(u64, u64)> {
        let idx = |shift: u32| ((virt >> shift) & 0x1FF) as usize;
        let mut table = root;
        for (level, shift) in [39u32, 30, 21].into_iter().enumerate() {
            let e = unsafe { (*(table as *const [u64; 512]))[idx(shift)] };
            if e & PRESENT == 0 {
                return None;
            }
            if e & HUGE != 0 {
                assert_eq!(level, 2, "solo 2M en PD");
                return Some((e & !0xFFF & !(NO_EXECUTE), e & (0xFFF | NO_EXECUTE)));
            }
            table = e & 0x000F_FFFF_FFFF_F000;
        }
        let e = unsafe { (*(table as *const [u64; 512]))[idx(12)] };
        if e & PRESENT == 0 {
            return None;
        }
        Some((e & 0x000F_FFFF_FFFF_F000, e & (0xFFF | NO_EXECUTE)))
    }

    #[test]
    fn identity_map_uses_2m_pages() {
        let arena = Arena::new(32);
        let mut pool = arena.pool(32);
        let mut t = PageTables::new(&mut pool);
        t.identity_map_2m(&mut pool, 4 << 30);
        let (phys, flags) = walk(t.root_phys(), 0x1234_5678).unwrap();
        assert_eq!(phys, 0x1234_5678 & !(HUGE_2M - 1));
        assert!(flags & HUGE != 0 && flags & WRITABLE != 0);
        assert!(
            walk(t.root_phys(), 4 << 30).is_none(),
            "fuera del limite no mapeado"
        );
        // PML4 + PDPT + 4 PD
        assert_eq!(pool.used_pages(arena.ptr as u64), 6);
    }

    #[test]
    fn map_4k_sets_flags_and_translates() {
        let arena = Arena::new(16);
        let mut pool = arena.pool(16);
        let mut t = PageTables::new(&mut pool);
        let virt = 0xFFFF_FFFF_8000_2000;
        t.map_4k(&mut pool, virt, 0x0010_0000, NO_EXECUTE | GLOBAL);
        let (phys, flags) = walk(t.root_phys(), virt).unwrap();
        assert_eq!(phys, 0x0010_0000);
        assert!(flags & NO_EXECUTE != 0 && flags & WRITABLE == 0 && flags & GLOBAL != 0);
        assert!(walk(t.root_phys(), virt + 0x1000).is_none());
    }

    #[test]
    fn pool_size_covers_identity_and_kernel() {
        // 4 GiB identidad + 16 paginas de kernel: 6 tablas bajas + 3 altas + margen
        assert!(pool_size_for(4 << 30, 16) >= 9);
        // 1 TiB identidad necesita 1024 PDs
        assert!(pool_size_for(1 << 40, 16) >= 1024 + 4);
    }

    #[test]
    #[should_panic(expected = "agotado")]
    fn pool_exhaustion_panics() {
        let arena = Arena::new(2);
        let mut pool = arena.pool(2);
        let mut t = PageTables::new(&mut pool);
        t.map_4k(&mut pool, 0x1000, 0x1000, 0); // necesita 3 tablas mas
    }
}
