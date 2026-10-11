//! Test driver: handles IRQ0 (PIT) from ring 3 and allocates a DMA buffer.
//! Handles: 0x10004 = IRQ line (READ|WAIT), 0x10005 = HARDWARE (MANAGE).
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Error, Handle};

const IRQ: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);

fn main(_arg: u64) -> u64 {
    let mut total = 0;
    for _ in 0..5 {
        match sys::irq_wait(IRQ, 2000) {
            Ok(n) => total += n,
            Err(_) => return 1,
        }
    }
    if total < 5 {
        return 2;
    }
    let Ok(dma) = sys::dma_alloc(HW, 16 * 1024) else {
        return 3;
    };
    if sys::region_len(dma) != Ok(16 * 1024) {
        return 4;
    }
    let Ok(virt) = sys::mmio_map(dma) else {
        return 5;
    };
    let buf = virt as *mut u32;
    for i in 0..4096u32 {
        // SAFETY: 16 KiB DMA buffer mapped RW in this process.
        unsafe {
            buf.add(i as usize)
                .write_volatile(i.wrapping_mul(0x9E37_79B9))
        };
    }
    for i in 0..4096u32 {
        // SAFETY: same as above.
        if unsafe { buf.add(i as usize).read_volatile() } != i.wrapping_mul(0x9E37_79B9) {
            return 6;
        }
    }
    match sys::dma_phys(dma) {
        Ok(p) if p != 0 && p % 4096 == 0 => {}
        _ => return 7,
    }
    // No DMA without MANAGE over HARDWARE: the IRQ line does not qualify.
    if sys::dma_alloc(IRQ, 4096) != Err(Error::Denied) {
        return 8;
    }
    0
}

aricode_rt::entry!(main);
