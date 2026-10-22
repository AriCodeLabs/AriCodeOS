// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! 4-level x86-64 page tables, generic over how frames are obtained and
//! how they are accessed:
//!
//! - [`FrameAlloc`]: hands out 4 KiB physical frames (the loader uses a
//!   bump over UEFI pages; the kernel, the PMM).
//! - [`PhysMap`]: translates a physical address into a usable pointer (the
//!   loader uses identity; the kernel, the linear map at `PHYS_MAP_BASE`).
//!
//! Memory safety: `Mapper` only dereferences tables it created itself with
//! its `FrameAlloc`, through its `PhysMap`; it does not expose the tables
//! nor accept external pointers. The only `unsafe` operation for the caller
//! is building a `FrameAlloc`/`PhysMap` that honours its contract.
//!
//! `no_std`; tested on the host with heap frames and identity translation.

#![no_std]

pub const PAGE_SIZE: u64 = 4096;
pub const HUGE_2M: u64 = 2 * 1024 * 1024;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
/// Set by the CPU on the first access through the entry; never cleared by
/// the kernel (the user stack high-water mark, incident 0018, reads it).
pub const ACCESSED: u64 = 1 << 5;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Flags a caller may request for a leaf. Everything else is discarded.
const LEAF_FLAG_MASK: u64 = WRITABLE | USER | WRITE_THROUGH | CACHE_DISABLE | GLOBAL | NO_EXECUTE;

/// Source of 4 KiB physical frames. The contents may be garbage: the
/// `Mapper` zeroes them before using them as a table.
pub trait FrameAlloc {
    fn alloc_frame(&mut self) -> Option<u64>;
}

/// Physical address to accessible pointer translation.
///
/// # Safety
///
/// `phys_to_virt(p)` must return a valid pointer, aligned like `p`, to
/// memory that mirrors exactly the physical `p` (identity or active linear
/// map) for every frame the associated `FrameAlloc` can hand out.
pub unsafe trait PhysMap {
    fn phys_to_virt(&self, phys: u64) -> *mut u8;
}

/// Identity translation (virtual == physical). Valid while the active
/// tables map memory by identity.
pub struct IdentityMap;

// SAFETY: by the caller's contract, memory is identity-mapped.
unsafe impl PhysMap for IdentityMap {
    fn phys_to_virt(&self, phys: u64) -> *mut u8 {
        phys as *mut u8
    }
}

/// Constant-offset translation (linear map).
#[derive(Debug, Clone, Copy)]
pub struct OffsetMap(pub u64);

// SAFETY: by the caller's contract, physical `[0, limit)` is mapped at
// `offset + p` in the active tables.
unsafe impl PhysMap for OffsetMap {
    fn phys_to_virt(&self, phys: u64) -> *mut u8 {
        (phys + self.0) as *mut u8
    }
}

/// Bump allocator over a contiguous region of already reserved pages.
pub struct BumpFrames {
    base: u64,
    next: u64,
    end: u64,
}

