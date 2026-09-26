//! Construcción de tablas de páginas x86-64 de 4 niveles sobre un pool de
//! páginas físicas identidad-mapeadas. Lo usa AriBoot para el mapa inicial.
//!
//! `no_std`. Se prueba en host con `cargo test -p aricode-x86-paging`: allí
//! las "direcciones físicas" son punteros del heap del proceso.
//!
//! Seguridad de memoria: la única operación `unsafe` para el llamador es
//! `PagePool::new`, que toma posesión de una región. A partir de ahí toda
//! tabla que se desreferencia proviene de ese pool (`PagePool::owns`), y la
//! jerarquía no expone sus tablas, así que la API segura no puede provocar
//! accesos fuera de la región.
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
pub const USER: u64 = 1 << 2;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Flags que un llamador puede pedir para una página hoja de 4 KiB. Cualquier
/// otro bit (dirección, HUGE, bits reservados) se descarta.
const LEAF_FLAG_MASK: u64 = WRITABLE | USER | GLOBAL | NO_EXECUTE;

/// Pool de páginas físicas cero-inicializadas, identidad-mapeadas.
///
/// Es dueño exclusivo de la región mientras vive; las tablas que reparte se
/// referencian por dirección física dentro de esa región.
pub struct PagePool {
    base: u64,
    next: u64,
    end: u64,
}

impl PagePool {
    /// Toma posesión de `pages` páginas contiguas a partir de `base`.
    ///
    /// # Safety
    ///
    /// - `base` está alineado a 4 KiB y apunta a `pages * 4096` bytes válidos,
    ///   escribibles y accesibles por identidad (dirección virtual == física).
    /// - Nadie más lee ni escribe esa región mientras el pool o las tablas
    ///   construidas con él estén en uso.
    /// - La región permanece válida mientras las tablas se usen (en la
    ///   práctica siempre: el kernel las hereda).
    pub unsafe fn new(base: NonNull<u8>, pages: usize) -> Self {
        let base = base.as_ptr() as u64;
        assert_eq!(base % PAGE_SIZE, 0, "pool no alineado a pagina");
        let end = base
            .checked_add(pages as u64 * PAGE_SIZE)
            .expect("pool overflow");
        // SAFETY: contrato de la función.
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, pages * PAGE_SIZE as usize) };
        Self {
            base,
            next: base,
            end,
        }
    }

    /// Reparte una tabla nueva (ya en cero) y devuelve su dirección física.
    fn alloc_table(&mut self) -> u64 {
        assert!(self.next < self.end, "page table pool agotado");
        let addr = self.next;
        self.next += PAGE_SIZE;
        addr
    }

    /// `true` si `phys` es una tabla que este pool ha repartido.
    fn owns(&self, phys: u64) -> bool {
        phys >= self.base && phys < self.next && phys.is_multiple_of(PAGE_SIZE)
    }

    /// Acceso a una tabla del pool. Panic si la dirección no es suya: es la
    /// barrera que impide desreferenciar direcciones arbitrarias.
    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        assert!(self.owns(phys), "tabla {phys:#x} ajena al pool");
        // SAFETY: `phys` es una página del pool (identidad-mapeada, alineada,
        // exclusiva) según `owns`; la vida queda ligada al préstamo `&mut self`.
        unsafe { &mut *(phys as *mut [u64; 512]) }
    }

    pub fn used_pages(&self) -> u64 {
        (self.next - self.base) / PAGE_SIZE
    }
}

/// Jerarquía de tablas. Todas sus tablas provienen del pool con el que se
/// creó; solo se expone la dirección física de la raíz.
pub struct PageTables {
    pml4: u64,
}

impl PageTables {
    pub fn new(pool: &mut PagePool) -> Self {
        Self {
            pml4: pool.alloc_table(),
        }
    }

    /// Dirección física del PML4, lista para `cr3`.
    pub fn root_phys(&self) -> u64 {
        self.pml4
    }

