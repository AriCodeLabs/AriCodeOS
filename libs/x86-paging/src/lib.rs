//! Tablas de páginas x86-64 de 4 niveles, genéricas sobre cómo se obtienen
//! frames y cómo se accede a ellos:
//!
//! - [`FrameAlloc`]: entrega frames físicos de 4 KiB (el cargador usa un
//!   bump sobre páginas UEFI; el kernel, el PMM).
//! - [`PhysMap`]: traduce una dirección física a un puntero utilizable (el
//!   cargador usa identidad; el kernel, el mapa lineal en `PHYS_MAP_BASE`).
//!
//! Seguridad de memoria: `Mapper` solo desreferencia tablas que él mismo ha
//! creado con su `FrameAlloc`, a través de su `PhysMap`; no expone las tablas
//! ni acepta punteros externos. La única operación `unsafe` para el llamador
//! es construir un `FrameAlloc`/`PhysMap` que cumpla su contrato.
//!
//! `no_std`; se prueba en host con frames del heap y traducción identidad.

#![no_std]

pub const PAGE_SIZE: u64 = 4096;
pub const HUGE_2M: u64 = 2 * 1024 * 1024;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Flags que un llamador puede pedir para una hoja. Lo demás se descarta.
const LEAF_FLAG_MASK: u64 = WRITABLE | USER | WRITE_THROUGH | CACHE_DISABLE | GLOBAL | NO_EXECUTE;

/// Fuente de frames físicos de 4 KiB. El contenido puede ser basura: el
/// `Mapper` los pone a cero antes de usarlos como tabla.
pub trait FrameAlloc {
    fn alloc_frame(&mut self) -> Option<u64>;
}

/// Traducción de dirección física a puntero accesible.
///
/// # Safety
///
/// `phys_to_virt(p)` debe devolver un puntero válido, alineado como `p`, a
/// memoria que refleje exactamente la física `p` (identidad o mapa lineal
/// activo) para todo frame que el `FrameAlloc` asociado pueda entregar.
pub unsafe trait PhysMap {
    fn phys_to_virt(&self, phys: u64) -> *mut u8;
}

/// Traducción identidad (virtual == física). Válida mientras las tablas
/// activas mapeen la memoria por identidad.
pub struct IdentityMap;

// SAFETY: por contrato del llamador, la memoria está identidad-mapeada.
unsafe impl PhysMap for IdentityMap {
    fn phys_to_virt(&self, phys: u64) -> *mut u8 {
        phys as *mut u8
    }
}

/// Traducción por desplazamiento constante (mapa lineal).
#[derive(Debug, Clone, Copy)]
pub struct OffsetMap(pub u64);

// SAFETY: por contrato del llamador, `[0, limite)` físico está mapeado en
// `offset + p` en las tablas activas.
unsafe impl PhysMap for OffsetMap {
    fn phys_to_virt(&self, phys: u64) -> *mut u8 {
        (phys + self.0) as *mut u8
    }
}

/// Bump allocator sobre una región contigua de páginas ya reservadas.
pub struct BumpFrames {
    base: u64,
    next: u64,
    end: u64,
}

impl BumpFrames {
    /// # Safety
    ///
    /// `base` alineado a 4 KiB, `pages * 4096` bytes válidos y de uso
    /// exclusivo de este allocator mientras viva.
    pub unsafe fn new(base: u64, pages: usize) -> Self {
        assert!(base.is_multiple_of(PAGE_SIZE), "base no alineada");
        let end = base
            .checked_add(pages as u64 * PAGE_SIZE)
            .expect("overflow");
        Self {
            base,
            next: base,
            end,
        }
    }
    pub fn used_pages(&self) -> u64 {
        (self.next - self.base) / PAGE_SIZE
    }
}

