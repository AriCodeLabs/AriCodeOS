// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! `notes_reader`: receives from the broker an fsd endpoint bound to a single
//! file in read mode (manifest `read_files`, ADR 0016). It lists it, reads it
//! and checks that it cannot create, write, replace or `flush`, and that it
//! does not see other files.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, kind, permissions};

fn denied<T>(r: Result<T, FsError>) -> bool {
    matches!(r, Err(FsError::Status(Status::Denied)))
}

fn main(_arg: u64) -> u64 {
    let (mut shared, mut hardware) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            shared = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (shared, hardware) else {
        sys::print(b"notes_reader: no shared file\n");
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let mut name = [0u8; 32];
    let Ok(Some((n, size))) = fs.list(0, &mut name) else {
        sys::print(b"notes_reader: listing is empty\n");
        return 3;
    };
    if fs.list(1, &mut name).is_ok_and(|e| e.is_some()) {
        sys::print(b"notes_reader: I see more than one file\n");
        return 4;
    }
    let Ok(shown) = core::str::from_utf8(&name[..n]) else {
        return 5;
    };
    let Ok(fd) = fs.open(shown) else {
        return 6;
    };
    let mut data = [0u8; 64];
    let Ok(read) = fs.read(fd, 0, &mut data) else {
        return 7;
    };
    let mut b = [0u8; 20];
    sys::print(b"notes_reader: ");
    sys::print(shown.as_bytes());
    sys::print(b" (");
    sys::print(aricode_rt::fmt_u64(size, &mut b));
    sys::print(b" bytes): ");
    sys::print(&data[..read]);
    if !denied(fs.create("otro"))
        || !denied(fs.write(fd, 0, b"x"))
        || !denied(fs.replace(fd, b"x"))
        || !denied(fs.flush())
    {
        sys::print(b"notes_reader: I could write to a read-only file\n");
        return 8;
    }
    if !matches!(fs.open("README"), Err(FsError::Status(Status::NotFound))) {
        sys::print(b"notes_reader: I could open a non-shared file\n");
        return 9;
    }
    if fs.close(fd).is_err() {
        return 10;
    }
    sys::print(
        b"notes_reader: read-only enforced (Denied on create, write, replace and flush; README invisible)\n",
    );
    0
}

aricode_rt::entry!(main);
