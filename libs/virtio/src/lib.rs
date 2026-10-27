// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Virtio 1.x (section 40). Everything that can be tested on the host: the
//! layout of a split virtqueue over a DMA buffer, discovery of the modern
//! virtio-pci structures in the configuration space and the virtio-blk
//! requests. The driver performs the real MMIO accesses.

#![no_std]

use core::sync::atomic::{fence, Ordering};

/// Device states (`device_status` register).
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const NEEDS_RESET: u8 = 64;
    pub const FAILED: u8 = 128;
}

/// Feature bit mandatory in virtio 1.x (high word, bit 0).
pub const FEATURE_VERSION_1: u64 = 1 << 32;
/// The device accesses memory through the platform IOMMU: the
/// driver must accept it if offered (in AriCodeOS IOVA = physical, so
/// nothing else changes).
pub const FEATURE_ACCESS_PLATFORM: u64 = 1 << 33;

/// Offsets in the `virtio_pci_common_cfg` structure.
pub mod common {
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    pub const DEVICE_FEATURE: u64 = 0x04;
    pub const DRIVER_FEATURE_SELECT: u64 = 0x08;
    pub const DRIVER_FEATURE: u64 = 0x0C;
    pub const MSIX_CONFIG: u64 = 0x10;
    pub const NUM_QUEUES: u64 = 0x12;
    pub const DEVICE_STATUS: u64 = 0x14;
    pub const CONFIG_GENERATION: u64 = 0x15;
    pub const QUEUE_SELECT: u64 = 0x16;
    pub const QUEUE_SIZE: u64 = 0x18;
    pub const QUEUE_MSIX_VECTOR: u64 = 0x1A;
    pub const QUEUE_ENABLE: u64 = 0x1C;
    pub const QUEUE_NOTIFY_OFF: u64 = 0x1E;
    pub const QUEUE_DESC: u64 = 0x20;
    pub const QUEUE_DRIVER: u64 = 0x28;
    pub const QUEUE_DEVICE: u64 = 0x30;
    /// Bytes the structure takes up to `QUEUE_DEVICE`'s end.
    pub const LEN: u64 = 0x38;
}

/// Structure types announced by the virtio-pci vendor capabilities.
pub mod cfg_type {
    pub const COMMON: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const ISR: u8 = 3;
    pub const DEVICE: u8 = 4;
}

/// A virtio structure located in a BAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciStructure {
    pub cfg_type: u8,
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    /// Only for `NOTIFY`.
    pub notify_off_multiplier: u32,
}

/// Locates the virtio structures in a PCI configuration space.
pub fn find_structures(cfg: &[u8]) -> [Option<PciStructure>; 5] {
    let mut out = [None; 5];
    for cap in aricode_pci::capabilities(cfg) {
        if cap.id != aricode_pci::CAP_VENDOR {
            continue;
        }
        let o = cap.offset as usize;
        if o + 16 > cfg.len() {
            continue;
        }
        let cfg_type = cfg[o + 3];
        // BARs are numbered 0 to 5 (virtio 1.2, 4.1.4); another number
        // would select nothing, or shift past the driver's BAR mask
        // (audit 2026-10-25 A, L1).
        if !(1..=4).contains(&cfg_type) || cfg[o + 4] > 5 {
            continue;
        }
        let u32_at = |p: usize| u32::from_le_bytes(cfg[p..p + 4].try_into().unwrap());
        let notify_off_multiplier = if cfg_type == cfg_type::NOTIFY && o + 20 <= cfg.len() {
            u32_at(o + 16)
        } else {
            0
        };
        let s = PciStructure {
            cfg_type,
            bar: cfg[o + 4],
            offset: u32_at(o + 8),
            length: u32_at(o + 12),
            notify_off_multiplier,
        };
        // The first capability of each type is the one that counts.
        if out[cfg_type as usize].is_none() {
            out[cfg_type as usize] = Some(s);
        }
    }
    out
}

/// Split virtqueue descriptor (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

/// Layout of a split virtqueue of `size` entries in contiguous memory:
/// descriptor table, driver ring (avail) and device ring
/// (used), each aligned as the specification requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLayout {
    pub size: u16,
    pub desc_offset: usize,
    pub avail_offset: usize,
    pub used_offset: usize,
    pub total_len: usize,
}

