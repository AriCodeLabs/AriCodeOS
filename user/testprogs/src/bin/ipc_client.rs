// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Ping-pong client and IPC negative cases. Handle 0x10004 = server
//! endpoint, WRITE only. Success = 0; otherwise, the failing step.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);
const ROUND_TRIPS: usize = 1000;

fn main(_arg: u64) -> u64 {
    let mut buf = [0u8; 16];
    let mut none: [Handle; 0] = [];
    // 1. Without READ we cannot receive from the server endpoint.
    if sys::ipc_receive(EP, &mut buf, &mut none, 0) != Err(Error::Denied) {
        return 1;
    }
    // 2. Our own reply endpoint; a duplicate with WRITE|TRANSFER is
    //    transferred to the server in every message.
    let Ok(reply) = sys::endpoint_create() else {
        return 2;
    };
    for _ in 0..ROUND_TRIPS {
        let Ok(w) = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER) else {
            return 3;
        };
        if sys::ipc_send(EP, b"ping", &[w], TIMEOUT_INFINITE).is_err() {
            return 4;
        }
        // 3. The transferred handle is no longer ours.
        if sys::object_kind(w) != Err(Error::BadHandle) {
            return 5;
        }
        match sys::ipc_receive(reply, &mut buf, &mut none, TIMEOUT_INFINITE) {
            Ok((4, 0)) if &buf[..4] == b"pong" => {}
            _ => return 6,
        }
    }
    // 4. Timeout: nobody writes to `reply`.
    if sys::ipc_receive(reply, &mut buf, &mut none, 30) != Err(Error::Timeout) {
        return 7;
    }
    // 5. Bounded queue: 16 messages fit, the 17th with timeout 0 fails.
    for _ in 0..sys::abi::IPC_QUEUE_CAPACITY {
        if sys::ipc_send(reply, b"x", &[], 0).is_err() {
            return 8;
        }
    }
    if sys::ipc_send(reply, b"x", &[], 0) != Err(Error::Timeout) {
        return 9;
    }
    // 6. A failed send loses no capabilities: the handle is still ours.
    let Ok(keep) = sys::handle_duplicate(reply, rights::READ | rights::TRANSFER) else {
        return 10;
    };
    if sys::ipc_send(reply, b"x", &[keep], 0) != Err(Error::Timeout)
        || sys::object_kind(keep).is_err()
    {
        return 11;
    }
    // 7. Message too large.
    let big = [0u8; sys::abi::IPC_MAX_DATA + 1];
    if sys::ipc_send(reply, &big, &[], 0) != Err(Error::TooBig) {
        return 12;
    }
    // 8. A handle without TRANSFER cannot be sent.
    if sys::ipc_send(reply, b"x", &[Handle::CONSOLE], 0) != Err(Error::Denied) {
        return 13;
    }
    if sys::ipc_send(EP, b"quit", &[], TIMEOUT_INFINITE).is_err() {
        return 14;
    }
    0
}

aricode_rt::entry!(main);
