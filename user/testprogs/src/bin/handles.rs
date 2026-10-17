// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Handles (Phase 12): each failing step returns its number; success = 11.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, kind, rights, Error, Handle};

fn main(_arg: u64) -> u64 {
    if sys::object_kind(Handle::CONSOLE) != Ok(kind::CONSOLE) {
        return 1;
    }
    if sys::object_kind(Handle(0x1234)) != Err(Error::BadHandle) {
        return 2;
    }
    let Ok(ro) = sys::handle_duplicate(Handle::CONSOLE, 0) else {
        return 3;
    };
    if sys::console_write(ro, b"ok\n") != Err(Error::Denied) {
        return 4;
    }
    if sys::handle_duplicate(ro, rights::WRITE) != Err(Error::Denied) {
        return 5;
    }
    if sys::handle_duplicate(Handle::CONSOLE, rights::MANAGE) != Err(Error::Escalation) {
        return 6;
    }
    if sys::handle_close(Handle::CONSOLE).is_err()
        || sys::console_write(Handle::CONSOLE, b"ok\n") != Err(Error::BadHandle)
    {
        return 7;
    }
    if sys::console_write(Handle::MAIN_THREAD, b"ok\n") != Err(Error::Denied) {
        return 8;
    }
    if sys::object_id(Handle::MAIN_THREAD) != Ok(sys::thread_get_tid()) {
        return 9;
    }
    11
}

aricode_rt::entry!(main);
