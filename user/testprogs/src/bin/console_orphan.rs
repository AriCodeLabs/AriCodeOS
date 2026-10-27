// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Typed-ahead console input and a reader that dies
//! (`test=console_orphan`, incident 0047). With `arg` 0 it plays a shell
//! that crashes with a line half read: it says it is ready, takes one byte
//! of the line the runner types, waits for the rest to arrive and faults.
//! With `arg` 1 it plays the shell started next: nothing typed for the
//! dead reader may reach it, so its read must time out.
//! Handles: 0x10004 = console (READ). Exit codes: 0 = nothing left over
//! (`arg` 1), 1 = it received the dead reader's bytes, 2 = a read failed.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Error, Handle};

const CONSOLE_IN: Handle = Handle(0x0001_0004);
/// How long the first reader waits for the line, and the second for
/// anything at all.
const READ_MS: u64 = 15_000;
const LEFTOVER_MS: u64 = 2_000;

fn main(arg: u64) -> u64 {
    let mut buf = [0u8; 64];
    if arg == 0 {
        sys::print(b"CONSOLE_ORPHAN_READY\n");
        if sys::console_read(CONSOLE_IN, &mut buf[..1], READ_MS).is_err() {
            return 2;
        }
        // The rest of the line arrives meanwhile and stays unread.
        sys::thread_sleep(500);
        sys::print(b"console_orphan: one byte read, crashing with the rest unread\n");
        // SAFETY: deliberate fault; page 0 is never mapped and the address
        // is hidden from the compiler so that it emits the read.
        let bad = core::hint::black_box(0x40u64) as *const u64;
        return unsafe { bad.read_volatile() };
    }
    match sys::console_read(CONSOLE_IN, &mut buf, LEFTOVER_MS) {
        Err(Error::Timeout) => {
            sys::print(b"console_orphan: nothing typed for the dead reader arrived ok\n");
            0
        }
        Ok(n) => {
            let mut b = [0u8; 20];
            sys::print(b"console_orphan: received ");
            sys::print(aricode_rt::fmt_u64(n as u64, &mut b));
            sys::print(b" byte(s) typed for the dead reader: ");
            sys::print(&buf[..n]);
            sys::print(b"\n");
            1
        }
        Err(_) => 2,
    }
}

aricode_rt::entry!(main);
