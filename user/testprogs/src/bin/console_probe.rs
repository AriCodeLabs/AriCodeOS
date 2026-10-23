// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Session console (ADR 0047): run from an SSH shell, or by init with an
//! endpoint as its console, it checks from inside that its console is a
//! console and offers nothing else, then writes two lines and, with
//! `arg` = N, N numbered lines of 100 bytes (sshd's flow control, audit of
//! ADR 0047, M1). Each failing step returns its number; success = 0. The
//! checks hold on the kernel console too, so the same program serves both.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, kind, rights, Error, Handle};

fn main(arg: u64) -> u64 {
    if sys::object_kind(Handle::CONSOLE) != Ok(kind::CONSOLE) {
        return 1;
    }
    // Not an endpoint: no empty message (the end of a session) and no
    // handles can travel through it.
    if sys::ipc_send(Handle::CONSOLE, &[], &[], 0) != Err(Error::Invalid) {
        return 2;
    }
    // It cannot leave the process.
    if sys::handle_duplicate(Handle::CONSOLE, rights::WRITE | rights::TRANSFER)
        != Err(Error::Escalation)
    {
        return 3;
    }
    // A zero-length write is accepted and sends nothing.
    if sys::console_write(Handle::CONSOLE, b"") != Ok(0) {
        return 4;
    }
    sys::print(b"console probe: checks passed\n");
    sys::print(b"console probe: still writing after an empty write\n");
    let mut line = [b'.'; 100];
    line[99] = b'\n';
    for n in 1..=arg {
        let mut digits = [0u8; 20];
        let number = aricode_rt::fmt_u64(n, &mut digits);
        line[..14].copy_from_slice(b"console probe ");
        line[14..14 + number.len()].copy_from_slice(number);
        line[14 + number.len()] = b' ';
        sys::print(&line);
    }
    0
}

aricode_rt::entry!(main);