impl FrameAlloc for BumpFrames {
    fn alloc_frame(&mut self) -> Option<u64> {
        if self.next >= self.end {
            return None;
        }
        let f = self.next;
        self.next += PAGE_SIZE;
        Some(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    OutOfFrames,
    /// La dirección no está alineada al tamaño de página pedido.
    Unaligned,
    /// Ya existe una hoja en esa dirección.
    AlreadyMapped,
    /// Un nivel intermedio es una página grande y no se puede descender.
    HugePageCollision,
    /// `unmap`/`translate` sobre algo no mapeado.
    NotMapped,
}

pub struct Mapper<A: FrameAlloc, M: PhysMap> {
    root: u64,
    alloc: A,
    map: M,
}

const fn index(virt: u64, shift: u32) -> usize {
    ((virt >> shift) & 0x1FF) as usize
}

impl<A: FrameAlloc, M: PhysMap> Mapper<A, M> {
    /// Crea una jerarquía vacía (PML4 a cero).
    pub fn new(alloc: A, map: M) -> Result<Self, MapError> {
        let mut m = Self {
            root: 0,
            alloc,
            map,
        };
        m.root = m.alloc_table()?;
        Ok(m)
    }

    /// Dirección física del PML4, lista para `cr3`.
    pub fn root_phys(&self) -> u64 {
        self.root
    }

    pub fn allocator(&mut self) -> &mut A {
        &mut self.alloc
    }

    fn alloc_table(&mut self) -> Result<u64, MapError> {
        let phys = self.alloc.alloc_frame().ok_or(MapError::OutOfFrames)?;
        // SAFETY: frame recién entregado por nuestro allocator, accesible por
        // contrato de `PhysMap`; lo ponemos a cero antes de usarlo.
        unsafe { core::ptr::write_bytes(self.map.phys_to_virt(phys), 0, PAGE_SIZE as usize) };
        Ok(phys)
    }

    fn table(&self, phys: u64) -> &[u64; 512] {
        // SAFETY: solo se llama con tablas creadas por `alloc_table` o la raíz;
        // el `PhysMap` garantiza el puntero.
        unsafe { &*(self.map.phys_to_virt(phys) as *const [u64; 512]) }
    }

    fn table_mut(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: ídem `table`; `&mut self` da la exclusividad.
        unsafe { &mut *(self.map.phys_to_virt(phys) as *mut [u64; 512]) }
    }

    /// Tabla del nivel inferior en `table[idx]`. Con `create`, la crea si falta.
    fn next_level(
        &mut self,
        table_phys: u64,
        idx: usize,
        create: bool,
    ) -> Result<Option<u64>, MapError> {
        let entry = self.table(table_phys)[idx];
        if entry & PRESENT == 0 {
            if !create {
                return Ok(None);
            }
            let t = self.alloc_table()?;
            self.table_mut(table_phys)[idx] = t | PRESENT | WRITABLE;
            return Ok(Some(t));
        }
        if entry & HUGE != 0 {
            return Err(MapError::HugePageCollision);
        }
        Ok(Some(entry & ADDR_MASK))
    }

    /// Mapea una página de 4 KiB.
    pub fn map_4k(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
        if !virt.is_multiple_of(PAGE_SIZE) || !phys.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Unaligned);
        }
        let pdpt = self
            .next_level(self.root, index(virt, 39), true)?
            .expect("create");
        let pd = self
            .next_level(pdpt, index(virt, 30), true)?
            .expect("create");
        let pt = self.next_level(pd, index(virt, 21), true)?.expect("create");
        let slot = &mut self.table_mut(pt)[index(virt, 12)];
        if *slot & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *slot = (phys & ADDR_MASK) | (flags & LEAF_FLAG_MASK) | PRESENT;
        Ok(())
    }

    /// Mapea una página de 2 MiB.
    pub fn map_2m(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
        if !virt.is_multiple_of(HUGE_2M) || !phys.is_multiple_of(HUGE_2M) {
            return Err(MapError::Unaligned);
        }
        let pdpt = self
            .next_level(self.root, index(virt, 39), true)?
            .expect("create");
        let pd = self
            .next_level(pdpt, index(virt, 30), true)?
            .expect("create");
        let slot = &mut self.table_mut(pd)[index(virt, 21)];
        if *slot & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *slot = (phys & ADDR_MASK) | (flags & LEAF_FLAG_MASK) | HUGE | PRESENT;
        Ok(())
    }

