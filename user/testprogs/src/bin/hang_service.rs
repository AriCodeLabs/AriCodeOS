// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Test service that never answers (audit 6ee19fb, finding 4): receives its
//! endpoint and sleeps forever without serving anything. The supervisor must
//! kill it when the startup probe expires.
#![no_std]
#![no_main]

use aricode_rt::sys;

fn main(_arg: u64) -> u64 {
    sys::print(b"hang_service: started, with no intention of answering\n");
    loop {
        sys::thread_sleep(60_000);
    }
}

aricode_rt::entry!(main);
