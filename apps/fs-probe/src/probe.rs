// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `fs_probe`: from another private directory it tries to read, write and
//! close descriptors 0..16 without having opened anything. fsd must answer
//! `BadFd` to all of them: descriptors belong to the endpoint that opened
//! them. Then (audit 6ee19fb) it checks that a commit that does not cover its
//! own chunks is rejected without leaking leftovers from `fs_victim`'s
//! journal, and that the transaction `fs_victim` left half done expires and
//! does not block its own.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, kind, permissions};

fn main(_arg: u64) -> u64 {
    let (mut fs_ep, mut hw) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (fs_ep, hw) else {
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let mut buf = [0u8; 32];
    for fd in 0..16u32 {
        match fs.read(fd, 0, &mut buf) {
            Ok(n) => {
                sys::print(b"fs_probe: CROSS ACCESS on read: ");
                sys::print(&buf[..n]);
                sys::print(b"\n");
                return 3;
            }
            Err(FsError::Status(Status::BadFd)) => {}
            Err(_) => return 4,
        }
        if !matches!(fs.write(fd, 0, b"X"), Err(FsError::Status(Status::BadFd))) {
            sys::print(b"fs_probe: CROSS ACCESS on write\n");
            return 5;
        }
        if !matches!(fs.close(fd), Err(FsError::Status(Status::BadFd))) {
            sys::print(b"fs_probe: CROSS ACCESS on close\n");
            return 6;
        }
    }
    // And its own directory still works.
    let Ok(own) = fs.create("mio") else {
        return 7;
    };
    if fs.write(own, 0, b"ok").is_err() {
        return 8;
    }
    sys::print(
        b"fs_probe: foreign descriptors inaccessible (BadFd on 16 x read, write and close)\n",
    );
    // fs_victim left a replace half done. fsd releases it when the kernel
    // reports the victim gone (ADR 0036), asking at the latest when this
    // replace finds the journal busy; were the owner alive but idle, the
    // transaction would expire (STAGING_TIMEOUT_MS) instead. Either way
    // nothing of the victim's blocks this app.
    let mut tries = 0u64;
    loop {
        match fs.replace(own, b"reemplazo propio") {
            Ok(_) => break,
            Err(FsError::Status(Status::Busy)) if tries < 60 => {
                tries += 1;
                sys::thread_sleep(100);
            }
            Err(_) => return 9,
        }
    }
    sys::print(b"fs_probe: replace after an abandoned transaction ok (");
    if tries == 0 {
        sys::print(b"journal released with its owner)\n");
    } else {
        let mut buf = [0u8; 20];
        sys::print(b"busy for ");
        sys::print(aricode_rt::fmt_u64(tries * 100, &mut buf));
        sys::print(b" ms first)\n");
    }
    // A one-byte chunk and a commit of 22: fsd must not fill the missing
    // bytes with what fs_victim left in the journal.
    if fs.replace_chunk(own, 0, b"Y").is_err() {
        return 10;
    }
    match fs.replace_commit(own, 22) {
        Err(FsError::Status(Status::Invalid)) => {}
        Ok(_) => {
            sys::print(b"fs_probe: INCOMPLETE COMMIT ACCEPTED\n");
            return 11;
        }
        Err(_) => return 12,
    }
    let n = match fs.read(own, 0, &mut buf) {
        Ok(n) => n,
        Err(_) => return 13,
    };
    if buf[..n].windows(9).any(|w| w == b"YNTHETIC_") {
        sys::print(b"fs_probe: FOREIGN JOURNAL LEFTOVERS: ");
        sys::print(&buf[..n]);
        sys::print(b"\n");
        return 14;
    }
    if &buf[..n] != b"reemplazo propio" {
        return 15;
    }
    let _ = fs.close(own);
    sys::print(b"fs_probe: journal does not leak foreign data (incomplete commit rejected)\n");
    0
}

aricode_rt::entry!(main);
