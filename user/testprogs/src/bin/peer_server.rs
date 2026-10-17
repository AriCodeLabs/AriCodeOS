// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Server that dies without answering (`test=ipc_peer`, ADR 0030). Its
//! client (`peer_client`) must learn it at once, not after its 30 s reply
//! wait. Handles: 0x10004 = service endpoint (READ, the only one),
//! 0x10005 = sync endpoint (READ). `arg` = case:
//! 1. takes one request and exits holding it, unanswered;
//! 2. waits for the client's "queued" and exits without receiving;
//! 3. waits for the client's "full", lets it block on the full queue, exits.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);
const SYNC: Handle = Handle(0x0001_0005);

fn main(case: u64) -> u64 {
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 1];
    match case {
        1 => match sys::ipc_receive(EP, &mut buf, &mut handles, TIMEOUT_INFINITE) {
            // Exits with the reply handle in its table: the kernel drops it
            // when the process is reaped.
            Ok((_, 1)) => 0,
            _ => 11,
        },
        2 | 3 => {
            if sys::ipc_receive(SYNC, &mut buf, &mut handles, TIMEOUT_INFINITE).is_err() {
                return 12;
            }
            if case == 3 {
                // Give the client time to block in its send.
                sys::thread_sleep(200);
            }
            0
        }
        _ => 10,
    }
}

aricode_rt::entry!(main);
