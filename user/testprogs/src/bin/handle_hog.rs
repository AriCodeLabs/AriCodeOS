// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Exhausts the process handle table (stage 3, quotas): creates endpoints
//! until the kernel answers `Limit`, checks that the rejection is predictable
//! (the table has 256 entries and four are already taken), frees them all and
//! shows that creation works again. The limit is per process: others do not
//! notice it.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Error, Handle};

/// Entries in a process handle table (the kernel's `HANDLE_LIMIT`).
const TABLE: usize = 256;
/// Process, thread, console and the `SYSTEM` handle the broker grants without a manifest.
const HELD: usize = 4;

fn main(_arg: u64) -> u64 {
    let mut created = [Handle(0); TABLE];
    let mut count = 0;
    let stop = loop {
        match sys::endpoint_create() {
            Ok(h) if count < TABLE => {
                created[count] = h;
                count += 1;
            }
            Ok(_) => break Err(()),
            Err(e) => break Ok(e),
        }
    };
    let mut b = [0u8; 20];
    match stop {
        Ok(Error::Limit) if count == TABLE - HELD => {
            sys::print(b"handle_hog: handle limit reached (Limit) after creating ");
            sys::print(aricode_rt::fmt_u64(count as u64, &mut b));
            sys::print(b" endpoints\n");
        }
        Ok(e) => {
            sys::print(b"handle_hog: unexpected error after ");
            sys::print(aricode_rt::fmt_u64(count as u64, &mut b));
            sys::print(b" endpoints: ");
            aricode_rt::print_error(b"", e);
            return 1;
        }
        Err(()) => {
            sys::print(b"handle_hog: the table has no limit\n");
            return 2;
        }
    }
    for h in &created[..count] {
        if sys::handle_close(*h).is_err() {
            return 3;
        }
    }
    match sys::endpoint_create() {
        Ok(h) => {
            let _ = sys::handle_close(h);
            sys::print(b"handle_hog: table freed, creation works again ok\n");
            0
        }
        Err(_) => 4,
    }
}

aricode_rt::entry!(main);
