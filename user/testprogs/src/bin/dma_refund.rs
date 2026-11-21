// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! DMA quota refund test (`test=dma_lifetime`, incident 0047): started
//! with a quota of two pages, it fills its handle table and asks for
//! one-page buffers that cannot get a handle, so each request fails after
//! the kernel charged it. Then it frees one entry and asks again: the
//! failed requests must have given their charge back, so the page fits.
//! Handles: 0x10004 = HARDWARE (MANAGE). Exit codes: 1 = the table did not
//! fill, 2 = a request with a full table did not fail with `Limit`, 3 =
//! `handle_close` failed, 4 = the last request was refused (the failed ones
//! kept their charge); see `freed_buffers_give_back` for the rest.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Error, Handle};

const HW: Handle = Handle(0x0001_0004);
/// Entries in a process handle table (the kernel's `HANDLE_LIMIT`).
const TABLE: usize = 256;
/// Requests made with a full table: twice the quota if none is refunded.
const FAILED: usize = 4;

fn main(_arg: u64) -> u64 {
    let mut created = [Handle(0); TABLE];
    let mut count = 0;
    loop {
        match sys::endpoint_create() {
            Ok(h) if count < TABLE => {
                created[count] = h;
                count += 1;
            }
            Err(Error::Limit) if count > 0 => break,
            _ => return 1,
        }
    }
    for _ in 0..FAILED {
        if sys::dma_alloc(HW, 4096) != Err(Error::Limit) {
            return 2;
        }
    }
    if sys::handle_close(created[count - 1]).is_err() {
        return 3;
    }
    let Ok(first) = sys::dma_alloc(HW, 4096) else {
        return 4;
    };
    sys::print(b"dma_refund: failed requests gave their quota back ok\n");
    // Room in the table for what follows.
    for &h in &created[count - 4..count - 1] {
        if sys::handle_close(h).is_err() {
            return 3;
        }
    }
    freed_buffers_give_back(first)
}

/// The quota bounds live buffers, for as long as their frames live (ADR 0078,
/// stage 0). Exit codes: 5 = the second page was refused, 6 = a third was
/// granted, 7 = a page after a close was refused, 8 = mapping failed, 9 = a
/// page was granted while a mapping keeps a closed buffer's frames, 10 = a
/// page was refused after the unmap.
fn freed_buffers_give_back(first: Handle) -> u64 {
    if sys::dma_alloc(HW, 4096).is_err() {
        return 5;
    }
    if sys::dma_alloc(HW, 4096) != Err(Error::Limit) {
        return 6;
    }
    // Many times the quota, freed as it goes: more than a cumulative charge
    // could ever give.
    let mut held = first;
    for _ in 0..32 {
        if sys::handle_close(held).is_err() {
            return 3;
        }
        held = match sys::dma_alloc(HW, 4096) {
            Ok(h) => h,
            Err(_) => return 7,
        };
    }
    // A closed buffer whose frames a mapping keeps stays charged.
    let Ok(virt) = sys::mmio_map(held) else {
        return 8;
    };
    if sys::handle_close(held).is_err() {
        return 3;
    }
    if sys::dma_alloc(HW, 4096) != Err(Error::Limit) {
        return 9;
    }
    if sys::mmio_unmap(virt).is_err() {
        return 3;
    }
    if sys::dma_alloc(HW, 4096).is_err() {
        return 10;
    }
    sys::print(b"dma_refund: freed buffers gave their quota back ok\n");
    0
}

aricode_rt::entry!(main);