impl QueueLayout {
    pub fn new(size: u16) -> Self {
        let n = size as usize;
        let desc_offset = 0;
        let avail_offset = 16 * n; // aligned to 2 (already is)
        let avail_len = 6 + 2 * n;
        let used_offset = (avail_offset + avail_len).next_multiple_of(4096); // aligned to 4 (we use a page)
        let used_len = 6 + 8 * n;
        let total_len = (used_offset + used_len).next_multiple_of(4096);
        Self {
            size,
            desc_offset,
            avail_offset,
            used_offset,
            total_len,
        }
    }
}

/// Largest queue a `Virtqueue` handles: its private bookkeeping is sized
/// for it (8 bytes per entry, inside the driver's own structures: the
/// network stack's has a size bound, incident 0018). The drivers ask for at
/// most 64.
pub const MAX_QUEUE_SIZE: u16 = 64;

/// A used-ring element the device should not have written (audit
/// 2026-10-25 A, M1): the queue's state is no longer trustworthy and the
/// driver resets the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsedError {
    /// The id is not the head of a chain the device holds.
    NotInFlight(u32),
}

/// A split virtqueue living in a DMA buffer mapped at `virt` with
/// physical address `phys`. Single producer (the driver).
///
/// The device can write anything in its DMA memory, the descriptor table
/// included, so the queue keeps its own copy of what it needs to trust:
/// the free list's links and, for each chain the device holds, its length
/// and how many bytes it may write. A used id or length the device makes
/// up is refused instead of steering raw-pointer writes (audit 2026-10-25
/// A, M1: an id of 1000 in a 64-entry queue wrote past the queue).
pub struct Virtqueue {
    layout: QueueLayout,
    virt: *mut u8,
    phys: u64,
    free_head: u16,
    num_free: u16,
    avail_idx: u16,
    last_used: u16,
    /// Next free descriptor after each free one.
    next: [u16; MAX_QUEUE_SIZE as usize],
    /// Per head of a chain the device holds: its descriptors (0: not a
    /// held head) and its device-writable bytes.
    held: [u16; MAX_QUEUE_SIZE as usize],
    writable: [u32; MAX_QUEUE_SIZE as usize],
    /// Used elements whose length exceeded what their chain takes.
    overlong: u32,
}

impl Virtqueue {
    /// # Safety
    ///
    /// `virt` must point to `layout.total_len` writable, page-aligned
    /// bytes that mirror physical `phys`, exclusive to this queue.
    ///
    /// # Panics
    /// If `layout.size` exceeds `MAX_QUEUE_SIZE`.
    pub unsafe fn new(layout: QueueLayout, virt: *mut u8, phys: u64) -> Self {
        assert!(
            layout.size <= MAX_QUEUE_SIZE,
            "virtqueue larger than MAX_QUEUE_SIZE"
        );
        core::ptr::write_bytes(virt, 0, layout.total_len);
        let mut q = Self {
            layout,
            virt,
            phys,
            free_head: 0,
            num_free: layout.size,
            avail_idx: 0,
            last_used: 0,
            next: [0; MAX_QUEUE_SIZE as usize],
            held: [0; MAX_QUEUE_SIZE as usize],
            writable: [0; MAX_QUEUE_SIZE as usize],
            overlong: 0,
        };
        for i in 0..layout.size {
            q.next[i as usize] = i.wrapping_add(1);
        }
        q
    }

    pub fn size(&self) -> u16 {
        self.layout.size
    }
    pub fn desc_phys(&self) -> u64 {
        self.phys + self.layout.desc_offset as u64
    }
    pub fn avail_phys(&self) -> u64 {
        self.phys + self.layout.avail_offset as u64
    }
    pub fn used_phys(&self) -> u64 {
        self.phys + self.layout.used_offset as u64
    }

    fn desc_mut(&mut self, i: u16) -> &mut Descriptor {
        // SAFETY: inside the descriptor table, per the contract of `new`.
        unsafe {
            &mut *(self.virt.add(self.layout.desc_offset + 16 * i as usize) as *mut Descriptor)
        }
    }
    #[cfg(test)]
    fn desc(&self, i: u16) -> Descriptor {
        // SAFETY: same as above.
        unsafe {
            (self.virt.add(self.layout.desc_offset + 16 * i as usize) as *const Descriptor)
                .read_volatile()
        }
    }
    fn write_u16(&mut self, off: usize, v: u16) {
        // SAFETY: inside the queue buffer.
        unsafe { (self.virt.add(off) as *mut u16).write_volatile(v) }
    }
    fn read_u16(&self, off: usize) -> u16 {
        // SAFETY: same as above.
        unsafe { (self.virt.add(off) as *const u16).read_volatile() }
    }
    fn read_u32(&self, off: usize) -> u32 {
        // SAFETY: same as above.
        unsafe { (self.virt.add(off) as *const u32).read_volatile() }
    }

