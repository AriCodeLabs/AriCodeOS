// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Virtio transport over modern PCI for ring 3 drivers: locates the
//! structures, negotiates features, creates queues in DMA memory and offers
//! MMIO access. Shared by virtio-blk and virtio-net; each driver provides
//! only its device logic.
//!
//! Contract with init (`start_driver`): handles `[service, cfg, hardware,
//! IRQ, present BARs..., driver extras...]`; `arg` is the mask with
//! bit 15 = IRQ present and bits 0..5 = BAR i present.

use crate::{self as sys, Error, Handle};
use aricode_virtio::{
    cfg_type, common, find_structures, status, QueueLayout, Virtqueue, FEATURE_ACCESS_PLATFORM,
    FEATURE_VERSION_1,
};

/// Fixed handles of the contract with init.
pub const SERVICE: Handle = Handle(0x0001_0004);
pub const CFG: Handle = Handle(0x0001_0005);
pub const HW: Handle = Handle(0x0001_0006);
pub const IRQ: Handle = Handle(0x0001_0007);
pub const FIRST_BAR_HANDLE: u32 = 0x0001_0008;

/// Handle of BAR `bar` according to the mask, if present.
pub fn bar_handle(mask: u64, bar: u8) -> Option<Handle> {
    if mask & (1 << bar) == 0 || mask & (1 << 15) == 0 {
        return None;
    }
    let before = (mask & ((1u64 << bar) - 1)).count_ones();
    Some(Handle(FIRST_BAR_HANDLE + before))
}

/// First handle after the BARs: driver-specific extras, in the order
/// init passed them.
pub fn extra_handle(mask: u64, index: u32) -> Handle {
    Handle(FIRST_BAR_HANDLE + (mask & 0x3F).count_ones() + index)
}

/// MMIO registers of a mapped virtio structure.
#[derive(Clone, Copy)]
pub struct Mmio(pub u64);