    /// Dirección de la tabla inferior en `table[idx]`, creándola si no existe.
    /// Nunca desciende por una entrada `HUGE`.
    fn next_level(pool: &mut PagePool, table_phys: u64, idx: usize) -> u64 {
        let entry = pool.table(table_phys)[idx];
        if entry & PRESENT == 0 {
            let t = pool.alloc_table();
            pool.table(table_phys)[idx] = t | PRESENT | WRITABLE;
            return t;
        }
        assert!(entry & HUGE == 0, "entrada {idx} es una pagina grande");
        entry & ADDR_MASK
    }

    /// Mapea una página de 4 KiB. `virt` y `phys` alineados; `flags` limitado
    /// a los bits de permiso de una entrada hoja (`PRESENT` se añade).
    pub fn map_4k(&mut self, pool: &mut PagePool, virt: u64, phys: u64, flags: u64) {
        assert_eq!(virt % PAGE_SIZE, 0, "virt no alineado");
        assert_eq!(phys % PAGE_SIZE, 0, "phys no alineado");
        let pdpt = Self::next_level(pool, self.pml4, ((virt >> 39) & 0x1FF) as usize);
        let pd = Self::next_level(pool, pdpt, ((virt >> 30) & 0x1FF) as usize);
        let pt = Self::next_level(pool, pd, ((virt >> 21) & 0x1FF) as usize);
        let pt_i = ((virt >> 12) & 0x1FF) as usize;
        pool.table(pt)[pt_i] = (phys & ADDR_MASK) | (flags & LEAF_FLAG_MASK) | PRESENT;
    }

    /// Mapea identidad [0, limit) con páginas de 2 MiB, RWX.
    pub fn identity_map_2m(&mut self, pool: &mut PagePool, limit: u64) {
        let limit = limit.checked_add(HUGE_2M - 1).expect("limite overflow") & !(HUGE_2M - 1);
        let mut addr = 0u64;
        while addr < limit {
            let pdpt = Self::next_level(pool, self.pml4, ((addr >> 39) & 0x1FF) as usize);
            let pd = Self::next_level(pool, pdpt, ((addr >> 30) & 0x1FF) as usize);
            pool.table(pd)[((addr >> 21) & 0x1FF) as usize] = addr | PRESENT | WRITABLE | HUGE;
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
            // SAFETY: memoria del heap alineada a 4 KiB, exclusiva del test.
            unsafe { PagePool::new(NonNull::new(self.ptr).unwrap(), pages) }
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
        assert_eq!(pool.used_pages(), 6);
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
    fn leaf_flags_cannot_smuggle_address_bits() {
        let arena = Arena::new(16);
        let mut pool = arena.pool(16);
        let mut t = PageTables::new(&mut pool);
        // Bits de direccion y HUGE en flags se descartan (auditoria, hallazgo 2)
        t.map_4k(&mut pool, 0x2000, 0x3000, 0xDEAD_B000 | HUGE | WRITABLE);
        let (phys, flags) = walk(t.root_phys(), 0x2000).unwrap();
        assert_eq!(phys, 0x3000);
        assert_eq!(flags, PRESENT | WRITABLE);
    }

    #[test]
    #[should_panic(expected = "pagina grande")]
    fn map_4k_over_2m_page_panics() {
        let arena = Arena::new(16);
        let mut pool = arena.pool(16);
        let mut t = PageTables::new(&mut pool);
        t.identity_map_2m(&mut pool, HUGE_2M);
        t.map_4k(&mut pool, 0x1000, 0x1000, 0);
    }

    #[test]
    #[should_panic(expected = "no alineado")]
    fn map_4k_rejects_unaligned() {
        let arena = Arena::new(16);
        let mut pool = arena.pool(16);
        let mut t = PageTables::new(&mut pool);
        t.map_4k(&mut pool, 0x1234, 0x1000, 0);
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