impl BumpFrames {
    /// # Safety
    ///
    /// `base` aligned to 4 KiB, `pages * 4096` valid bytes for the exclusive
    /// use of this allocator while it lives.
    pub unsafe fn new(base: u64, pages: usize) -> Self {
        assert!(base.is_multiple_of(PAGE_SIZE), "base not aligned");
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
    /// The address is not aligned to the requested page size.
    Unaligned,
    /// A leaf already exists at that address.
    AlreadyMapped,
    /// An intermediate level is a huge page and cannot be descended.
    HugePageCollision,
    /// `unmap`/`translate` on something not mapped.
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
    /// Creates an empty hierarchy (zeroed PML4).
    pub fn new(alloc: A, map: M) -> Result<Self, MapError> {
        let mut m = Self {
            root: 0,
            alloc,
            map,
        };
        m.root = m.alloc_table()?;
        Ok(m)
    }

    /// Physical address of the PML4, ready for `cr3`.
    pub fn root_phys(&self) -> u64 {
        self.root
    }

    pub fn allocator(&mut self) -> &mut A {
        &mut self.alloc
    }

    fn alloc_table(&mut self) -> Result<u64, MapError> {
        let phys = self.alloc.alloc_frame().ok_or(MapError::OutOfFrames)?;
        // SAFETY: frame just handed out by our allocator, accessible by the
        // `PhysMap` contract; we zero it before using it.
        unsafe { core::ptr::write_bytes(self.map.phys_to_virt(phys), 0, PAGE_SIZE as usize) };
        Ok(phys)
    }

    fn table(&self, phys: u64) -> &[u64; 512] {
        // SAFETY: only called with tables created by `alloc_table` or the root;
        // the `PhysMap` guarantees the pointer.
        unsafe { &*(self.map.phys_to_virt(phys) as *const [u64; 512]) }
    }

    fn table_mut(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: same as `table`; `&mut self` gives exclusivity.
        unsafe { &mut *(self.map.phys_to_virt(phys) as *mut [u64; 512]) }
    }

    /// Lower-level table at `table[idx]`. With `create`, creates it if missing.
    /// `user` adds the USER bit to the intermediate entry (needed along the
    /// whole path for a user leaf to be accessible from ring 3).
    fn next_level(
        &mut self,
        table_phys: u64,
        idx: usize,
        create: bool,
        user: bool,
    ) -> Result<Option<u64>, MapError> {
        let entry = self.table(table_phys)[idx];
        if entry & PRESENT == 0 {
            if !create {
                return Ok(None);
            }
            let t = self.alloc_table()?;
            let user_bit = if user { USER } else { 0 };
            self.table_mut(table_phys)[idx] = t | PRESENT | WRITABLE | user_bit;
            return Ok(Some(t));
        }
        if entry & HUGE != 0 {
            return Err(MapError::HugePageCollision);
        }
        if user && entry & USER == 0 {
            self.table_mut(table_phys)[idx] = entry | USER;
        }
        Ok(Some(entry & ADDR_MASK))
    }

    /// Maps a 4 KiB page.
    pub fn map_4k(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
        if !virt.is_multiple_of(PAGE_SIZE) || !phys.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Unaligned);
        }
        let user = flags & USER != 0;
        let pdpt = self
            .next_level(self.root, index(virt, 39), true, user)?
            .expect("create");
        let pd = self
            .next_level(pdpt, index(virt, 30), true, user)?
            .expect("create");
        let pt = self
            .next_level(pd, index(virt, 21), true, user)?
            .expect("create");
        let slot = &mut self.table_mut(pt)[index(virt, 12)];
        if *slot & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *slot = (phys & ADDR_MASK) | (flags & LEAF_FLAG_MASK) | PRESENT;
        Ok(())
    }

    /// Maps a 2 MiB page.
    pub fn map_2m(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
        if !virt.is_multiple_of(HUGE_2M) || !phys.is_multiple_of(HUGE_2M) {
            return Err(MapError::Unaligned);
        }
        let user = flags & USER != 0;
        let pdpt = self
            .next_level(self.root, index(virt, 39), true, user)?
            .expect("create");
        let pd = self
            .next_level(pdpt, index(virt, 30), true, user)?
            .expect("create");
        let slot = &mut self.table_mut(pd)[index(virt, 21)];
        if *slot & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *slot = (phys & ADDR_MASK) | (flags & LEAF_FLAG_MASK) | HUGE | PRESENT;
        Ok(())
    }

    /// Maps `[virt, virt+len)` -> `[phys, phys+len)` with 2 MiB pages.
    /// `len` is rounded up to 2 MiB.
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

    /// Maps `[virt, virt+len)` -> `[phys, phys+len)` with 4 KiB pages.
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
            if let Err(e) = self.map_4k(virt + off, phys + off, flags) {
                // All or nothing (incident 0004): a half-mapped range would
                // leave leaves the caller does not know about (pointing at
                // frames it does not own). The pages this call mapped go
                // again; tables it created stay, empty.
                let mut back = 0;
                while back < off {
                    let _ = self.unmap_4k(virt + back);
                    back += PAGE_SIZE;
                }
                return Err(e);
            }
            off += PAGE_SIZE;
        }
        Ok(())
    }

    /// Undoes a `map_4k`. Returns the physical address that was mapped. Does not
    /// free frames nor invalidate the TLB: that is up to the caller.
    pub fn unmap_4k(&mut self, virt: u64) -> Result<u64, MapError> {
        if !virt.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Unaligned);
        }
        let pdpt = self
            .next_level(self.root, index(virt, 39), false, false)?
            .ok_or(MapError::NotMapped)?;
        let pd = self
            .next_level(pdpt, index(virt, 30), false, false)?
            .ok_or(MapError::NotMapped)?;
        let pt = self
            .next_level(pd, index(virt, 21), false, false)?
            .ok_or(MapError::NotMapped)?;
        let slot = &mut self.table_mut(pt)[index(virt, 12)];
        if *slot & PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        let phys = *slot & ADDR_MASK;
        *slot = 0;
        Ok(phys)
    }

    /// Physical address and flags of the leaf covering `virt` (4 KiB or 2 MiB).
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

