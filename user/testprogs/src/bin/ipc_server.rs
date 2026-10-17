// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Ping-pong server: receives "ping" with a reply handle, answers "pong"
//! through it and closes it; exits on "quit". Handle 0x10004 = endpoint.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);

fn main(_arg: u64) -> u64 {
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 1];
    loop {
        let Ok((n, nh)) = sys::ipc_receive(EP, &mut buf, &mut handles, TIMEOUT_INFINITE) else {
            return 1;
        };
        match &buf[..n] {
            b"ping" if nh == 1 => {
                if sys::ipc_send(handles[0], b"pong", &[], TIMEOUT_INFINITE).is_err() {
                    return 2;
                }
                if sys::handle_close(handles[0]).is_err() {
                    return 3;
                }
            }
            b"quit" => return 0,
            _ => return 4,
        }
    }
}

aricode_rt::entry!(main);
