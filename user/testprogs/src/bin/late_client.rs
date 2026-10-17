// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Client for the late-reply test (stage 1B): the first request times out
//! before the server answers; the second must receive ITS reply ("fresh"),
//! never the late reply to the first one.
//! Handle 0x10004 = server endpoint (WRITE).
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rpc, Error, Handle};

const EP: Handle = Handle(0x0001_0004);

fn main(_arg: u64) -> u64 {
    let mut out = [0u8; 16];
    // 1. The deadline (200 ms) expires before the reply (800 ms).
    match rpc::call(EP, b"one", &[], 500, 200, &mut out) {
        Err(Error::Timeout) => {}
        Ok(n) => {
            sys::print(b"late_client: the first request should not get a reply: ");
            sys::print(&out[..n]);
            sys::print(b"\n");
            return 2;
        }
        Err(_) => return 3,
    }
    // 2. Another request while the late reply is in flight: with a new reply
    // endpoint per request, the late one is lost on the closed endpoint and
    // this one gets its own.
    match rpc::call(EP, b"two", &[], 500, 5000, &mut out) {
        Ok(n) if &out[..n] == b"fresh" => {}
        Ok(n) => {
            sys::print(b"late_client: mixed-up reply: ");
            sys::print(&out[..n]);
            sys::print(b"\n");
            return 4;
        }
        Err(_) => return 5,
    }
    // 3. And one more after the late reply has arrived (and been discarded).
    sys::thread_sleep(900);
    match rpc::call(EP, b"three", &[], 500, 5000, &mut out) {
        Ok(n) if &out[..n] == b"fresh" => {}
        _ => return 6,
    }
    let _ = sys::ipc_send(EP, b"quit", &[], 500);
    sys::print(b"late_client: deadline expired, late reply discarded, replies correlated ok\n");
    0
}

aricode_rt::entry!(main);
