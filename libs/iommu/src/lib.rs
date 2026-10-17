// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Intel VT-d (Phase 33): what the kernel needs to isolate each driver's DMA
//! in its own domain, without touching hardware: register offsets and bits,
//! capability decoding, root and context table entries, second-level paging
//! (3 or 4 levels) and fault records. Everything is tested on the host; the
//! kernel only reads and writes MMIO.

#![no_std]

use aricode_x86_paging::{FrameAlloc, PhysMap, PAGE_SIZE};

/// Register offsets of a remapping unit.
pub mod regs {
    pub const VERSION: u64 = 0x00;
    pub const CAPABILITY: u64 = 0x08;
    pub const EXTENDED_CAPABILITY: u64 = 0x10;
    pub const GLOBAL_COMMAND: u64 = 0x18;
    pub const GLOBAL_STATUS: u64 = 0x1C;
    pub const ROOT_TABLE_ADDRESS: u64 = 0x20;
    pub const CONTEXT_COMMAND: u64 = 0x28;
    pub const FAULT_STATUS: u64 = 0x34;
    pub const FAULT_EVENT_CONTROL: u64 = 0x38;
    pub const FAULT_EVENT_DATA: u64 = 0x3C;
    pub const FAULT_EVENT_ADDRESS: u64 = 0x40;
    pub const FAULT_EVENT_UPPER_ADDRESS: u64 = 0x44;
    /// The IOTLB register is at `iro + 8`; IVA at `iro` (see `ExtendedCapability::iotlb_offset`).
    pub const IOTLB_FROM_IRO: u64 = 8;
}

/// Global Command / Global Status.
pub mod gcmd {
    pub const TRANSLATION_ENABLE: u32 = 1 << 31;
    pub const SET_ROOT_TABLE_POINTER: u32 = 1 << 30;
    pub const WRITE_BUFFER_FLUSH: u32 = 1 << 27;
}

/// Context Command (64 bits).
pub mod ccmd {
    pub const INVALIDATE: u64 = 1 << 63;
    /// Global granularity (CIRG = 01).
    pub const GLOBAL: u64 = 1 << 61;
}

/// IOTLB register (64 bits).
pub mod iotlb {
    pub const INVALIDATE: u64 = 1 << 63;
    /// Global granularity (IIRG = 01).
    pub const GLOBAL: u64 = 1 << 60;
    pub const DRAIN_READS: u64 = 1 << 49;
    pub const DRAIN_WRITES: u64 = 1 << 48;
}

/// Fault Status.
pub mod fsts {
    pub const PRIMARY_FAULT_OVERFLOW: u32 = 1 << 0;
    pub const PRIMARY_PENDING_FAULT: u32 = 1 << 1;
    /// Write-1-to-clear bits (all but PPF, which is read-only).
    pub const CLEAR_MASK: u32 = 0xFD;
}

/// Fault Event Control.
pub mod fectl {
    pub const INTERRUPT_MASK: u32 = 1 << 31;
    pub const INTERRUPT_PENDING: u32 = 1 << 30;
}

/// Capability register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability(pub u64);

impl Capability {
    /// Domains supported by the unit: `2^(4 + 2*ND)`.
    pub fn domains(self) -> u32 {
        1 << (4 + 2 * (self.0 & 0b111) as u32)
    }
    /// Supported guest address bits (MGAW + 1).
    pub fn guest_address_width(self) -> u8 {
        (((self.0 >> 16) & 0x3F) + 1) as u8
    }
    /// Supported second-level paging widths (SAGAW): bit 1 =
    /// 39 bits (3 levels), bit 2 = 48 bits (4 levels).
    pub fn sagaw(self) -> u8 {
        ((self.0 >> 8) & 0x1F) as u8
    }
    /// Paging levels to use: the largest supported up to 4.
    pub fn levels(self) -> Option<u8> {
        if self.sagaw() & (1 << 2) != 0 {
            Some(4)
        } else if self.sagaw() & (1 << 1) != 0 {
            Some(3)
        } else {
            None
        }
    }
    /// Byte offset of the first fault record register.
    pub fn fault_record_offset(self) -> u64 {
        ((self.0 >> 24) & 0x3FF) * 16
    }
    /// Number of fault record registers.
    pub fn fault_records(self) -> u32 {
        (((self.0 >> 40) & 0xFF) + 1) as u32
    }
    /// The write buffer must be flushed after modifying tables.
    pub fn write_buffer_flush(self) -> bool {
        self.0 & (1 << 4) != 0
    }
    /// Caching mode: the unit also caches non-present entries and every
    /// mapping must be invalidated (emulators).
    pub fn caching_mode(self) -> bool {
        self.0 & (1 << 7) != 0
    }
    pub fn drain_reads(self) -> bool {
        self.0 & (1 << 55) != 0
    }
    pub fn drain_writes(self) -> bool {
        self.0 & (1 << 54) != 0
    }
}