    /// Chains buffers `(phys, len, device-writable)` and publishes
    /// them in the avail ring. Returns the head descriptor, or `None` if
    /// there is no room.
    pub fn add(&mut self, buffers: &[(u64, u32, bool)]) -> Option<u16> {
        if buffers.is_empty() || (buffers.len() as u16) > self.num_free {
            return None;
        }
        let head = self.free_head;
        let mut i = head;
        let mut writable = 0u32;
        for (n, &(addr, len, write)) in buffers.iter().enumerate() {
            let next_free = self.next[i as usize];
            let last = n + 1 == buffers.len();
            let d = self.desc_mut(i);
            d.addr = addr;
            d.len = len;
            d.flags = if write { DESC_WRITE } else { 0 } | if last { 0 } else { DESC_NEXT };
            d.next = if last { 0 } else { next_free };
            if write {
                writable = writable.saturating_add(len);
            }
            i = next_free;
        }
        self.free_head = i;
        self.num_free -= buffers.len() as u16;
        self.held[head as usize] = buffers.len() as u16;
        self.writable[head as usize] = writable;
        let slot = self.avail_idx % self.layout.size;
        self.write_u16(self.layout.avail_offset + 4 + 2 * slot as usize, head);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.write_u16(self.layout.avail_offset + 2, self.avail_idx);
        fence(Ordering::SeqCst);
        Some(head)
    }

    /// Whether the device has returned a buffer not yet popped: the next
    /// `pop_used` would answer `Some`. Lets a driver know that frames are
    /// waiting without taking one (`Link::rx_pending`, incident 0013).
    pub fn has_used(&self) -> bool {
        fence(Ordering::SeqCst);
        self.read_u16(self.layout.used_offset + 2) != self.last_used
    }

    /// Next element consumed by the device: (head, bytes written).
    /// Returns the chain's descriptors to the free list, walking the
    /// queue's own links, never the device-writable table. `Err` if the
    /// device names a chain it does not hold: the driver resets the
    /// device. A length past what the chain takes is cut to it and counted
    /// (`overlong`): some devices report one wrongly, and it only sizes the
    /// driver's copy.
    pub fn pop_used(&mut self) -> Result<Option<(u16, u32)>, UsedError> {
        if !self.has_used() {
            return Ok(None);
        }
        let slot = self.last_used % self.layout.size;
        let base = self.layout.used_offset + 4 + 8 * slot as usize;
        let id = self.read_u32(base);
        let len = self.read_u32(base + 4);
        let head = match u16::try_from(id) {
            Ok(h) if h < self.layout.size && self.held[h as usize] != 0 => h,
            _ => return Err(UsedError::NotInFlight(id)),
        };
        let writable = self.writable[head as usize];
        let len = if len > writable {
            self.overlong = self.overlong.saturating_add(1);
            writable
        } else {
            len
        };
        self.last_used = self.last_used.wrapping_add(1);
        // The chain's descriptors in the order `add` linked them.
        let count = core::mem::take(&mut self.held[head as usize]);
        let mut tail = head;
        for _ in 1..count {
            tail = self.next[tail as usize];
        }
        self.next[tail as usize] = self.free_head;
        self.free_head = head;
        self.num_free += count;
        Ok(Some((head, len)))
    }

    /// Used elements so far whose length was cut to their chain's capacity.
    pub fn overlong(&self) -> u32 {
        self.overlong
    }

    pub fn num_free(&self) -> u16 {
        self.num_free
    }

    /// Descriptors the device still owns: published in the available ring
    /// and not returned through the used ring (virtio 1.2, 2.7, split
    /// virtqueues). Every buffer they point at may still be read or written
    /// by the device; without an IOMMU its
    /// memory must not be released until they come back or the device is
    /// reset (incident 0017).
    pub fn in_flight(&self) -> u16 {
        self.layout.size - self.num_free
    }
}

