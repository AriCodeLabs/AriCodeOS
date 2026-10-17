// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test service for the supervisor (stage 1): answers "ping" with "pong" on
//! endpoint 0x10004; on "die" it replies "pong" and then crashes. Used to
//! trigger deterministic restarts and check that the endpoint, owned by init,
//! survives every generation.
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
        let die = match &buf[..n] {
            b"ping" if nh == 1 => false,
            b"die" if nh == 1 => true,
            _ => return 4,
        };
        // The reply endpoint may be gone (a client that stopped waiting):
        // that is no reason to die.
        let _ = sys::ipc_send(handles[0], b"pong", &[], 500);
        let _ = sys::handle_close(handles[0]);
        if die {
            // SAFETY: deliberate fault; page 0 is never mapped and the address
            // is hidden from the compiler so that it emits the read.
            let bad = core::hint::black_box(0x40u64) as *const u64;
            return unsafe { bad.read_volatile() };
        }
    }
}

aricode_rt::entry!(main);
