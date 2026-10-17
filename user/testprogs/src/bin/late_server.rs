// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Server that answers the first request late (stage 1B): the first one
//! gets "late" after 800 ms; the rest get "fresh" immediately.
//! Handle 0x10004 = endpoint. Exits on "quit".
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);

fn main(_arg: u64) -> u64 {
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 1];
    let mut served = 0u32;
    loop {
        let Ok((n, nh)) = sys::ipc_receive(EP, &mut buf, &mut handles, TIMEOUT_INFINITE) else {
            return 1;
        };
        if &buf[..n] == b"quit" {
            return 0;
        }
        if nh != 1 {
            return 4;
        }
        served += 1;
        let reply: &[u8] = if served == 1 {
            sys::thread_sleep(800);
            b"late"
        } else {
            b"fresh"
        };
        // A late reply may find the endpoint already closed: that is not
        // the server's error.
        let _ = sys::ipc_send(handles[0], reply, &[], 500);
        let _ = sys::handle_close(handles[0]);
    }
}

aricode_rt::entry!(main);