/// virtio-blk request header (16 bytes) followed by data and a status byte.
pub mod blk {
    pub const T_IN: u32 = 0;
    pub const T_OUT: u32 = 1;
    pub const T_FLUSH: u32 = 4;
    /// `VIRTIO_BLK_F_FLUSH`: the device handles `T_FLUSH`.
    pub const F_FLUSH: u64 = 1 << 9;
    pub const STATUS_OK: u8 = 0;
    pub const STATUS_IOERR: u8 = 1;
    pub const STATUS_UNSUPP: u8 = 2;
    pub const SECTOR_SIZE: u64 = 512;
    /// Offset of `capacity` (u64, sectors) in the device configuration.
    pub const CFG_CAPACITY: u64 = 0;

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct RequestHeader {
        pub kind: u32,
        pub reserved: u32,
        pub sector: u64,
    }

    impl RequestHeader {
        pub const fn read(sector: u64) -> Self {
            Self {
                kind: T_IN,
                reserved: 0,
                sector,
            }
        }
        pub const fn write(sector: u64) -> Self {
            Self {
                kind: T_OUT,
                reserved: 0,
                sector,
            }
        }
        /// Makes previous writes durable (no data).
        pub const fn flush() -> Self {
            Self {
                kind: T_FLUSH,
                reserved: 0,
                sector: 0,
            }
        }
        pub fn to_bytes(self) -> [u8; 16] {
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&self.kind.to_le_bytes());
            b[8..16].copy_from_slice(&self.sector.to_le_bytes());
            b
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};
    use std::vec;

    struct Buf {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Buf {
        fn new(len: usize) -> Self {
            let layout = Layout::from_size_align(len, 4096).unwrap();
            Self {
                ptr: unsafe { alloc_zeroed(layout) },
                layout,
            }
        }
    }
    impl Drop for Buf {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    #[test]
    fn layout_alignment() {
        let l = QueueLayout::new(8);
        assert_eq!(l.avail_offset, 128);
        assert_eq!(l.used_offset, 4096);
        assert_eq!(l.total_len, 8192);
        let l = QueueLayout::new(256);
        assert_eq!(l.avail_offset, 4096);
        assert_eq!(l.used_offset, 8192);
        assert!(l.total_len % 4096 == 0);
    }

    #[test]
    fn add_chain_and_pop_used_recycles_descriptors() {
        let l = QueueLayout::new(4);
        let b = Buf::new(l.total_len);
        let mut q = unsafe { Virtqueue::new(l, b.ptr, 0x10_0000) };
        assert_eq!(q.desc_phys(), 0x10_0000);
        assert_eq!(q.avail_phys(), 0x10_0000 + 64);
        let head = q
            .add(&[(0x1000, 16, false), (0x2000, 512, true), (0x3000, 1, true)])
            .unwrap();
        assert_eq!(head, 0);
        assert_eq!(q.num_free(), 1);
        assert_eq!(q.in_flight(), 3, "the device owns the whole chain");
        // Chained descriptors with correct flags.
        let d0 = q.desc(0);
        assert_eq!(
            (d0.addr, d0.len, d0.flags, d0.next),
            (0x1000, 16, DESC_NEXT, 1)
        );
        let d1 = q.desc(1);
        assert_eq!((d1.flags, d1.next), (DESC_NEXT | DESC_WRITE, 2));
        let d2 = q.desc(2);
        assert_eq!(d2.flags, DESC_WRITE);
        // avail: idx 1, ring[0] = 0
        assert_eq!(q.read_u16(l.avail_offset + 2), 1);
        assert_eq!(q.read_u16(l.avail_offset + 4), 0);
        // No room for a chain of 2.
        assert!(q.add(&[(0, 1, false), (0, 1, false)]).is_none());
        assert!(!q.has_used());
        assert_eq!(q.pop_used(), Ok(None));
        // The "device" consumes: used.ring[0] = {id 0, len 513}, used.idx = 1
        unsafe {
            (b.ptr.add(l.used_offset + 4) as *mut u32).write(0);
            (b.ptr.add(l.used_offset + 8) as *mut u32).write(513);
            (b.ptr.add(l.used_offset + 2) as *mut u16).write(1);
        }
        assert!(q.has_used(), "a used entry is visible before it is popped");
        assert_eq!(q.pop_used(), Ok(Some((0, 513))));
        assert_eq!(q.num_free(), 4, "the chain returns to the free list");
        assert_eq!(q.in_flight(), 0);
        assert!(!q.has_used());
        assert_eq!(q.pop_used(), Ok(None));
        // The whole queue can be used again.
        assert!(q
            .add(&[(0, 1, false), (0, 1, false), (0, 1, false), (0, 1, false)])
            .is_some());
        assert_eq!(q.num_free(), 0);
    }

