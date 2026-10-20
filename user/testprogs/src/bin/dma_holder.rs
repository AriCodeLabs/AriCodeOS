// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! DMA lifetime test server (`test=dma_lifetime`, incident 0004): behaves
//! like the compositor with a window buffer. It maps the DMA buffer it was
//! given, closes its handle, writes `MAGIC` into the first word so the
//! kernel knows the mapping exists and the handle is gone, and then keeps
//! the mapping until it is killed.
//! Handles: 0x10004 = DMA buffer (READ|WRITE).
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};

const DMA: Handle = Handle(0x0001_0004);
/// Must match `DMA_HOLDER_MAGIC` in `kernel/src/user/process.rs`.
const MAGIC: u64 = 0xD3A0_11FE_71E0_0004;

fn main(_arg: u64) -> u64 {
    let Ok(virt) = sys::mmio_map(DMA) else {
        return 1;
    };
    if sys::handle_close(DMA).is_err() {
        return 2;
    }
    // SAFETY: the buffer is mapped RW in this process (at least one page).
    unsafe { (virt as *mut u64).write_volatile(MAGIC) };
    loop {
        sys::thread_sleep(1000);
    }
}

aricode_rt::entry!(main);
