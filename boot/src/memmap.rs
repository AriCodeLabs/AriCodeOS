// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Snapshot of the UEFI memory map using direct boot services calls.
//!
//! `uefi::boot::memory_map` chains size query, allocation and read with
//! `assert!`s and no traces; on real hardware we need to know which call the
//! firmware stalls in (docs/hardware.md), so each one is logged before and
//! after. Descriptors are walked with the `boot-protocol` iterator, the same
//! one the kernel uses.

use alloc::vec::Vec;
use boot_protocol::{MemoryDescriptor, MemoryMap};
use log::info;
use uefi::{table, Status};

/// Slack for the allocations the loader makes after reading the map
/// (the same one Linux uses).
const EXTRA_DESCRIPTORS: usize = 8;

pub struct Snapshot {
    /// Buffer holding the descriptors; `map.descriptors` points into it.
    _buf: Vec<u8>,
    pub map: MemoryMap,
}

impl Snapshot {
    pub fn regions(&self) -> impl Iterator<Item = MemoryDescriptor> + '_ {
        // SAFETY: `_buf` lives as long as `self` and is identity-mapped (boot
        // services active); it is not modified after construction.
        unsafe { self.map.iter(0) }
    }
}

/// Reads the current memory map. Fails if the firmware answers outside the
/// specification (impossible size or descriptor).
pub fn snapshot() -> Result<Snapshot, &'static str> {
    let st = table::system_table_raw().ok_or("no system table")?;
    // SAFETY: boot services active (before ExitBootServices) and table
    // registered by `uefi::helpers::init`.
    let bs = unsafe { &*(*st.as_ptr()).boot_services };
    let (mut size, mut key, mut desc_size, mut desc_version) = (0usize, 0usize, 0usize, 0u32);
    info!("memory: GetMemoryMap (size)");
    // SAFETY: boot services call with pointers to valid locals; with size 0
    // the firmware only fills in the sizes.
    let status = unsafe {
        (bs.get_memory_map)(
            &mut size,
            core::ptr::null_mut(),
            &mut key,
            &mut desc_size,
            &mut desc_version,
        )
    };
    info!(
        "memory: GetMemoryMap -> {status:?}, {size} bytes, descriptor size {desc_size} (version {desc_version})"
    );
    if status != Status::BUFFER_TOO_SMALL
        || size == 0
        || desc_size < core::mem::size_of::<MemoryDescriptor>()
    {
        return Err("GetMemoryMap: invalid response");
    }
    let len = size + EXTRA_DESCRIPTORS * desc_size;
    let mut buf = alloc::vec![0u8; len];
    size = len;
    // SAFETY: `buf` has `size` writable bytes; the rest are locals.
    let status = unsafe {
        (bs.get_memory_map)(
            &mut size,
            buf.as_mut_ptr().cast(),
            &mut key,
            &mut desc_size,
            &mut desc_version,
        )
    };
    info!("memory: GetMemoryMap({len}) -> {status:?}, {size} bytes");
    if status != Status::SUCCESS || size > len || desc_size == 0 || size % desc_size != 0 {
        return Err("GetMemoryMap: invalid response");
    }
    let map = MemoryMap {
        descriptors: buf.as_ptr() as u64,
        descriptor_size: desc_size as u32,
        descriptor_count: (size / desc_size) as u32,
    };
    Ok(Snapshot { _buf: buf, map })
}
