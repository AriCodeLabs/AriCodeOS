// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! DMA lifetime test server (`test=dma_lifetime`, incident 0004, ADR 0038):
//! behaves like the compositor with a window buffer. It maps the DMA buffer
//! it was given, closes its handle and writes `MAGIC` into the first word
//! so the kernel knows the mapping exists and the handle is gone. With
//! argument 0 it then keeps the mapping until it is killed; with 1 it waits
//! until the kernel writes `UNMAP` into the second word, unmaps the buffer
//! (`mmio_unmap`) and sleeps until it is killed.
//! Handles: 0x10004 = DMA buffer (READ|WRITE).
//! Exit codes: 1 = `mmio_map` refused, 2 = `handle_close` failed,
//! 3 = `mmio_unmap` failed.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};

const DMA: Handle = Handle(0x0001_0004);
/// Must match `DMA_HOLDER_MAGIC` and `DMA_HOLDER_UNMAP` in
/// `kernel/src/user/process.rs`.
const MAGIC: u64 = 0xD3A0_11FE_71E0_0004;
const UNMAP: u64 = 0x0A3A_9000_0000_0038;

fn main(arg: u64) -> u64 {
    let Ok(virt) = sys::mmio_map(DMA) else {
        return 1;
    };
    if sys::handle_close(DMA).is_err() {
        return 2;
    }
    let word = virt as *mut u64;
    // SAFETY: the buffer is mapped RW in this process (at least one page).
    unsafe { word.write_volatile(MAGIC) };
    if arg == 1 {
        // SAFETY: same mapping, second word.
        while unsafe { word.add(1).read_volatile() } != UNMAP {
            sys::thread_sleep(10);
        }
        if sys::mmio_unmap(virt).is_err() {
            return 3;
        }
    }
    loop {
        sys::thread_sleep(1000);
    }
}

aricode_rt::entry!(main);
