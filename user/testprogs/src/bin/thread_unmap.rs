// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! TLB shootdown for a process's memory (`test=thread_unmap`, ADR 0082): a second
//! thread writes a DMA page without pause, no syscall in between, so it keeps the
//! page's translation on its CPU; the main thread unmaps the page. The writer's
//! next write must fault, which kills the process (`#PF write`). Should it go on
//! writing, its CPU kept a stale entry into freed memory: the main thread sees
//! its count still rising and exits with 4.
//! Handles: 0x10004 = DMA buffer (READ|WRITE).
//! Exit codes: 1 = `mmio_map` refused, 2 = no second thread, 3 = `mmio_unmap`
//! failed, 4 = the writer wrote after the unmap.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};
use aricode_rt::thread;
use core::sync::atomic::{AtomicU64, Ordering};

const DMA: Handle = Handle(0x0001_0004);
/// Writes the second thread has made.
static WRITES: AtomicU64 = AtomicU64::new(0);

fn main(_arg: u64) -> u64 {
    let Ok(virt) = sys::mmio_map(DMA) else {
        return 1;
    };
    let writer = thread::spawn(move || {
        let page = virt as *mut u64;
        let mut n = 0u64;
        loop {
            n += 1;
            // SAFETY: mapped RW until the main thread unmaps it; after that this
            // write must fault.
            unsafe { page.write_volatile(n) };
            WRITES.store(n, Ordering::Relaxed);
        }
    });
    if writer.is_err() {
        return 2;
    }
    while WRITES.load(Ordering::Relaxed) < 100_000 {
        sys::thread_yield();
    }
    if sys::mmio_unmap(virt).is_err() {
        return 3;
    }
    // The process should be gone before this ends.
    sys::thread_sleep(200);
    let before = WRITES.load(Ordering::Relaxed);
    sys::thread_sleep(100);
    if WRITES.load(Ordering::Relaxed) != before {
        sys::print(b"thread_unmap: the writer still writes after the unmap\n");
        return 4;
    }
    0
}

aricode_rt::entry!(main);
