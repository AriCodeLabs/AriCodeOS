// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test of `ipc_receive_any` (stage 5, ADR 0020): receive from the first of
//! several endpoints that has a message, with a deadline; the returned index
//! says which one; with no messages the deadline expires; a handle that is
//! not an endpoint or lacks READ is rejected before waiting.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rights, Error, Handle};

fn main(_arg: u64) -> u64 {
    let Ok(a) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(b) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(c) = sys::endpoint_create() else {
        return 1;
    };
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 4];
    // 1. No messages: deadline expires.
    if sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 20) != Err(Error::Timeout) {
        sys::print(b"ipc_any: with no messages the deadline should expire\n");
        return 2;
    }
    // 2. Message on the third one: index 2 and its data, with a transferred handle.
    let Ok(extra) = sys::endpoint_create() else {
        return 1;
    };
    let Ok(dup) = sys::handle_duplicate(extra, rights::WRITE | rights::TRANSFER) else {
        return 1;
    };
    if sys::ipc_send(c, b"tercero", &[dup], 0).is_err() {
        return 3;
    }
    match sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 1000) {
        Ok((2, n, 1)) if &buf[..n] == b"tercero" => {
            let _ = sys::handle_close(handles[0]);
        }
        other => {
            sys::print(b"ipc_any: unexpected result from the third endpoint\n");
            let _ = other;
            return 4;
        }
    }
    // 3. Messages on two at once: both are served, one per call, in index
    //    order.
    if sys::ipc_send(b, b"segundo", &[], 0).is_err()
        || sys::ipc_send(a, b"primero", &[], 0).is_err()
    {
        return 5;
    }
    let first = sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 1000);
    if !matches!(first, Ok((0, 7, 0))) || &buf[..7] != b"primero" {
        sys::print(b"ipc_any: the first one with a message should be index 0\n");
        return 6;
    }
    let second = sys::ipc_receive_any(&[a, b, c], &mut buf, &mut handles, 1000);
    if !matches!(second, Ok((1, 7, 0))) || &buf[..7] != b"segundo" {
        return 7;
    }
    // 4. A handle without READ or that is not an endpoint: Denied/Invalid without waiting.
    let Ok(write_only) = sys::handle_duplicate(a, rights::WRITE) else {
        return 1;
    };
    if sys::ipc_receive_any(&[write_only, b], &mut buf, &mut handles, 1000) != Err(Error::Denied) {
        sys::print(b"ipc_any: an endpoint without READ should be rejected\n");
        return 8;
    }
    if sys::ipc_receive_any(&[Handle::SELF_PROCESS], &mut buf, &mut handles, 1000)
        != Err(Error::Invalid)
    {
        return 9;
    }
    if sys::ipc_receive_any(&[], &mut buf, &mut handles, 10) != Err(Error::Invalid) {
        return 10;
    }
    sys::print(b"ipc_any: receive from several endpoints ok\n");
    0
}

aricode_rt::entry!(main);
