// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test driver: handles IRQ0 (PIT) from ring 3 and allocates a DMA buffer.
//! Handles: 0x10004 = IRQ line (READ|WAIT|DUPLICATE), 0x10005 = HARDWARE
//! (MANAGE). Also waits on the line and an endpoint of its own at once
//! (`wait_any`, ADR 0044), with the checks of that call's arguments.
#![no_std]
#![no_main]

use aricode_rt::sys::abi::receive_any::Ready;
use aricode_rt::sys::{self, abi, rights, Error, Handle};

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
    wait_set()
}

/// The line and an endpoint in one wait (ADR 0044): the PIT's interrupt
/// (100 Hz) ends a wait whose endpoint stays empty; a queued message wins
/// when its endpoint is listed first; and the call checks its members
/// before waiting: a line without `WAIT`, an object that is neither an
/// endpoint nor a line, and more lines than `IPC_MAX_ANY_IRQS`.
fn wait_set() -> u64 {
    let Ok(ep) = sys::endpoint_create() else {
        return 20;
    };
    let mut buf = [0u8; 8];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    match sys::wait_any(&[ep, IRQ], &mut buf, &mut handles, 2000) {
        Ok(Ready::Irq { index: 1, count }) if count >= 1 => {}
        _ => return 21,
    }
    if sys::ipc_send(ep, b"ping", &[], 0).is_err() {
        return 22;
    }
    match sys::wait_any(&[ep, IRQ], &mut buf, &mut handles, 2000) {
        Ok(Ready::Message {
            index: 0,
            len: 4,
            handles: 0,
        }) if &buf[..4] == b"ping" => {}
        _ => return 23,
    }
    let Ok(no_wait) = sys::handle_duplicate(IRQ, rights::READ) else {
        return 24;
    };
    if sys::wait_any(&[ep, no_wait], &mut buf, &mut handles, 2000) != Err(Error::Denied) {
        return 25;
    }
    if sys::wait_any(&[ep, HW], &mut buf, &mut handles, 2000) != Err(Error::Invalid) {
        return 26;
    }
    let too_many = [IRQ; abi::IPC_MAX_ANY_IRQS + 1];
    if sys::wait_any(&too_many, &mut buf, &mut handles, 2000) != Err(Error::Invalid) {
        return 27;
    }
    sys::print(b"pit_driver: wait on the IRQ line and an endpoint ok\n");
    0
}

aricode_rt::entry!(main);
