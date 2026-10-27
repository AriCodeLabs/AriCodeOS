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
//! kept their charge).
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
    match sys::dma_alloc(HW, 4096) {
        Ok(_) => {
            sys::print(b"dma_refund: failed requests gave their quota back ok\n");
            0
        }
        Err(e) => {
            aricode_rt::print_error(b"dma_refund: the request within the quota", e);
            4
        }
    }
}

aricode_rt::entry!(main);
