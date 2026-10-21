// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Driver that dies holding a claimed PCI function (`test=pci_reap`,
//! incident 0004, H2): it turns bus mastering on as a driver does after
//! resetting its device, allocates a DMA buffer, keeps it only through its
//! mapping (as a driver with a descriptor ring would) and faults. The
//! kernel must stop the device's bus mastering before that buffer's frames
//! go back to the PMM.
//! Handles: 0x10004 = the function's configuration (claim), 0x10005 =
//! HARDWARE (MANAGE). Exit codes (only if something fails before the
//! fault): 1 = no DMA, 2 = no mapping, 3 = handle_close failed, 4 = bus
//! mastering could not be enabled.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};

const CFG: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);

fn main(_arg: u64) -> u64 {
    if sys::pci::enable_bus_master(CFG).is_err() {
        return 4;
    }
    let Ok(dma) = sys::dma_alloc(HW, 16 * 1024) else {
        return 1;
    };
    let Ok(virt) = sys::mmio_map(dma) else {
        return 2;
    };
    if sys::handle_close(dma).is_err() {
        return 3;
    }
    // SAFETY: the buffer is mapped RW in this process.
    unsafe { (virt as *mut u64).write_volatile(0x0004_0004) };
    // SAFETY: deliberate fault; page 0 is never mapped and the address is
    // hidden from the compiler so that it emits the read.
    let bad = core::hint::black_box(0x40u64) as *const u64;
    unsafe { bad.read_volatile() }
}

aricode_rt::entry!(main);
