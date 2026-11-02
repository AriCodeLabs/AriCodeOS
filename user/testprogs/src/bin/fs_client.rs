// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! File service client (Phase 20): lists, opens, reads, creates, writes,
//! replaces, flushes and reads back over IPC. Handles: 0x10004 = fsd (WRITE), 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, Handle};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);

fn main(_arg: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FSD, HW) else {
        return 1;
    };
    // 1. Listing.
    let mut name = [0u8; 32];
    let mut count = 0;
    let mut saw_hola = false;
    let mut i = 0;
    while let Ok(Some((n, size))) = fs.list(i, &mut name) {
        sys::print(b"fs_client: ");
        sys::print(&name[..n]);
        sys::print(b" (");
        let mut buf = [0u8; 20];
        sys::print(aricode_rt::fmt_u64(size, &mut buf));
        sys::print(b" bytes)\n");
        saw_hola |= &name[..n] == b"hola.txt";
        count += 1;
        i += 1;
    }
    if count < 2 || !saw_hola {
        return 2;
    }
    // 2. Read hola.txt.
    let Ok(fd) = fs.open("hola.txt") else {
        return 3;
    };
    let mut data = [0u8; 64];
    let n = match fs.read(fd, 0, &mut data) {
        Ok(n) => n,
        Err(_) => return 4,
    };
    if &data[..n] != b"Hola desde AriFS\n" {
        return 5;
    }
    sys::print(b"fs_client: hola.txt says: ");
    sys::print(&data[..n]);
    let _ = fs.close(fd);
    // 3. Create, write, read back. On a disk that already booted before, the
    //    file exists: it is opened and overwritten.
    let fd = match fs.create("nuevo.txt") {
        Ok(fd) => fd,
        Err(FsError::Status(Status::Exists)) => {
            sys::print(b"fs_client: nuevo.txt already existed (persistent disk)\n");
            let Ok(fd) = fs.open("nuevo.txt") else {
                return 6;
            };
            // Empty it so the following checks see the same as on a freshly
            // formatted disk.
            if fs.replace(fd, b"").is_err() {
                return 6;
            }
            fd
        }
        Err(_) => return 6,
    };
    if fs.write(fd, 0, b"escrito desde ring 3") != Ok(20) {
        return 7;
    }
    if fs.read(fd, 8, &mut data) != Ok(12) || &data[..12] != b"desde ring 3" {
        return 8;
    }
    // 4. Atomic replace (ADR 0014): the whole content changes at once and is
    //    durable; explicit flush of the previous writes.
    if fs.replace(fd, b"contenido reemplazado de una vez") != Ok(32) {
        return 12;
    }
    if fs.read(fd, 0, &mut data) != Ok(32) || &data[..32] != b"contenido reemplazado de una vez" {
        return 13;
    }
    if fs.flush().is_err() {
        return 14;
    }
    sys::print(b"fs_client: atomic replace and flush ok\n");
    // 5. Errors: duplicate create, open of a missing file, closed fd.
    if fs.create("nuevo.txt").is_ok() {
        return 9;
    }
    if fs.open("no-existe").is_ok() {
        return 10;
    }
    if fs.close(fd).is_err() || fs.read(fd, 0, &mut data).is_ok() || fs.replace(fd, b"x").is_ok() {
        return 11;
    }
    sys::print(b"fs_client: list, read, create and write over IPC ok\n");
    // 6. Rename over and delete (AriFS v2, ADR 0072): `tmp.new` replaces
    // `old.txt` in one commit, then goes; a volume still in v1 has neither.
    match rename_and_delete(&mut fs) {
        Ok(()) => sys::print(b"fs_client: rename over and delete ok\n"),
        Err(FsError::Status(Status::Unsupported)) => {
            sys::print(b"fs_client: rename and delete unsupported (AriFS v1)\n")
        }
        Err(_) => return 15,
    }
    0
}

fn rename_and_delete(fs: &mut FsClient) -> Result<(), FsError> {
    match fs.delete("absent.txt") {
        Err(FsError::Status(Status::NotFound)) => {}
        Err(e) => return Err(e),
        Ok(()) => return Err(FsError::Status(Status::Io)),
    }
    for (name, text) in [("old.txt", &b"old"[..]), ("tmp.new", &b"new"[..])] {
        let fd = fs.create(name).or_else(|_| fs.open(name))?;
        fs.replace(fd, text)?;
        fs.close(fd)?;
    }
    let tmp = fs.open("tmp.new")?;
    let old = fs.open("old.txt")?;
    fs.rename(tmp, "old.txt")?;
    let mut buf = [0u8; 8];
    // The replaced file's descriptor no longer works; the renamed one does.
    if fs.read(old, 0, &mut buf).is_ok()
        || fs.read(tmp, 0, &mut buf)? != 3
        || &buf[..3] != b"new"
        || fs.open("tmp.new").is_ok()
    {
        return Err(FsError::Status(Status::Io));
    }
    fs.close(tmp)?;
    fs.delete("old.txt")?;
    if fs.open("old.txt").is_ok() {
        return Err(FsError::Status(Status::Io));
    }
    Ok(())
}

aricode_rt::entry!(main);
