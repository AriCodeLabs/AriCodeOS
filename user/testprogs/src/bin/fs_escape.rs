// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Tries to leave its private directory (audit 2026-10-25 C, H1). It fills
//! fsd's descriptor table from its own session and sends `Open "hola.txt"`
//! (a file of the main namespace) carrying its only send side of the
//! session as the third handle: the kernel takes it from this process with
//! the send, and fsd closes extra handles before serving, so when the full
//! table makes fsd sweep for dead clients while serving that very request,
//! the session has no sender left and is retired, whatever the timing. The
//! request must then be refused: served without a session it would see the
//! whole namespace. Launched through the broker with `run fs_escape` and the
//! manifest `caps = fs`: handles 0x10004 = its fsd session, 0x10005 =
//! HARDWARE. Success (refused) = 0.
#![no_std]
#![no_main]

use aricode_proto::fs::{Op, Reply, Request, Status};
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, abi, rpc, Handle};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
/// More than fsd's descriptor table (16 slots).
const MAX_ATTEMPTS: usize = 32;

fn main(_arg: u64) -> u64 {
    let Ok(fs) = FsClient::new(FSD, HW) else {
        return 1;
    };
    match fs.create("x") {
        Ok(_) | Err(FsError::Status(Status::Exists)) => {}
        Err(_) => return 2,
    }
    let mut open = 0;
    loop {
        if open == MAX_ATTEMPTS {
            return 3;
        }
        match fs.open("x") {
            Ok(_) => open += 1,
            Err(FsError::Status(Status::NoSpace)) => break,
            Err(_) => return 4,
        }
    }
    let Some(req) = Request::new(Op::Open).with_name("hola.txt") else {
        return 5;
    };
    // The second handle is the request's data buffer slot (unused by
    // `Open`): any transferable handle.
    let Ok((filler, other)) = sys::endpoint_create_pair() else {
        return 6;
    };
    let _ = sys::handle_close(other);
    let Ok(reply) = rpc::send(FSD, &req.encode(), &[filler, FSD], 1000) else {
        return 6;
    };
    let mut msg = [0u8; Reply::LEN];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    let got = sys::ipc_receive(reply, &mut msg, &mut handles, 5000);
    let _ = sys::handle_close(reply);
    let Some(r) = got.ok().and_then(|(len, _)| Reply::decode(&msg[..len])) else {
        sys::print(b"fs_escape: no reply\n");
        return 7;
    };
    let mut b = [0u8; 20];
    if r.status == Status::Ok {
        sys::print(b"fs_escape: opened hola.txt outside its directory (fd ");
        sys::print(fmt_u64(r.value, &mut b));
        sys::print(b")\n");
        return 10;
    }
    sys::print(b"fs_escape: refused (status ");
    sys::print(fmt_u64(r.status as u64, &mut b));
    sys::print(b")\n");
    0
}

aricode_rt::entry!(main);
