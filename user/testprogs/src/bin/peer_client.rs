// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Client of a server that dies (`test=ipc_peer`, ADR 0030): every wait
//! here is 30 s, and every one must end at once with `PeerClosed` (the
//! kernel self-test bounds the whole case to a few seconds). Handles:
//! 0x10004 = service endpoint (WRITE), 0x10005 = sync endpoint (WRITE).
//! `arg` = case (see `peer_server`); case 4 runs alone, with its own
//! endpoints: send side gone, receive side gone, `ipc_receive_any`,
//! `endpoint_close` and revocation. Success = 0; otherwise the failing step.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rights, rpc, Error, Handle};

const EP: Handle = Handle(0x0001_0004);
const SYNC: Handle = Handle(0x0001_0005);
const WAIT_MS: u64 = 30_000;

fn main(case: u64) -> u64 {
    let code = match case {
        1 => died_holding_the_request(),
        2 => died_with_the_request_queued(),
        3 => died_with_a_sender_blocked(),
        4 => local_endpoints(),
        _ => 10,
    };
    if code == 0 {
        let mut b = [0u8; 20];
        sys::print(b"peer_client: case ");
        sys::print(aricode_rt::fmt_u64(case, &mut b));
        sys::print(b" ok\n");
    }
    code
}

/// The server took the request and exited: the reply wait ends because the
/// only send handle of the reply endpoint died with it; the next call is
/// refused because the service endpoint has no receiver left.
fn died_holding_the_request() -> u64 {
    let mut out = [0u8; 16];
    if rpc::call(EP, b"req", &[], WAIT_MS, WAIT_MS, &mut out) != Err(Error::PeerClosed) {
        return 21;
    }
    if rpc::call(EP, b"again", &[], WAIT_MS, WAIT_MS, &mut out) != Err(Error::PeerClosed) {
        return 22;
    }
    0
}

/// The request was still queued when the server exited: the queue is
/// discarded with the reply handle inside it.
fn died_with_the_request_queued() -> u64 {
    let Ok((reply, reply_w)) = sys::endpoint_create_pair() else {
        return 31;
    };
    if sys::ipc_send(EP, b"req", &[reply_w], WAIT_MS).is_err() {
        return 32;
    }
    if sys::ipc_send(SYNC, b"queued", &[], WAIT_MS).is_err() {
        return 33;
    }
    let mut out = [0u8; 16];
    let mut none = [Handle(0); 1];
    if sys::ipc_receive(reply, &mut out, &mut none, WAIT_MS) != Err(Error::PeerClosed) {
        return 34;
    }
    0
}

/// A sender waiting for room in a full queue wakes when the receiver goes.
fn died_with_a_sender_blocked() -> u64 {
    for _ in 0..sys::abi::IPC_QUEUE_CAPACITY {
        if sys::ipc_send(EP, b"fill", &[], 0).is_err() {
            return 41;
        }
    }
    if sys::ipc_send(EP, b"fill", &[], 0) != Err(Error::Timeout) {
        return 42;
    }
    if sys::ipc_send(SYNC, b"full", &[], WAIT_MS).is_err() {
        return 43;
    }
    if sys::ipc_send(EP, b"blocked", &[], WAIT_MS) != Err(Error::PeerClosed) {
        return 44;
    }
    0
}

fn local_endpoints() -> u64 {
    let mut buf = [0u8; 16];
    let mut none = [Handle(0); 1];
    // 1. Send side gone: queued messages first, then PeerClosed.
    let Ok((r, w)) = sys::endpoint_create_pair() else {
        return 51;
    };
    if sys::ipc_send(w, b"last", &[], 0).is_err() || sys::handle_close(w).is_err() {
        return 52;
    }
    if !matches!(
        sys::ipc_receive(r, &mut buf, &mut none, WAIT_MS),
        Ok((4, 0))
    ) || &buf[..4] != b"last"
    {
        return 53;
    }
    if sys::ipc_receive(r, &mut buf, &mut none, WAIT_MS) != Err(Error::PeerClosed) {
        return 54;
    }
    // 2. Receive side gone: the send is refused.
    let Ok((r, w)) = sys::endpoint_create_pair() else {
        return 55;
    };
    if sys::handle_close(r).is_err()
        || sys::ipc_send(w, b"x", &[], WAIT_MS) != Err(Error::PeerClosed)
    {
        return 56;
    }
    // 3. ipc_receive_any: PeerClosed only once every endpoint lost its
    //    send side.
    let (Ok((r1, w1)), Ok((r2, w2))) = (sys::endpoint_create_pair(), sys::endpoint_create_pair())
    else {
        return 57;
    };
    let mut handles = [Handle(0); 4];
    if sys::handle_close(w1).is_err()
        || sys::ipc_receive_any(&[r1, r2], &mut buf, &mut handles, 20) != Err(Error::Timeout)
    {
        return 58;
    }
    if sys::handle_close(w2).is_err()
        || sys::ipc_receive_any(&[r1, r2], &mut buf, &mut handles, WAIT_MS)
            != Err(Error::PeerClosed)
    {
        return 59;
    }
    // 4. endpoint_close: needs MANAGE; closes both sides for every holder
    //    and leaves the handles valid.
    let Ok(e) = sys::endpoint_create() else {
        return 60;
    };
    let Ok(ew) = sys::handle_duplicate(e, rights::WRITE | rights::DUPLICATE) else {
        return 61;
    };
    if sys::endpoint_close(ew) != Err(Error::Denied) {
        return 62;
    }
    if sys::endpoint_close(e).is_err()
        || sys::ipc_send(ew, b"x", &[], WAIT_MS) != Err(Error::PeerClosed)
        || sys::ipc_receive(e, &mut buf, &mut none, WAIT_MS) != Err(Error::PeerClosed)
        || sys::handle_duplicate(ew, rights::WRITE).is_err()
    {
        return 63;
    }
    // 5. Revocation also invalidates the handles.
    let Ok(e) = sys::endpoint_create() else {
        return 64;
    };
    let Ok(ew) = sys::handle_duplicate(e, rights::WRITE) else {
        return 65;
    };
    if sys::object_revoke(e).is_err()
        || sys::ipc_send(ew, b"x", &[], WAIT_MS) != Err(Error::Revoked)
    {
        return 66;
    }
    0
}

aricode_rt::entry!(main);
