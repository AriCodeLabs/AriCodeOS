// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Floods the kernel event ring (audit 2026-10-25 E, L4): 200
//! `event_record` calls in a row, then, after a pause long enough for its
//! budget to refill, one more. The kernel keeps 16 of the flood and reports
//! the rest as `events dropped` with the last one, so the events before the
//! flood stay in the ring (`run event_flood`, diag.event_flood).
#![no_std]
#![no_main]

use aricode_rt::sys;

const FLOOD: u64 = 200;

fn main(_arg: u64) -> u64 {
    for i in 0..FLOOD {
        let _ = sys::event_record(i, "event flood");
    }
    sys::thread_sleep(600);
    if sys::event_record(FLOOD, "event flood end").is_err() {
        return 1;
    }
    sys::print(b"event_flood: 201 events asked for\n");
    0
}

aricode_rt::entry!(main);
