// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Memory for big programs (ADR 0084). A 64 MiB memory object takes no
//! memory until touched, takes about what is touched, gives it back when
//! let go; the kernel backs a page a call writes into; mapping it twice gives
//! one mapping; the heap grows past the old 8 MiB; and the quota stops what
//! is beyond the share of the machine one application may hold. Launched
//! with `run mem_probe` and `caps = system` (handle 0x10004). Each failing
//! step returns its number; success = 0.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_rt::fmt_u64;
use aricode_rt::sys::{self, abi, Error, Handle};

const SYSTEM: Handle = Handle(0x0001_0004);
const MIB: u64 = 1024 * 1024;
const PAGE: u64 = 4096;

fn free_frames() -> u64 {
    let mut b = [0u8; 16];
    match sys::system_info(SYSTEM, abi::sysinfo::MEMORY, &mut b) {
        Ok(16) => u64::from_le_bytes(b[8..16].try_into().unwrap_or([0; 8])),
        _ => 0,
    }
}

fn say(what: &[u8], v: u64) {
    let mut buf = [0u8; 20];
    sys::print(b"mem_probe: ");
    sys::print(what);
    sys::print(fmt_u64(v, &mut buf));
    sys::print(b"\n");
}

fn main(_arg: u64) -> u64 {
    let before = free_frames();
    // 1, 2: made, nothing taken.
    let Ok(mem) = sys::mem_create(64 * MIB) else {
        return 1;
    };
    let made = free_frames();
    say(
        b"frames taken by making 64 MiB: ",
        before.saturating_sub(made),
    );
    if before.saturating_sub(made) > 256 {
        return 2;
    }
    // 3, 4: every 16th page touched (4 MiB): each reads 0 first, and about
    // that much is taken.
    let Ok(virt) = sys::mmio_map(mem) else {
        return 3;
    };
    let words = (64 * MIB / 4) as usize;
    // SAFETY: the object's mapping, `64 MiB` long, this program's alone.
    let m = unsafe { core::slice::from_raw_parts_mut(virt as *mut u32, words) };
    let step = (16 * PAGE / 4) as usize;
    for i in (0..words).step_by(step) {
        if m[i] != 0 {
            return 4;
        }
        m[i] = i as u32 ^ 0xA5A5_A5A5;
    }
    let touched = free_frames();
    let taken = made.saturating_sub(touched);
    say(b"frames taken by touching 1024 pages: ", taken);
    if !(1024..=1024 + 256).contains(&taken) {
        return 3;
    }
    if (0..words)
        .step_by(step)
        .any(|i| m[i] != i as u32 ^ 0xA5A5_A5A5)
    {
        return 4;
    }
    // 5: a call writes into a page of the object nobody touched.
    let into =
        &mut unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, (64 * MIB) as usize) }
            [(32 * MIB + 100) as usize..(32 * MIB + 164) as usize];
    if sys::system_info(SYSTEM, abi::sysinfo::RANDOM, into) != Ok(64) {
        return 5;
    }
    // 6: a second handle maps to the same place.
    let Ok(again) = sys::handle_duplicate(mem, abi::rights::READ | abi::rights::WRITE) else {
        return 6;
    };
    if sys::mmio_map(again) != Ok(virt) {
        return 6;
    }
    // 7: let go, everything comes back.
    let _ = sys::handle_close(again);
    let _ = sys::mmio_unmap(virt);
    let _ = sys::handle_close(mem);
    let after = free_frames();
    say(
        b"frames short after letting go: ",
        before.saturating_sub(after),
    );
    if before.saturating_sub(after) > 256 {
        return 7;
    }
    // 8: the heap grows past the old 8 MiB.
    let mut big = alloc::vec![0u8; (40 * MIB) as usize];
    for (i, b) in big.iter_mut().enumerate().step_by(PAGE as usize) {
        *b = (i / PAGE as usize) as u8;
    }
    if big
        .iter()
        .step_by(PAGE as usize)
        .enumerate()
        .any(|(i, &b)| b != i as u8)
    {
        return 8;
    }
    say(
        b"heap grew to MiB: ",
        aricode_rt::heap_stats().total as u64 / MIB,
    );
    // 9: beyond the share of the machine (half of QEMU's 512 MiB) is refused,
    // though the manifest asked for 1 GiB.
    if sys::mem_create(300 * MIB) != Err(Error::Limit) {
        return 9;
    }
    // 10: sizes that are not whole pages.
    if sys::mem_create(0) != Err(Error::Invalid) || sys::mem_create(4097) != Err(Error::Invalid) {
        return 10;
    }
    drop(big);
    sys::print(b"mem_probe: ok\n");
    0
}

aricode_rt::entry!(main);