impl Mmio {
    pub fn r8(&self, off: u64) -> u8 {
        // SAFETY: within the region mapped by `mmio_map`.
        unsafe { ((self.0 + off) as *const u8).read_volatile() }
    }
    pub fn r16(&self, off: u64) -> u16 {
        // SAFETY: same as above.
        unsafe { ((self.0 + off) as *const u16).read_volatile() }
    }
    pub fn r32(&self, off: u64) -> u32 {
        // SAFETY: same as above.
        unsafe { ((self.0 + off) as *const u32).read_volatile() }
    }
    pub fn r64(&self, off: u64) -> u64 {
        self.r32(off) as u64 | (self.r32(off + 4) as u64) << 32
    }
    pub fn w8(&self, off: u64, v: u8) {
        // SAFETY: same as above.
        unsafe { ((self.0 + off) as *mut u8).write_volatile(v) }
    }
    pub fn w16(&self, off: u64, v: u16) {
        // SAFETY: same as above.
        unsafe { ((self.0 + off) as *mut u16).write_volatile(v) }
    }
    pub fn w32(&self, off: u64, v: u32) {
        // SAFETY: same as above.
        unsafe { ((self.0 + off) as *mut u32).write_volatile(v) }
    }
    pub fn w64(&self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

/// The driver's own DMA buffer, mapped in its address space.
#[derive(Clone, Copy)]
pub struct DmaRegion {
    pub virt: u64,
    pub phys: u64,
    pub len: u64,
}

impl DmaRegion {
    pub fn alloc(len: u64) -> Result<Self, Error> {
        let h = sys::dma_alloc(HW, len)?;
        let virt = sys::mmio_map(h)?;
        let phys = sys::dma_phys(h)?;
        Ok(Self { virt, phys, len })
    }

    /// Mutable view of `[off, off + len)`.
    pub fn slice_mut(&mut self, off: u64, len: u64) -> &mut [u8] {
        assert!(off + len <= self.len, "outside the DMA buffer");
        // SAFETY: DMA memory mapped RW and exclusive to the driver.
        unsafe { core::slice::from_raw_parts_mut((self.virt + off) as *mut u8, len as usize) }
    }
}

/// Initialization steps that can fail, for exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupError {
    MapConfig,
    NoCommon,
    NoNotify,
    NoDevice,
    MapBar,
    NoVersion1,
    FeaturesRejected,
    QueueMissing,
    QueueDma,
    MsixRejected,
}

impl SetupError {
    /// Stable exit code for the scenarios.
    pub fn code(self) -> u64 {
        10 + self as u64
    }
}

pub struct Transport {
    pub common: Mmio,
    pub notify: Mmio,
    pub device_cfg: Mmio,
    notify_multiplier: u64,
    /// Negotiated features.
    pub features: u64,
}

pub struct Queue {
    pub vq: Virtqueue,
    notify_addr: u64,
}

impl Queue {
    /// Notifies the device that there are new descriptors.
    pub fn notify(&self, transport: &Transport) {
        transport.notify.w16(self.notify_addr, 0);
    }
}

impl Transport {
    /// Maps the structures and negotiates `VERSION_1 | wanted & offered`.
    /// Leaves the device in `FEATURES_OK`; queues and `driver_ok` remain to be done.
    pub fn open(mask: u64, wanted: u64) -> Result<Self, SetupError> {
        let cfg_virt = sys::mmio_map(CFG).map_err(|_| SetupError::MapConfig)?;
        let mut cfg = [0u8; 256];
        for (i, b) in cfg.iter_mut().enumerate() {
            // SAFETY: mapped configuration page.
            *b = unsafe { ((cfg_virt + i as u64) as *const u8).read_volatile() };
        }
        let structures = find_structures(&cfg);
        let common_s = structures[cfg_type::COMMON as usize].ok_or(SetupError::NoCommon)?;
        let notify_s = structures[cfg_type::NOTIFY as usize].ok_or(SetupError::NoNotify)?;
        let device_s = structures[cfg_type::DEVICE as usize].ok_or(SetupError::NoDevice)?;
        let mut mapped: [Option<(u8, u64)>; 3] = [None; 3];
        let mut bar_virt = |bar: u8| -> Result<u64, SetupError> {
            if let Some((_, v)) = mapped.iter().flatten().find(|(b, _)| *b == bar) {
                return Ok(*v);
            }
            let h = bar_handle(mask, bar).ok_or(SetupError::MapBar)?;
            let v = sys::mmio_map(h).map_err(|_| SetupError::MapBar)?;
            if let Some(slot) = mapped.iter_mut().find(|m| m.is_none()) {
                *slot = Some((bar, v));
            }
            Ok(v)
        };
        let common = Mmio(bar_virt(common_s.bar)? + common_s.offset as u64);
        let notify = Mmio(bar_virt(notify_s.bar)? + notify_s.offset as u64);
        let device_cfg = Mmio(bar_virt(device_s.bar)? + device_s.offset as u64);

        // Negotiation (virtio 1.x, section 3.1.1).
        common.w8(common::DEVICE_STATUS, 0);
        while common.r8(common::DEVICE_STATUS) != 0 {}
        common.w8(common::DEVICE_STATUS, status::ACKNOWLEDGE);
        common.w8(common::DEVICE_STATUS, status::ACKNOWLEDGE | status::DRIVER);
        common.w32(common::DEVICE_FEATURE_SELECT, 0);
        let f_lo = common.r32(common::DEVICE_FEATURE) as u64;
        common.w32(common::DEVICE_FEATURE_SELECT, 1);
        let offered = f_lo | (common.r32(common::DEVICE_FEATURE) as u64) << 32;
        if offered & FEATURE_VERSION_1 == 0 {
            return Err(SetupError::NoVersion1);
        }
        // ACCESS_PLATFORM belongs to the transport: if the device offers it (DMA
        // through the IOMMU) it must be accepted or the device rejects FEATURES_OK.
        let features = FEATURE_VERSION_1 | (wanted & offered) | (offered & FEATURE_ACCESS_PLATFORM);
        common.w32(common::DRIVER_FEATURE_SELECT, 0);
        common.w32(common::DRIVER_FEATURE, features as u32);
        common.w32(common::DRIVER_FEATURE_SELECT, 1);
        common.w32(common::DRIVER_FEATURE, (features >> 32) as u32);
        common.w8(
            common::DEVICE_STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
        );
        if common.r8(common::DEVICE_STATUS) & status::FEATURES_OK == 0 {
            return Err(SetupError::FeaturesRejected);
        }
        common.w16(common::MSIX_CONFIG, 0xFFFF);
        Ok(Self {
            common,
            notify,
            device_cfg,
            notify_multiplier: notify_s.notify_off_multiplier as u64,
            features,
        })
    }

    /// Creates queue `index` (up to `max_size` entries) in DMA memory and
    /// binds it to MSI-X vector 0.
    pub fn setup_queue(&self, index: u16, max_size: u16) -> Result<Queue, SetupError> {
        let c = &self.common;
        c.w16(common::QUEUE_SELECT, index);
        let size = c.r16(common::QUEUE_SIZE).min(max_size);
        if size == 0 {
            return Err(SetupError::QueueMissing);
        }
        let layout = QueueLayout::new(size);
        let region = DmaRegion::alloc(layout.total_len as u64).map_err(|_| SetupError::QueueDma)?;
        // SAFETY: zeroed, page-aligned DMA buffer, exclusive to the queue.
        let vq = unsafe { Virtqueue::new(layout, region.virt as *mut u8, region.phys) };
        c.w16(common::QUEUE_SIZE, size);
        c.w64(common::QUEUE_DESC, vq.desc_phys());
        c.w64(common::QUEUE_DRIVER, vq.avail_phys());
        c.w64(common::QUEUE_DEVICE, vq.used_phys());
        c.w16(common::QUEUE_MSIX_VECTOR, 0);
        if c.r16(common::QUEUE_MSIX_VECTOR) != 0 {
            return Err(SetupError::MsixRejected);
        }
        let notify_addr = c.r16(common::QUEUE_NOTIFY_OFF) as u64 * self.notify_multiplier;
        c.w16(common::QUEUE_ENABLE, 1);
        Ok(Queue { vq, notify_addr })
    }

    /// Sets `DRIVER_OK`: the device starts operating.
    pub fn driver_ok(&self) {
        self.common.w8(
            common::DEVICE_STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
        );
    }

    pub fn needs_reset(&self) -> bool {
        self.common.r8(common::DEVICE_STATUS) & status::NEEDS_RESET != 0
    }
}