/// PML4 entries that belong to the kernel (upper half).
pub const KERNEL_PML4_FIRST: usize = 256;

impl<A: FrameAlloc, M: PhysMap> Mapper<A, M> {
    /// Copies entries 256..512 of the `from_root` PML4 (the kernel half)
    /// into this mapper: the kernel PDPTs become shared.
    ///
    /// # Safety
    ///
    /// `from_root` must be a valid PML4 accessible through `self.map`.
    pub unsafe fn share_kernel_half(&mut self, from_root: u64) {
        let src = &*(self.map.phys_to_virt(from_root) as *const [u64; 512]);
        let dst = self.table_mut(self.root);
        dst[KERNEL_PML4_FIRST..].copy_from_slice(&src[KERNEL_PML4_FIRST..]);
    }

    /// Pre-creates the PDPT for every entry of the PML4 upper half.
    /// This way `share_kernel_half` shares PDPTs and not empty entries: any
    /// mapping the kernel adds later is visible in every user address space
    /// without having to propagate it.
    pub fn preallocate_kernel_half(&mut self) -> Result<(), MapError> {
        for i in KERNEL_PML4_FIRST..512 {
            if self.table(self.root)[i] & PRESENT == 0 {
                let t = self.alloc_table()?;
                self.table_mut(self.root)[i] = t | PRESENT | WRITABLE;
            }
        }
        Ok(())
    }

    /// Frees the whole lower (user) half: 4 KiB and 2 MiB leaves and
    /// intermediate tables, handing each frame to `free`. Kernel entries are
    /// left untouched. Afterwards the PML4 is still valid and empty below.
    pub fn free_user_half(&mut self, mut free: impl FnMut(u64)) {
        for i in 0..KERNEL_PML4_FIRST {
            let e = self.table(self.root)[i];
            if e & PRESENT == 0 {
                continue;
            }
            let pdpt = e & ADDR_MASK;
            for j in 0..512 {
                let e = self.table(pdpt)[j];
                if e & PRESENT == 0 {
                    continue;
                }
                let pd = e & ADDR_MASK;
                for k in 0..512 {
                    let e = self.table(pd)[k];
                    if e & PRESENT == 0 {
                        continue;
                    }
                    if e & HUGE != 0 {
                        free(e & ADDR_MASK & !(HUGE_2M - 1));
                        continue;
                    }
                    let pt = e & ADDR_MASK;
                    for l in 0..512 {
                        let e = self.table(pt)[l];
                        if e & PRESENT != 0 {
                            free(e & ADDR_MASK);
                        }
                    }
                    free(pt);
                }
                free(pd);
            }
            free(pdpt);
            self.table_mut(self.root)[i] = 0;
        }
    }

    /// Frees the PML4 (after `free_user_half`) and consumes the mapper.
    pub fn into_root_frame(self) -> u64 {
        self.root
    }
}

/// Table pages needed, with margin, for `windows` 2 MiB maps each covering
/// `limit` bytes plus `kernel_pages` 4 KiB pages.
pub fn pool_size_for(limit: u64, windows: u64, kernel_pages: u64) -> usize {
    let gib = limit.div_ceil(1 << 30);
    // PML4 + per window (PDPT + one PD per GiB) + upper PDPT/PD + PTs + margin
    (1 + windows * (1 + gib) + 2 + kernel_pages.div_ceil(512) + 8) as usize
}

