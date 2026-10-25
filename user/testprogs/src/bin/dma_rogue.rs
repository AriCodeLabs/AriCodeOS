// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! IOMMU test driver (Phase 33): receives the virtio-blk with the
//! `virtio_pci` contract (SERVICE = SYSTEM handle with READ), reads sector 0
//! into its own DMA buffer (translated by the unit) and then asks the device
//! to write to a physical address it does not own: the request must not
//! complete and the unit must have recorded the fault.
#![no_std]
#![no_main]

use aricode_rt::fmt_u64;
use aricode_rt::sys::virtio_pci::{DmaRegion, Queue, Transport, IRQ, SERVICE};
use aricode_rt::sys::{self, abi, Error};
use aricode_virtio::blk;

/// Reserved low memory: never belongs to this process or anyone else.
const FOREIGN_PHYS: u64 = 0x1000;

fn submit(
    transport: &Transport,
    queue: &mut Queue,
    req: &mut DmaRegion,
    data_phys: u64,
    timeout_ms: u64,
) -> Result<bool, u64> {
    req.slice_mut(0, 16)
        .copy_from_slice(&blk::RequestHeader::read(0).to_bytes());
    req.slice_mut(528, 1)[0] = 0xFF;
    let head = queue
        .vq
        .add(&[
            (req.phys, 16, false),
            (data_phys, 512, true),
            (req.phys + 528, 1, true),
        ])
        .ok_or(40u64)?;
    queue.notify(transport);
    loop {
        match queue.vq.pop_used() {
            Ok(Some((done, _))) => return if done == head { Ok(true) } else { Err(41) },
            Ok(None) => {}
            Err(_) => return Err(44),
        }
        match sys::irq_wait(IRQ, timeout_ms) {
            Ok(_) => {}
            Err(Error::Timeout) => return Ok(false),
            Err(_) => return Err(43),
        }
    }
}

fn faults() -> Result<u64, u64> {
    let mut buf = [0u8; 24];
    if sys::system_info(SERVICE, abi::sysinfo::IOMMU, &mut buf) != Ok(24) {
        return Err(50);
    }
    Ok(u64::from_le_bytes(buf[16..24].try_into().unwrap()))
}

fn main(mask: u64) -> u64 {
    let transport = match Transport::open(mask, 0) {
        Ok(t) => t,
        Err(e) => return e.code(),
    };
    let mut queue = match transport.setup_queue(0, 8) {
        Ok(q) => q,
        Err(e) => return e.code(),
    };
    transport.driver_ok();
    let Ok(mut req) = DmaRegion::alloc(4096) else {
        return 30;
    };
    let Ok(before) = faults() else {
        return 50;
    };

    // 1. Legitimate read: sector 0 (the GPT protective MBR) into our own buffer.
    let own_data = req.phys + 16;
    match submit(&transport, &mut queue, &mut req, own_data, 2000) {
        Ok(true) => {}
        Ok(false) => return 20,
        Err(e) => return e,
    }
    if req.slice_mut(528, 1)[0] != blk::STATUS_OK {
        return 21;
    }
    let sector = req.slice_mut(16, 512);
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return 22;
    }

    // 2. DMA to foreign memory. Whatever the device reports about the request
    // (not completing it, an I/O error or even "success", as QEMU does when
    // it drops the write) proves nothing: the proof is that the unit recorded
    // the fault and, on the kernel side, that the memory did not change.
    match submit(&transport, &mut queue, &mut req, FOREIGN_PHYS, 500) {
        Ok(false) => sys::print(b"dma_rogue: the illegal request did not complete\n"),
        Ok(true) if req.slice_mut(528, 1)[0] != blk::STATUS_OK => {
            sys::print(b"dma_rogue: the device reported an I/O error\n")
        }
        Ok(true) => sys::print(b"dma_rogue: the device reported the request as completed\n"),
        Err(e) => return e,
    }
    let Ok(after) = faults() else {
        return 50;
    };
    if after <= before {
        sys::print(b"dma_rogue: the unit did not record the fault\n");
        return 24;
    }
    let mut b = [0u8; 20];
    sys::print(b"dma_rogue: legitimate read ok, DMA to foreign memory blocked (");
    sys::print(fmt_u64(after - before, &mut b));
    sys::print(b" fault(s) recorded)\n");
    0
}

aricode_rt::entry!(main);