    /// Mapea `[virt, virt+len)` -> `[phys, phys+len)` con páginas de 2 MiB.
    /// `len` se redondea hacia arriba a 2 MiB.
    pub fn map_range_2m(
        &mut self,
        virt: u64,
        phys: u64,
        len: u64,
        flags: u64,
    ) -> Result<(), MapError> {
        let len = len.checked_add(HUGE_2M - 1).ok_or(MapError::Unaligned)? & !(HUGE_2M - 1);
        let mut off = 0;
        while off < len {
            self.map_2m(virt + off, phys + off, flags)?;
            off += HUGE_2M;
        }
        Ok(())
    }

    /// Mapea `[virt, virt+len)` -> `[phys, phys+len)` con páginas de 4 KiB.
    pub fn map_range_4k(
        &mut self,
        virt: u64,
        phys: u64,
        len: u64,
        flags: u64,
    ) -> Result<(), MapError> {
        let len = len.checked_add(PAGE_SIZE - 1).ok_or(MapError::Unaligned)? & !(PAGE_SIZE - 1);
        let mut off = 0;
        while off < len {
            self.map_4k(virt + off, phys + off, flags)?;
            off += PAGE_SIZE;
        }
        Ok(())
    }

    /// Deshace un `map_4k`. Devuelve la física que estaba mapeada. No libera
    /// frames ni invalida el TLB: eso es cosa del llamador.
    pub fn unmap_4k(&mut self, virt: u64) -> Result<u64, MapError> {
        if !virt.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Unaligned);
        }
        let pdpt = self
            .next_level(self.root, index(virt, 39), false)?
            .ok_or(MapError::NotMapped)?;
        let pd = self
            .next_level(pdpt, index(virt, 30), false)?
            .ok_or(MapError::NotMapped)?;
        let pt = self
            .next_level(pd, index(virt, 21), false)?
            .ok_or(MapError::NotMapped)?;
        let slot = &mut self.table_mut(pt)[index(virt, 12)];
        if *slot & PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        let phys = *slot & ADDR_MASK;
        *slot = 0;
        Ok(phys)
    }

    /// Física y flags de la hoja que cubre `virt` (4 KiB o 2 MiB).
    pub fn translate(&self, virt: u64) -> Option<(u64, u64)> {
        let mut table = self.root;
        for shift in [39u32, 30] {
            let e = self.table(table)[index(virt, shift)];
            if e & PRESENT == 0 {
                return None;
            }
            table = e & ADDR_MASK;
        }
        let pde = self.table(table)[index(virt, 21)];
        if pde & PRESENT == 0 {
            return None;
        }
        if pde & HUGE != 0 {
            let base = pde & ADDR_MASK & !(HUGE_2M - 1);
            return Some((base + (virt & (HUGE_2M - 1)), pde & !ADDR_MASK));
        }
        let pte = self.table(pde & ADDR_MASK)[index(virt, 12)];
        if pte & PRESENT == 0 {
            return None;
        }
        Some((
            (pte & ADDR_MASK) + (virt & (PAGE_SIZE - 1)),
            pte & !ADDR_MASK,
        ))
    }
}