/// Enables EFER.NXE so the NX bit is valid (OVMF usually does it; do not rely on it).
///
/// # Safety
///
/// Must run in ring 0 on x86-64; modifies an MSR of the current CPU.
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

    /// Host heap frames; their address acts as the "physical" one (identity).
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
            "no address bits nor HUGE"
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
        // Descending to 4K below a huge page is not allowed.
        assert_eq!(
            m.map_4k(0x1000, 0x1000, 0),
            Err(MapError::HugePageCollision)
        );
        assert_eq!(m.map_2m(0, 0, 0), Err(MapError::AlreadyMapped));
        assert_eq!(m.map_2m(0x1000, 0, 0), Err(MapError::Unaligned));
    }

    #[test]
    fn a_failed_range_leaves_nothing_mapped() {
        // PML4 + PDPT + PD + one PT: the first 2 MiB fit, the page past it
        // needs a second PT that the allocator cannot give.
        let mut m = mapper(4);
        let virt = 0x20_0000 - 2 * PAGE_SIZE;
        assert_eq!(
            m.map_range_4k(virt, 0x0050_0000, 4 * PAGE_SIZE, WRITABLE),
            Err(MapError::OutOfFrames)
        );
        for i in 0..4 {
            assert!(m.translate(virt + i * PAGE_SIZE).is_none(), "page {i}");
        }
        // An occupied page in the middle refuses the whole range and
        // keeps the page that was there before.
        m.map_4k(0x5000, 0x0060_0000, WRITABLE).unwrap();
        assert_eq!(
            m.map_range_4k(0x3000, 0x0070_0000, 4 * PAGE_SIZE, WRITABLE),
            Err(MapError::AlreadyMapped)
        );
        assert!(m.translate(0x3000).is_none() && m.translate(0x4000).is_none());
        assert_eq!(m.translate(0x5000).map(|t| t.0), Some(0x0060_0000));
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
        // Mapping again after unmap works.
        m.map_4k(0x5000, 0xB000, 0).unwrap();
        assert_eq!(m.translate(0x5000).unwrap().0, 0xB000);
    }

    #[test]
    fn user_mappings_set_user_on_the_whole_path() {
        let mut m = mapper(16);
        m.map_4k(0x40_0000, 0x10_0000, USER | WRITABLE).unwrap();
        let root = m.root_phys();
        let pml4e = unsafe { (*(root as *const [u64; 512]))[0] };
        assert!(pml4e & USER != 0, "PML4E without USER");
        let pdpte = unsafe { (*((pml4e & ADDR_MASK) as *const [u64; 512]))[0] };
        assert!(pdpte & USER != 0, "PDPTE without USER");
        let (_, flags) = m.translate(0x40_0000).unwrap();
        assert!(flags & USER != 0);
        // A kernel mapping in the same PD does not strip USER from the path.
        m.map_4k(0x40_1000, 0x10_1000, WRITABLE).unwrap();
        let pml4e2 = unsafe { (*(root as *const [u64; 512]))[0] };
        assert!(pml4e2 & USER != 0);
    }

    #[test]
    fn preallocated_kernel_half_makes_later_mappings_visible() {
        let mut kernel = mapper(300);
        kernel.preallocate_kernel_half().unwrap();
        let mut user = mapper(8);
        unsafe { user.share_kernel_half(kernel.root_phys()) };
        // A later mapping in a new upper region is visible from the user address space.
        kernel
            .map_4k(0xFFFF_D200_0000_0000, 0x30_0000, GLOBAL)
            .unwrap();
        assert_eq!(
            user.translate(0xFFFF_D200_0000_0000).map(|t| t.0),
            Some(0x30_0000)
        );
        assert_eq!(
            kernel.allocator().live.len(),
            1 + 256 + 2,
            "PML4 + 256 PDPT + PD + PT"
        );
        assert_eq!(
            Mapper::new(HostFrames::new(10), IdentityMap)
                .unwrap()
                .preallocate_kernel_half(),
            Err(MapError::OutOfFrames)
        );
    }

    #[test]
    fn share_kernel_half_and_free_user_half() {
        let mut kernel = mapper(32);
        kernel
            .map_4k(0xFFFF_FFFF_8000_0000, 0x20_0000, GLOBAL)
            .unwrap();
        let mut user = mapper(32);
        unsafe { user.share_kernel_half(kernel.root_phys()) };
        assert_eq!(
            user.translate(0xFFFF_FFFF_8000_0000).map(|t| t.0),
            Some(0x20_0000)
        );
        user.map_4k(0x40_0000, 0x10_0000, USER).unwrap();
        user.map_2m(0x80_0000, 0x40_0000, USER | WRITABLE).unwrap();
        let mut freed = Vec::new();
        user.free_user_half(|p| freed.push(p));
        // Hojas: 0x10_0000 (4K) y 0x40_0000 (2M); tablas: PT, PD, PDPT.
        assert!(freed.contains(&0x10_0000) && freed.contains(&0x40_0000));
        assert_eq!(freed.len(), 5, "{freed:x?}");
        assert!(user.translate(0x40_0000).is_none());
        assert_eq!(
            user.translate(0xFFFF_FFFF_8000_0000).map(|t| t.0),
            Some(0x20_0000),
            "kernel intact"
        );
        let root = user.into_root_frame();
        assert_ne!(root, 0);
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
        // Identity + 4 GiB physical map, 16-page kernel: 1 + 2*(1+4) + 2 + 1 + 8
        assert_eq!(pool_size_for(4 << 30, 2, 16), 22);
        assert!(pool_size_for(1 << 40, 1, 16) >= 1024 + 4);
    }
}
