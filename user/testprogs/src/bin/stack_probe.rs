// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Checks the kernel's user stack high-water mark (`sysinfo::STACKS`,
//! incident 0018) against a depth it knows: two nested 12 KiB frames take
//! at least 24 KiB of its 64 KiB stack, so its own entry must say at least
//! that much and less than the whole stack (a scan from the wrong end, or
//! of the wrong bit, gives one page or nothing). Launched from the shell
//! with `run stack_probe` and no manifest: handle 0x10004 is `SYSTEM`
//! (`READ`).
#![no_std]
#![no_main]

use aricode_rt::fmt_u64;
use aricode_rt::sys::abi::sysinfo::{self, StackEntry};
use aricode_rt::sys::{self, Handle};

const SYSTEM: Handle = Handle(0x0001_0004);
const NAME: &str = "stack_probe";
/// One frame; two of them nested make the known depth. Each stays under
/// the 16 KiB frame budget of `cargo xtask stack-check`.
const FRAME: usize = 12 * 1024;

#[inline(never)]
fn inner() -> u8 {
    let mut buf = [0u8; FRAME];
    for i in (0..FRAME).step_by(512) {
        buf[i] = i as u8;
    }
    core::hint::black_box(&mut buf);
    buf[FRAME - 512]
}

#[inline(never)]
fn outer() -> u8 {
    let mut buf = [0u8; FRAME];
    for i in (0..FRAME).step_by(512) {
        buf[i] = i as u8;
    }
    core::hint::black_box(&mut buf);
    buf[0] ^ inner()
}

fn main(_arg: u64) -> u64 {
    core::hint::black_box(outer());
    let mut buf = [0u8; StackEntry::LEN * 32];
    let Ok(n) = sys::system_info(SYSTEM, sysinfo::STACKS, &mut buf) else {
        sys::print(b"stack_probe: no access to stack use\n");
        return 1;
    };
    let mine = buf[..n]
        .as_chunks::<{ StackEntry::LEN }>()
        .0
        .iter()
        .filter_map(|c| StackEntry::decode(c))
        .find(|e| e.name_str() == NAME);
    let Some(e) = mine else {
        sys::print(b"stack_probe: no entry of its own\n");
        return 2;
    };
    let mut b = [0u8; 20];
    sys::print(b"stack_probe: touched ");
    sys::print(fmt_u64(e.touched as u64, &mut b));
    sys::print(b" of ");
    sys::print(fmt_u64(e.size as u64, &mut b));
    sys::print(b" bytes after two 12 KiB frames: ");
    if (e.touched as usize) >= 2 * FRAME && e.touched < e.size {
        sys::print(b"ok\n");
        0
    } else {
        sys::print(b"WRONG\n");
        3
    }
}

aricode_rt::entry!(main);
