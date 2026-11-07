// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A fault on a thread other than the main one ends the whole process (ADR 0082),
//! with its main thread in a wait that would never end: the end must wake it.
//! Never exits by itself: the kernel kills it.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, TIMEOUT_INFINITE};
use aricode_rt::thread;
use core::sync::atomic::AtomicU32;

static NEVER: AtomicU32 = AtomicU32::new(0);

fn main(_arg: u64) -> u64 {
    let spawned = thread::spawn(|| {
        sys::thread_sleep(50);
        sys::print(b"thread_fault: the second thread writes to page 0\n");
        // SAFETY: none: a deliberate fault.
        unsafe { (0x10 as *mut u64).write_volatile(1) };
        0
    });
    if spawned.is_err() {
        return 2;
    }
    sys::print(b"thread_fault: the main thread waits for ever\n");
    let _ = sys::futex_wait(&NEVER, 0, TIMEOUT_INFINITE);
    sys::print(b"thread_fault: the main thread woke and went on\n");
    3
}

aricode_rt::entry!(main);