/// Extended capability register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedCapability(pub u64);

impl ExtendedCapability {
    /// The unit's table accesses are coherent with the CPU cache; otherwise
    /// every written entry must be `clflush`ed.
    pub fn coherent(self) -> bool {
        self.0 & 1 != 0
    }
    /// Byte offset of the IVA/IOTLB registers.
    pub fn iotlb_offset(self) -> u64 {
        ((self.0 >> 8) & 0x3FF) * 16
    }
    pub fn interrupt_remapping(self) -> bool {
        self.0 & (1 << 3) != 0
    }
}

/// Root table entry (16 bytes, one per bus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RootEntry {
    pub low: u64,
    pub high: u64,
}

impl RootEntry {
    pub const NOT_PRESENT: RootEntry = RootEntry { low: 0, high: 0 };

    pub fn new(context_table_phys: u64) -> Self {
        debug_assert!(context_table_phys.is_multiple_of(PAGE_SIZE));
        RootEntry {
            low: context_table_phys | 1,
            high: 0,
        }
    }
    pub fn present(self) -> bool {
        self.low & 1 != 0
    }
    pub fn context_table(self) -> u64 {
        self.low & 0x000F_FFFF_FFFF_F000
    }
}

/// Context entry (16 bytes, one per function of a bus): domain and root of
/// its second-level tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ContextEntry {
    pub low: u64,
    pub high: u64,
}

impl ContextEntry {
    pub const NOT_PRESENT: ContextEntry = ContextEntry { low: 0, high: 0 };

    /// Second-level translation (TT = 00) with `levels` levels (3 or 4).
    pub fn new(second_level_root: u64, domain: u16, levels: u8) -> Self {
        debug_assert!(second_level_root.is_multiple_of(PAGE_SIZE));
        let aw = match levels {
            3 => 0b001,
            4 => 0b010,
            _ => panic!("unsupported paging levels"),
        };
        ContextEntry {
            low: second_level_root | 1,
            high: aw | (domain as u64) << 8,
        }
    }
    pub fn present(self) -> bool {
        self.low & 1 != 0
    }
    pub fn domain(self) -> u16 {
        ((self.high >> 8) & 0xFFFF) as u16
    }
    pub fn second_level_root(self) -> u64 {
        self.low & 0x000F_FFFF_FFFF_F000
    }
}

/// A DMA fault recorded by the unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultRecord {
    /// Request address (page aligned).
    pub address: u64,
    /// `bus << 8 | dev << 3 | func`.
    pub source_id: u16,
    pub reason: u8,
    pub is_read: bool,
}

impl FaultRecord {
    /// Decodes a 128-bit record; `None` if there is no fault (F = 0).
    pub fn decode(low: u64, high: u64) -> Option<Self> {
        if high & (1 << 63) == 0 {
            return None;
        }
        Some(FaultRecord {
            address: low & !0xFFF,
            source_id: (high & 0xFFFF) as u16,
            reason: ((high >> 32) & 0xFF) as u8,
            is_read: high & (1 << 62) != 0,
        })
    }

    pub fn bus(self) -> u8 {
        (self.source_id >> 8) as u8
    }
    pub fn device(self) -> u8 {
        ((self.source_id >> 3) & 0x1F) as u8
    }
    pub fn function(self) -> u8 {
        (self.source_id & 0x7) as u8
    }
}

/// PCI source identifier as used by the unit.
pub const fn source_id(bus: u8, device: u8, function: u8) -> u16 {
    (bus as u16) << 8 | (device as u16 & 0x1F) << 3 | (function as u16 & 0x7)
}

