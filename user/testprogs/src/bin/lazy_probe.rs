// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Zero-fill pages backed on first touch (performance audit 731c398,
//! finding D). The kernel writes into a page of a static buffer the program
//! never touched (a syscall's output), the heap is used, and the program's
//! own `sysinfo::ON_DEMAND` entry must show less backed than reserved.
//! Launched with `run lazy_probe` and the manifest `caps = system`:
//! handle 0x10004 = SYSTEM. Each failing step returns its number;
//! success = 0.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_rt::fmt_u64;
use aricode_rt::sys::{self, abi, Handle};

const SYSTEM: Handle = Handle(0x0001_0004);

/// 96 KiB of `.bss`, page aligned, never written by the program itself.
#[repr(align(4096))]
struct Untouched([u8; 96 * 1024]);
static mut BUF: Untouched = Untouched([0; 96 * 1024]);

fn main(_arg: u64) -> u64 {
    // The kernel copies into a page nobody touched: it must back it first.
    // SAFETY: single-threaded; the only reference to BUF.
    let untouched = unsafe { &mut *core::ptr::addr_of_mut!(BUF) };
    let out = &mut untouched.0[40 * 1024..40 * 1024 + 64];
    if sys::system_info(SYSTEM, abi::sysinfo::RANDOM, out) != Ok(64) {
        return 1;
    }
    // 128 KiB of heap, touched page by page.
    let mut heap = alloc::vec![0u8; 128 * 1024];
    for i in (0..heap.len()).step_by(4096) {
        heap[i] = i as u8;
    }
    let Ok(me) = sys::object_id(Handle::SELF_PROCESS) else {
        return 2;
    };
    let mut buf = [0u8; abi::sysinfo::StackEntry::LEN * 32];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::ON_DEMAND, &mut buf) else {
        return 3;
    };
    let Some(e) = buf[..n]
        .as_chunks::<{ abi::sysinfo::StackEntry::LEN }>()
        .0
        .iter()
        .filter_map(|c| abi::sysinfo::StackEntry::decode(c))
        .find(|e| e.pid as u64 == me)
    else {
        return 4;
    };
    let mut b = [0u8; 20];
    sys::print(b"lazy probe: ");
    sys::print(fmt_u64(e.touched as u64 / 1024, &mut b));
    sys::print(b" KiB backed of ");
    sys::print(fmt_u64(e.size as u64 / 1024, &mut b));
    sys::print(b" KiB reserved\n");
    // Reserved: BUF and the 256 KiB arena (less the page it may share with
    // the end of `.data`); backed: the heap used and one page of BUF,
    // never all of it.
    if e.size < (96 + 256 - 4) * 1024 || e.touched < 128 * 1024 || e.touched >= e.size {
        return 5;
    }
    core::hint::black_box(&heap);
    0
}

aricode_rt::entry!(main);
