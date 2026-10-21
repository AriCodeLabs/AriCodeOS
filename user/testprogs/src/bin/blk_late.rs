// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Incident 0017: a client buffer whose request timed out while the device
//! still owned it must not be released under the device. Run by init with
//! `blk_fault=LBA` (the virtio-blk driver does not notify the device of the
//! first request covering `LBA`, so the request times out for real and the
//! device completes it on the next notification). Handles: 0x10004 =
//! service endpoint (WRITE), 0x10005 = HARDWARE. `arg` = the LBA (in the
//! test disk's raw area, where every sector starts with `ARIDISK\0`).
//!
//! 1. Read `LBA` into buffer A: the request times out (`Io`).
//! 2. Close A, then make the server map eight other buffers (requests it
//!    refuses as too small, so the device is not touched): its client
//!    buffer cache (8 entries) evicts A, and A's frames go back to the PMM
//!    unless someone still holds A.
//! 3. Allocate fresh buffers (the PMM hands out the lowest free frames, so
//!    A's frames come back here if they were freed) and fill them.
//! 4. A valid request notifies the device, which also processes whatever
//!    chain is still in its ring.
//! 5. No fresh buffer may contain disk data (a late device write into
//!    reused memory), and the valid request must succeed.
//!
//! Exit 0 on success; 20 if a late device write landed in a fresh buffer;
//! other codes for setup failures.
#![no_std]
#![no_main]

use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};
use aricode_rt::sys::{self, rights, Handle, TIMEOUT_INFINITE};

const BLK: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const PAGE: u64 = 4096;
/// Buffers that push A out of the server's cache (`ClientBuffers<8>`).
const EVICTORS: usize = 8;
/// Fresh buffers allocated after the eviction.
const FRESH: usize = 8;
/// Reply wait: the driver's request timeout (2 s) plus its recovery.
const REPLY_WAIT_MS: u64 = 15_000;

fn say(msg: &[u8]) {
    sys::print(b"blk_late: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn hex(v: u64) {
    let mut buf = [0u8; 20];
    sys::print(b"0x");
    let mut n = 0;
    let mut x = v;
    loop {
        let d = (x & 0xF) as u8;
        buf[n] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        n += 1;
        x >>= 4;
        if x == 0 {
            break;
        }
    }
    buf[..n].reverse();
    sys::print(&buf[..n]);
}

/// One request with `dma` as its buffer; the reply status.
fn call(reply: Handle, op: Op, lba: u64, count: u16, dma: Handle) -> Option<Status> {
    let reply_w = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER).ok()?;
    let buf = sys::handle_duplicate(dma, rights::READ | rights::WRITE | rights::TRANSFER).ok()?;
    let req = Request { op, count, lba };
    sys::ipc_send(BLK, &req.encode(), &[reply_w, buf], TIMEOUT_INFINITE).ok()?;
    let mut msg = [0u8; 32];
    let mut none: [Handle; 0] = [];
    let (len, _) = sys::ipc_receive(reply, &mut msg, &mut none, REPLY_WAIT_MS).ok()?;
    Reply::decode(&msg[..len]).map(|r| r.status)
}

/// Fills the whole buffer `dma` with `byte` through a short-lived mapping
/// (unmapped again, so this process keeps no hold on it but the handle).
fn fill(dma: Handle, byte: u8) -> Option<()> {
    let virt = sys::mmio_map(dma).ok()?;
    // SAFETY: a PAGE-long DMA buffer mapped RW here until the unmap below.
    unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, PAGE as usize) }.fill(byte);
    sys::mmio_unmap(virt).ok()
}

fn main(lba: u64) -> u64 {
    let Ok(reply) = sys::endpoint_create() else {
        return 1;
    };
    let alloc = || sys::dma_alloc(HW, PAGE).ok();
    let mut evictors = [Handle(0); EVICTORS];
    for e in evictors.iter_mut() {
        let Some(h) = alloc() else {
            return 2;
        };
        *e = h;
    }
    let Some(a) = alloc() else {
        return 2;
    };
    let Ok(a_phys) = sys::dma_phys(a) else {
        return 3;
    };
    if fill(a, 0xAA).is_none() {
        return 3;
    }
    // 1. The injected request: the device is never told about it.
    sys::print(b"blk_late: reading LBA ");
    sys::print(aricode_rt::fmt_u64(lba, &mut [0u8; 20]));
    sys::print(b" into buffer A at ");
    hex(a_phys);
    sys::print(b" (the device is not notified)\n");
    match call(reply, Op::Read, lba, 1, a) {
        Some(Status::Io) => say(b"the request timed out (Io), as injected"),
        _ => return 4,
    }
    // 2. A is the server's alone now; eight new mappings evict it.
    let _ = sys::handle_close(a);
    for &e in &evictors {
        if call(reply, Op::Read, lba + 1, MAX_SECTORS, e) != Some(Status::BadHandles) {
            return 5;
        }
    }
    say(b"buffer A closed and pushed out of the server's mapping cache");
    // 3. Fresh buffers, possibly on A's frames.
    let mut fresh = [Handle(0); FRESH];
    let mut reused = false;
    for f in fresh.iter_mut() {
        let Some(h) = alloc() else {
            return 6;
        };
        if fill(h, 0x55).is_none() {
            return 6;
        }
        reused |= sys::dma_phys(h).ok() == Some(a_phys);
        *f = h;
    }
    if reused {
        say(b"a fresh buffer got the frame of buffer A");
    }
    // 4. A valid request: the device is notified again.
    let next = call(reply, Op::Read, lba + 1, 1, evictors[0]);
    // 5. Nothing may have written into the fresh buffers.
    for &f in &fresh {
        let Ok(virt) = sys::mmio_map(f) else {
            return 7;
        };
        // SAFETY: a PAGE-long DMA buffer mapped RW here.
        let bytes = unsafe { core::slice::from_raw_parts(virt as *const u8, PAGE as usize) };
        if bytes.iter().any(|&b| b != 0x55) {
            sys::print(b"blk_late: a late device write landed in a fresh buffer at ");
            hex(sys::dma_phys(f).unwrap_or(0));
            sys::print(if &bytes[..8] == b"ARIDISK\0" {
                b": disk data (incident 0017)\n".as_slice()
            } else {
                b": unexpected contents (incident 0017)\n".as_slice()
            });
            return 20;
        }
        let _ = sys::mmio_unmap(virt);
    }
    if next != Some(Status::Ok) {
        say(b"the request after the timeout failed: the ring still held the old chain");
        return 21;
    }
    say(b"no late write reached reused memory; the next request succeeded");
    0
}

aricode_rt::entry!(main);
