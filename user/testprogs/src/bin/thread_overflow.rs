// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A thread's stack overflowing into its guard page (ADR 0082) ends the process,
//! named as a stack overflow. Never exits by itself.
#![no_std]
#![no_main]

use aricode_rt::sys;
use aricode_rt::thread;

#[inline(never)]
fn deeper(n: u64) -> u64 {
    // Never true, but the compiler cannot tell: the recursion may end.
    if core::hint::black_box(n) == u64::MAX {
        return 0;
    }
    let frame = [n; 64];
    // Read through the frame so the recursion keeps its stack.
    core::hint::black_box(&frame);
    deeper(n + 1) + frame[(n % 64) as usize]
}

fn main(_arg: u64) -> u64 {
    let Ok(t) = thread::spawn(|| deeper(0)) else {
        return 2;
    };
    let _ = t.join();
    sys::print(b"thread_overflow: the thread came back\n");
    3
}

aricode_rt::entry!(main);
