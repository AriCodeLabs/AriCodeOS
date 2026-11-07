// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `test=thread_kill` (ADR 0082): three threads wait on a futex for ever and the
//! main thread waits to join one of them for ever; only a kill from outside ends
//! the process, and it must wake every one of them. Never exits by itself.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, TIMEOUT_INFINITE};
use aricode_rt::thread;
use core::sync::atomic::AtomicU32;

static NEVER: AtomicU32 = AtomicU32::new(0);

fn main(_arg: u64) -> u64 {
    let mut first = None;
    for _ in 0..3 {
        let Ok(t) = thread::spawn(|| {
            let _ = sys::futex_wait(&NEVER, 0, TIMEOUT_INFINITE);
            sys::print(b"thread_waiters: a waiter woke and went on\n");
            0
        }) else {
            return 2;
        };
        first.get_or_insert(t);
    }
    sys::print(b"thread_waiters: four threads wait for ever\n");
    if let Some(t) = first {
        let _ = t.join();
    }
    sys::print(b"thread_waiters: the main thread joined and went on\n");
    3
}

aricode_rt::entry!(main);