/// Name of a translation fault reason (table from the specification).
pub fn fault_reason_name(reason: u8) -> &'static str {
    match reason {
        0x1 => "root entry not present",
        0x2 => "context entry not present",
        0x3 => "invalid context entry",
        0x4 => "address beyond supported width",
        0x5 => "write to page without write permission",
        0x6 => "read from page without read permission",
        0x7 => "second-level entry not present",
        0x8 => "error reading root table",
        0x9 => "error reading context table",
        0xA => "reserved field in root entry",
        0xB => "reserved field in context entry",
        0xC => "reserved field in second-level paging",
        0xD => "error reading second-level paging",
        _ => "unknown reason",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IommuError {
    OutOfFrames,
    Unaligned,
    /// The address does not fit in the configured levels.
    OutOfRange,
    AlreadyMapped,
    NotMapped,
}

const SL_READ: u64 = 1 << 0;
const SL_WRITE: u64 = 1 << 1;
const SL_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Second-level paging tables of a domain: they translate the address the
/// device emits (IOVA) to physical. Each entry carries R/W permissions
/// instead of PRESENT; an entry with both cleared does not exist.
/// `flush` is called with the virtual address of each written entry (for
/// `clflush` when the unit is not cache coherent).
pub struct SecondLevel<A: FrameAlloc, M: PhysMap> {
    root: u64,
    levels: u8,
    alloc: A,
    map: M,
    flush: Option<fn(u64)>,
}

impl<A: FrameAlloc, M: PhysMap> SecondLevel<A, M> {
    pub fn new(mut alloc: A, map: M, levels: u8, flush: Option<fn(u64)>) -> Option<Self> {
        assert!(matches!(levels, 3 | 4), "unsupported levels");
        let root = alloc.alloc_frame()?;
        let mut sl = Self {
            root,
            levels,
            alloc,
            map,
            flush,
        };
        sl.zero(root);
        Some(sl)
    }

    pub fn root_phys(&self) -> u64 {
        self.root
    }
    pub fn levels(&self) -> u8 {
        self.levels
    }
    /// First address that can no longer be translated.
    pub fn limit(&self) -> u64 {
        1 << (12 + 9 * self.levels as u64)
    }

    fn table(&self, phys: u64) -> &[u64; 512] {
        // SAFETY: `phys` is a table created by this mapper (or its root),
        // accessible through `map` per the `PhysMap` contract.
        unsafe { &*(self.map.phys_to_virt(phys) as *const [u64; 512]) }
    }

    fn table_mut(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: same as `table`; `&mut self` gives exclusive access.
        unsafe { &mut *(self.map.phys_to_virt(phys) as *mut [u64; 512]) }
    }

    fn zero(&mut self, phys: u64) {
        let flush = self.flush;
        let t = self.table_mut(phys);
        *t = [0; 512];
        if let Some(f) = flush {
            for i in (0..512).step_by(8) {
                f(&t[i] as *const u64 as u64);
            }
        }
    }

    fn write(&mut self, table: u64, index: usize, value: u64) {
        let flush = self.flush;
        let t = self.table_mut(table);
        // SAFETY: entry of one of our own tables; volatile write so the unit
        // sees it in program order.
        unsafe { core::ptr::write_volatile(&mut t[index], value) };
        if let Some(f) = flush {
            f(&t[index] as *const u64 as u64);
        }
    }

    fn index(iova: u64, level: u8) -> usize {
        ((iova >> (12 + 9 * level as u64)) & 0x1FF) as usize
    }

    /// Leaf table (level 1) for `iova`, creating intermediate ones if `create`.
    fn leaf_table(&mut self, iova: u64, create: bool) -> Result<Option<u64>, IommuError> {
        let mut table = self.root;
        for level in (1..self.levels).rev() {
            let idx = Self::index(iova, level);
            let e = self.table(table)[idx];
            if e & (SL_READ | SL_WRITE) == 0 {
                if !create {
                    return Ok(None);
                }
                let f = self.alloc.alloc_frame().ok_or(IommuError::OutOfFrames)?;
                self.zero(f);
                self.write(table, idx, f | SL_READ | SL_WRITE);
                table = f;
            } else {
                table = e & SL_ADDR_MASK;
            }
        }
        Ok(Some(table))
    }

    fn check(&self, iova: u64) -> Result<(), IommuError> {
        if !iova.is_multiple_of(PAGE_SIZE) {
            return Err(IommuError::Unaligned);
        }
        if iova >= self.limit() {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// Maps a 4 KiB page: `iova -> phys`, always readable and writable
    /// if `write`.
    pub fn map_4k(&mut self, iova: u64, phys: u64, write: bool) -> Result<(), IommuError> {
        self.check(iova)?;
        if !phys.is_multiple_of(PAGE_SIZE) {
            return Err(IommuError::Unaligned);
        }
        let leaf = self
            .leaf_table(iova, true)?
            .expect("with create there is always a leaf");
        let idx = Self::index(iova, 0);
        if self.table(leaf)[idx] & (SL_READ | SL_WRITE) != 0 {
            return Err(IommuError::AlreadyMapped);
        }
        let mut e = (phys & SL_ADDR_MASK) | SL_READ;
        if write {
            e |= SL_WRITE;
        }
        self.write(leaf, idx, e);
        Ok(())
    }

    /// Removes page `iova` and returns the physical address it had.
    pub fn unmap_4k(&mut self, iova: u64) -> Result<u64, IommuError> {
        self.check(iova)?;
        let leaf = self.leaf_table(iova, false)?.ok_or(IommuError::NotMapped)?;
        let idx = Self::index(iova, 0);
        let e = self.table(leaf)[idx];
        if e & (SL_READ | SL_WRITE) == 0 {
            return Err(IommuError::NotMapped);
        }
        self.write(leaf, idx, 0);
        Ok(e & SL_ADDR_MASK)
    }

    /// Physical address and write permission of `iova`, if mapped.
    pub fn translate(&mut self, iova: u64) -> Option<(u64, bool)> {
        let page = iova & !(PAGE_SIZE - 1);
        self.check(page).ok()?;
        let leaf = self.leaf_table(page, false).ok()??;
        let e = self.table(leaf)[Self::index(iova, 0)];
        if e & (SL_READ | SL_WRITE) == 0 {
            return None;
        }
        Some((e & SL_ADDR_MASK | (iova & 0xFFF), e & SL_WRITE != 0))
    }

    /// Hands every table frame (intermediate and root) to `free` and
    /// consumes the mapper. Leaf pages are not its own.
    pub fn free_tables(self, mut free: impl FnMut(u64)) {
        fn walk<A: FrameAlloc, M: PhysMap>(
            sl: &SecondLevel<A, M>,
            table: u64,
            level: u8,
            free: &mut impl FnMut(u64),
        ) {
            if level > 1 {
                for i in 0..512 {
                    let e = sl.table(table)[i];
                    if e & (SL_READ | SL_WRITE) != 0 {
                        walk(sl, e & SL_ADDR_MASK, level - 1, free);
                    }
                }
            }
            free(table);
        }
        walk(&self, self.root, self.levels, &mut free);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use aricode_x86_paging::IdentityMap;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::alloc::{alloc_zeroed, dealloc, Layout};
    use std::vec::Vec;

    struct HostFrames {
        live: Vec<*mut u8>,
    }
    impl HostFrames {
        fn layout() -> Layout {
            Layout::from_size_align(PAGE_SIZE as usize, PAGE_SIZE as usize).unwrap()
        }
    }
    impl FrameAlloc for HostFrames {
        fn alloc_frame(&mut self) -> Option<u64> {
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

    static FLUSHED: AtomicUsize = AtomicUsize::new(0);
    fn count_flush(_addr: u64) {
        FLUSHED.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn capability_fields() {
        // ND = 2 (256 domains), SAGAW = 0b110 (39 and 48 bits), MGAW = 47
        // (48 bits), FRO = 0x22 (0x220 bytes), NFR = 0 (1 record), CM.
        let cap = Capability(
            0b010
                | (0b110 << 8)
                | (47 << 16)
                | (0x22 << 24)
                | (0 << 40)
                | (1 << 7)
                | (1 << 4)
                | (1 << 55),
        );
        assert_eq!(cap.domains(), 256);
        assert_eq!(cap.sagaw(), 0b110);
        assert_eq!(cap.levels(), Some(4));
        assert_eq!(cap.guest_address_width(), 48);
        assert_eq!(cap.fault_record_offset(), 0x220);
        assert_eq!(cap.fault_records(), 1);
        assert!(cap.caching_mode());
        assert!(cap.write_buffer_flush());
        assert!(cap.drain_reads());
        assert!(!cap.drain_writes());
        assert_eq!(Capability(0b010 << 8).levels(), Some(3));
        assert_eq!(Capability(0).levels(), None);
        let ecap = ExtendedCapability(1 | (0x10 << 8) | (1 << 3));
        assert!(ecap.coherent());
        assert_eq!(ecap.iotlb_offset(), 0x100);
        assert!(ecap.interrupt_remapping());
    }

    #[test]
    fn root_and_context_entries() {
        let r = RootEntry::new(0x1234_5000);
        assert!(r.present());
        assert_eq!(r.context_table(), 0x1234_5000);
        assert!(!RootEntry::NOT_PRESENT.present());
        let c = ContextEntry::new(0xABCD_E000, 7, 4);
        assert!(c.present());
        assert_eq!(c.domain(), 7);
        assert_eq!(c.second_level_root(), 0xABCD_E000);
        assert_eq!(c.high & 0b111, 0b010, "48-bit AW");
        assert_eq!(c.low & 0b1100, 0, "TT = second level");
        assert_eq!(ContextEntry::new(0x1000, 1, 3).high & 0b111, 0b001);
        assert!(!ContextEntry::NOT_PRESENT.present());
    }

    #[test]
    fn fault_record_decoding() {
        assert_eq!(FaultRecord::decode(0x1000, 0), None);
        let high = (1u64 << 63) | (1 << 62) | (0x7u64 << 32) | source_id(0, 2, 0) as u64;
        let f = FaultRecord::decode(0xDEAD_BEEF, high).unwrap();
        assert_eq!(f.address, 0xDEAD_B000);
        assert_eq!((f.bus(), f.device(), f.function()), (0, 2, 0));
        assert_eq!(f.reason, 7);
        assert!(f.is_read);
        assert_eq!(fault_reason_name(7), "second-level entry not present");
        assert_eq!(fault_reason_name(0x77), "unknown reason");
        assert_eq!(source_id(1, 31, 7), 0x01FF);
    }

    #[test]
    fn second_level_map_translate_unmap_and_limits() {
        for levels in [3u8, 4] {
            let mut sl =
                SecondLevel::new(HostFrames { live: Vec::new() }, IdentityMap, levels, None)
                    .unwrap();
            assert_eq!(sl.limit(), 1 << (12 + 9 * levels as u64));
            assert_eq!(sl.map_4k(0x1000, 0x5000, true), Ok(()));
            assert_eq!(sl.map_4k(0x2000, 0x6000, false), Ok(()));
            assert_eq!(
                sl.map_4k(0x1000, 0x7000, true),
                Err(IommuError::AlreadyMapped)
            );
            assert_eq!(sl.map_4k(0x1001, 0x7000, true), Err(IommuError::Unaligned));
            assert_eq!(
                sl.map_4k(sl.limit(), 0x7000, true),
                Err(IommuError::OutOfRange)
            );
            assert_eq!(sl.translate(0x1234), Some((0x5234, true)));
            assert_eq!(sl.translate(0x2000), Some((0x6000, false)));
            assert_eq!(sl.translate(0x3000), None);
            assert_eq!(sl.translate(sl.limit() - PAGE_SIZE), None);
            // The last translatable page works too.
            let last = sl.limit() - PAGE_SIZE;
            assert_eq!(sl.map_4k(last, 0x9000, true), Ok(()));
            assert_eq!(sl.translate(last), Some((0x9000, true)));
            assert_eq!(sl.unmap_4k(0x1000), Ok(0x5000));
            assert_eq!(sl.unmap_4k(0x1000), Err(IommuError::NotMapped));
            assert_eq!(sl.unmap_4k(0x8000), Err(IommuError::NotMapped));
            assert_eq!(sl.translate(0x1000), None);
            // Tables: root + (levels-1) intermediate for 0x1000 and another
            // (levels-1) for the last page.
            let mut freed = 0;
            sl.free_tables(|_| freed += 1);
            assert_eq!(freed, 1 + 2 * (levels as usize - 1));
        }
    }

    #[test]
    fn flush_hook_runs_for_every_written_entry() {
        FLUSHED.store(0, Ordering::Relaxed);
        let mut sl = SecondLevel::new(
            HostFrames { live: Vec::new() },
            IdentityMap,
            3,
            Some(count_flush),
        )
        .unwrap();
        let after_root = FLUSHED.load(Ordering::Relaxed);
        assert_eq!(after_root, 64, "the root is zeroed in 64-byte lines");
        sl.map_4k(0x1000, 0x2000, true).unwrap();
        // Two new tables (2 x 64 lines) + 2 intermediate entries + 1 leaf.
        assert_eq!(FLUSHED.load(Ordering::Relaxed) - after_root, 2 * 64 + 3);
    }
}
