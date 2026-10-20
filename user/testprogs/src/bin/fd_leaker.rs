// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Descriptor leaker (incident 0011): opens one file of its private
//! directory until fsd refuses a descriptor, says how many it holds and
//! exits without closing any, as a program killed or crashed mid-work
//! would. Launched through the broker with `run fd_leaker` and the manifest
//! `caps = fs`: handles 0x10004 = its fsd session, 0x10005 = HARDWARE.
//! fsd must release the descriptors once the process is gone (ADR 0036),
//! so the shell's next `open` succeeds and `diag` lists the reclaim.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, Handle};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
/// More than fsd's descriptor table (16 slots): the loop must end in a
/// refusal.
const MAX_ATTEMPTS: usize = 32;

fn main(_arg: u64) -> u64 {
    let Ok(fs) = FsClient::new(FSD, HW) else {
        sys::print(b"fd_leaker: no file client\n");
        return 1;
    };
    // The file lives in the private directory; a previous boot may have
    // left it there.
    match fs.create("leak") {
        Ok(_) | Err(FsError::Status(Status::Exists)) => {}
        Err(_) => {
            sys::print(b"fd_leaker: could not create the file\n");
            return 2;
        }
    }
    let mut open = 1;
    let refused = loop {
        if open == MAX_ATTEMPTS {
            sys::print(b"fd_leaker: fsd never refused a descriptor\n");
            return 3;
        }
        match fs.open("leak") {
            Ok(_) => open += 1,
            Err(FsError::Status(Status::NoSpace)) => break true,
            Err(_) => break false,
        }
    };
    let mut buf = [0u8; 20];
    sys::print(b"fd_leaker: holding ");
    sys::print(fmt_u64(open as u64, &mut buf));
    sys::print(b" descriptors, the next open was ");
    sys::print(if refused {
        b"refused (NoSpace); exiting without closing any\n".as_slice()
    } else {
        b"not refused with NoSpace\n".as_slice()
    });
    if refused {
        0
    } else {
        4
    }
}

aricode_rt::entry!(main);
