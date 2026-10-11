//! Test driver: paints the framebuffer through an MMIO capability.
//! Handle 0x10004 = framebuffer MMIO region (READ|WRITE); arg = stride.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};

const FB: Handle = Handle(0x0001_0004);
const COLOR: u32 = 0x00FF_8800;

fn main(stride: u64) -> u64 {
    let Ok(len) = sys::region_len(FB) else {
        return 1;
    };
    let Ok(virt) = sys::mmio_map(FB) else {
        return 2;
    };
    let pixels = virt as *mut u32;
    for y in 0..64u64 {
        for x in 0..64u64 {
            let idx = y * stride + x;
            if (idx + 1) * 4 > len {
                return 3;
            }
            // SAFETY: inside the mapped region, checked against `len`.
            unsafe { pixels.add(idx as usize).write_volatile(COLOR) };
        }
    }
    0
}

aricode_rt::entry!(main);
