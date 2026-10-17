// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! `hello_app`: the SDK sample app (Phases 27 and 28). It receives from the
//! broker only what its manifest asks for (`system`, `fs`): a read-only
//! clock, a private directory in fsd and the hardware capability for its
//! buffer, bounded by the manifest memory.
#![no_std]
#![no_main]

use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time};
use aricode_rt::{fmt_hex, fmt_u64};

fn main(arg: u64) -> u64 {
    sys::print(b"hello_app: hello from an SDK app (arg ");
    let mut buf = [0u8; 20];
    sys::print(fmt_u64(arg, &mut buf));
    sys::print(b")\n");

    // 1. What I was given.
    sys::print(b"hello_app: capabilities:");
    let mut count = 0;
    let (mut system, mut fs_ep, mut hardware) = (None, None, None);
    permissions::granted(|h, name| {
        sys::print(b" ");
        sys::print(name.as_bytes());
        sys::print(b"(0x");
        let mut hex = [0u8; 16];
        sys::print(fmt_hex(h.0 as u64, 5, &mut hex));
        sys::print(b")");
        if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
        count += 1;
    });
    sys::print(b"\nhello_app: ");
    sys::print(fmt_u64(count, &mut buf));
    sys::print(b" handles\n");

    // 2. The clock, if I have it.
    match system {
        Some(h) => match time::uptime_ms(h) {
            Ok(ms) => {
                sys::print(b"hello_app: uptime ");
                sys::print(fmt_u64(ms / 1000, &mut buf));
                sys::print(b" s\n");
            }
            Err(e) => aricode_rt::print_error(b"hello_app: uptime", e),
        },
        None => sys::print(b"hello_app: no access to the system clock\n"),
    }

    // 2b. Kernel randomness.
    if let Some(h) = system {
        let mut r = [0u8; 8];
        match sys::random(h, &mut r) {
            Ok(8) => {
                let mut hex = [0u8; 16];
                sys::print(b"hello_app: random ");
                sys::print(aricode_rt::fmt_hex(u64::from_le_bytes(r), 16, &mut hex));
                sys::print(b"\n");
            }
            _ => sys::print(b"hello_app: no randomness\n"),
        }
    }

    // 3. Private directory: write, read and list (I only see my own files).
    let (Some(ep), Some(hw)) = (fs_ep, hardware) else {
        sys::print(b"hello_app: no private directory (manifest does not request fs)\n");
        return 0;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 1;
    };
    let fd = match fs.open("notas").or_else(|_| fs.create("notas")) {
        Ok(fd) => fd,
        Err(_) => return 2,
    };
    if fs.write(fd, 0, b"privado").is_err() {
        return 3;
    }
    let mut data = [0u8; 16];
    if fs.read(fd, 0, &mut data) != Ok(7) || &data[..7] != b"privado" {
        return 4;
    }
    let _ = fs.close(fd);
    sys::print(b"hello_app: private file 'notas' written and read\n");
    sys::print(b"hello_app: private ls:");
    let mut name = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, _))) = fs.list(i, &mut name) {
        sys::print(b" ");
        sys::print(&name[..n]);
        if &name[..n] == b"hola.txt" {
            sys::print(b"\nhello_app: I can see other apps' files!\n");
            return 5;
        }
        i += 1;
    }
    sys::print(b"\n");
    if fs.open("hola.txt").is_ok() {
        sys::print(b"hello_app: I could open another app's file!\n");
        return 6;
    }

    // 4. Memory quota: more than the manifest allows -> Limit.
    match sys::dma_alloc(hw, 256 * 1024) {
        Err(sys::Error::Limit) => sys::print(b"hello_app: memory quota enforced (Limit)\n"),
        other => {
            sys::print(b"hello_app: quota was not enforced\n");
            let _ = other;
            return 7;
        }
    }
    0
}

aricode_rt::entry!(main);