    /// Writes used element `n` as {id, len} and publishes it.
    fn device_returns(b: &Buf, l: &QueueLayout, n: u16, id: u32, len: u32) {
        unsafe {
            (b.ptr.add(l.used_offset + 4 + 8 * n as usize) as *mut u32).write(id);
            (b.ptr.add(l.used_offset + 8 + 8 * n as usize) as *mut u32).write(len);
            (b.ptr.add(l.used_offset + 2) as *mut u16).write(n + 1);
        }
    }

    /// Audit 2026-10-25 A, M1: the used id indexed the descriptor table
    /// unchecked (an id past the queue wrote past it), a forged or repeated
    /// id corrupted the free list, and a cyclic chain hung the driver.
    #[test]
    fn device_made_up_used_elements_are_refused() {
        let l = QueueLayout::new(4);
        let b = Buf::new(l.total_len);
        let mut q = unsafe { Virtqueue::new(l, b.ptr, 0x10_0000) };
        let head = q.add(&[(0x1000, 16, false), (0x2000, 512, true)]).unwrap();
        device_returns(&b, &l, 0, 1000, 1);
        assert_eq!(q.pop_used(), Err(UsedError::NotInFlight(1000)));
        // Descriptor 1 is in the chain but no chain's head.
        device_returns(&b, &l, 0, 1, 1);
        assert_eq!(q.pop_used(), Err(UsedError::NotInFlight(1)));
        // The device rewrites the chain's links into a cycle: the queue
        // walks its own copy and terminates. Its length past the chain's
        // 512 writable bytes is cut to them.
        unsafe {
            let d1 = b.ptr.add(l.desc_offset + 16) as *mut Descriptor;
            (*d1).flags = DESC_NEXT;
            (*d1).next = 0;
        }
        device_returns(&b, &l, 0, head as u32, 513);
        assert_eq!(q.pop_used(), Ok(Some((head, 512))));
        assert_eq!(q.overlong(), 1);
        assert_eq!(q.num_free(), 4);
        // The same id again: no longer held.
        device_returns(&b, &l, 1, head as u32, 1);
        assert_eq!(q.pop_used(), Err(UsedError::NotInFlight(head as u32)));
    }

    #[test]
    fn finds_virtio_pci_structures() {
        let mut cfg = vec![0u8; 256];
        cfg[0..2].copy_from_slice(&0x1AF4u16.to_le_bytes());
        cfg[6] |= 0x10;
        cfg[0x34] = 0x40;
        // vendor cap: common in BAR4 offset 0 len 0x38
        cfg[0x40] = 0x09;
        cfg[0x41] = 0x50;
        cfg[0x42] = 16;
        cfg[0x43] = cfg_type::COMMON;
        cfg[0x44] = 4;
        cfg[0x4C..0x50].copy_from_slice(&0x38u32.to_le_bytes());
        // vendor cap: notify in BAR4 offset 0x3000, multiplier 4
        cfg[0x50] = 0x09;
        cfg[0x51] = 0x64;
        cfg[0x52] = 20;
        cfg[0x53] = cfg_type::NOTIFY;
        cfg[0x54] = 4;
        cfg[0x58..0x5C].copy_from_slice(&0x3000u32.to_le_bytes());
        cfg[0x5C..0x60].copy_from_slice(&0x1000u32.to_le_bytes());
        cfg[0x60..0x64].copy_from_slice(&4u32.to_le_bytes());
        // MSI-X cap (not vendor) at the end
        cfg[0x64] = 0x11;
        cfg[0x65] = 0;
        let s = find_structures(&cfg);
        assert_eq!(
            s[cfg_type::COMMON as usize],
            Some(PciStructure {
                cfg_type: 1,
                bar: 4,
                offset: 0,
                length: 0x38,
                notify_off_multiplier: 0
            })
        );
        let n = s[cfg_type::NOTIFY as usize].unwrap();
        assert_eq!((n.bar, n.offset, n.notify_off_multiplier), (4, 0x3000, 4));
        assert!(s[cfg_type::ISR as usize].is_none());
    }

    #[test]
    fn blk_request_header_bytes() {
        let h = blk::RequestHeader::read(7).to_bytes();
        assert_eq!(&h[0..4], &[0, 0, 0, 0]);
        assert_eq!(&h[8..16], &7u64.to_le_bytes());
        assert_eq!(blk::RequestHeader::write(1).kind, blk::T_OUT);
    }
}