/// Páginas de tabla necesarias, con margen, para `windows` mapas de 2 MiB
/// que cubran `limit` bytes cada uno más `kernel_pages` páginas de 4 KiB.
pub fn pool_size_for(limit: u64, windows: u64, kernel_pages: u64) -> usize {
    let gib = limit.div_ceil(1 << 30);
    // PML4 + por ventana (PDPT + un PD por GiB) + PDPT/PD altos + PTs + margen
    (1 + windows * (1 + gib) + 2 + kernel_pages.div_ceil(512) + 8) as usize
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
    use std::vec::Vec;

    /// Frames del heap del host; su dirección hace de "física" (identidad).
    struct HostFrames {
        live: Vec<*mut u8>,
        limit: usize,
    }
    impl HostFrames {
        fn new(limit: usize) -> Self {
            Self {
                live: Vec::new(),
                limit,
            }
        }
        fn layout() -> Layout {
            Layout::from_size_align(PAGE_SIZE as usize, PAGE_SIZE as usize).unwrap()
        }
    }
    impl FrameAlloc for HostFrames {
        fn alloc_frame(&mut self) -> Option<u64> {
            if self.live.len() >= self.limit {
                return None;
            }
            let p = unsafe { alloc_zeroed(Self::layout()) };
            self.live.push(p);
            Some(p as u64)
        }
    }
    impl Drop for HostFrames {
        fn drop(&mut self) {
            for p in self.live.drain(..) {
                unsafe { dealloc(p, Self::layout()) };
            }
        }
    }

    fn mapper(frames: usize) -> Mapper<HostFrames, IdentityMap> {
        Mapper::new(HostFrames::new(frames), IdentityMap).unwrap()
    }

    #[test]
    fn map_4k_translates_and_masks_flags() {
        let mut m = mapper(16);
        let virt = 0xFFFF_FFFF_8000_2000;
        m.map_4k(virt, 0x0010_0000, NO_EXECUTE | GLOBAL | 0xDEAD_B000 | HUGE)
            .unwrap();
        let (phys, flags) = m.translate(virt + 0x123).unwrap();
        assert_eq!(phys, 0x0010_0123);
        assert_eq!(
            flags,
            PRESENT | NO_EXECUTE | GLOBAL,
            "sin bits de direccion ni HUGE"
        );
        assert!(m.translate(virt + 0x1000).is_none());
        assert_eq!(m.map_4k(virt, 0x0020_0000, 0), Err(MapError::AlreadyMapped));
        assert_eq!(m.map_4k(virt + 1, 0, 0), Err(MapError::Unaligned));
        assert_eq!(m.allocator().live.len(), 4, "PML4 + PDPT + PD + PT");
    }

    #[test]
    fn map_2m_and_collisions() {
        let mut m = mapper(16);
        m.map_range_2m(0, 0, 4 * HUGE_2M, WRITABLE).unwrap();
        let (phys, flags) = m.translate(3 * HUGE_2M + 0x1234).unwrap();
        assert_eq!(phys, 3 * HUGE_2M + 0x1234);
        assert!(flags & HUGE != 0 && flags & WRITABLE != 0);
        assert!(m.translate(4 * HUGE_2M).is_none());
        // Bajar a 4K por debajo de una pagina grande no esta permitido.
        assert_eq!(
            m.map_4k(0x1000, 0x1000, 0),
            Err(MapError::HugePageCollision)
        );
        assert_eq!(m.map_2m(0, 0, 0), Err(MapError::AlreadyMapped));
        assert_eq!(m.map_2m(0x1000, 0, 0), Err(MapError::Unaligned));
    }

    #[test]
    fn unmap_returns_phys_and_clears() {
        let mut m = mapper(16);
        m.map_range_4k(0x4000, 0x9000, 2 * PAGE_SIZE, WRITABLE)
            .unwrap();
        assert_eq!(m.unmap_4k(0x5000), Ok(0xA000));
        assert!(m.translate(0x5000).is_none());
        assert_eq!(m.translate(0x4000).unwrap().0, 0x9000);
        assert_eq!(m.unmap_4k(0x5000), Err(MapError::NotMapped));
        assert_eq!(m.unmap_4k(0x9_0000_0000), Err(MapError::NotMapped));
        // Volver a mapear tras unmap funciona.
        m.map_4k(0x5000, 0xB000, 0).unwrap();
        assert_eq!(m.translate(0x5000).unwrap().0, 0xB000);
    }

    #[test]
    fn out_of_frames_is_reported() {
        let mut m = mapper(2); // PML4 + 1
        assert_eq!(m.map_4k(0x1000, 0x1000, 0), Err(MapError::OutOfFrames));
        assert!(Mapper::new(HostFrames::new(0), IdentityMap).is_err());
    }

    #[test]
    fn offset_map_and_bump_frames() {
        let om = OffsetMap(0x1000);
        assert_eq!(om.phys_to_virt(0x10) as u64, 0x1010);
        let mut b = unsafe { BumpFrames::new(0x10_0000, 2) };
        assert_eq!(b.alloc_frame(), Some(0x10_0000));
        assert_eq!(b.alloc_frame(), Some(0x10_1000));
        assert_eq!(b.alloc_frame(), None);
        assert_eq!(b.used_pages(), 2);
    }

    #[test]
    fn pool_size_covers_windows_and_kernel() {
        // Identidad + mapa fisico de 4 GiB, kernel de 16 paginas: 1 + 2*(1+4) + 2 + 1 + 8
        assert_eq!(pool_size_for(4 << 30, 2, 16), 22);
        assert!(pool_size_for(1 << 40, 1, 16) >= 1024 + 4);
    }
}
