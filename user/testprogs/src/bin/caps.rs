// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Capabilities (Phase 13): revocation and derivation. Success = 21; otherwise, the failing step.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rights, Error, Handle};

fn main(_arg: u64) -> u64 {
    // Derivation: a duplicate with fewer rights is still the same object.
    let Ok(dup) = sys::handle_duplicate(Handle::CONSOLE, rights::WRITE) else {
        return 1;
    };
    if sys::console_write(dup, b"writing through the duplicate\n").is_err() {
        return 2;
    }
    // Revoking requires MANAGE: the console lacks it.
    if sys::object_revoke(Handle::CONSOLE) != Err(Error::Denied) {
        return 3;
    }
    // The process does have MANAGE over itself: duplicating without MANAGE and
    // revoking through the original also invalidates the duplicate.
    let Ok(self_ro) = sys::handle_duplicate(Handle::SELF_PROCESS, rights::READ) else {
        return 4;
    };
    if sys::object_id(self_ro).is_err() {
        return 5;
    }
    if sys::object_revoke(Handle::SELF_PROCESS).is_err() {
        return 6;
    }
    if sys::object_id(self_ro) != Err(Error::Revoked) {
        return 7;
    }
    if sys::object_id(Handle::SELF_PROCESS) != Err(Error::Revoked) {
        return 8;
    }
    // A revoked handle can be closed, but not duplicated.
    if sys::handle_duplicate(self_ro, rights::READ) != Err(Error::Revoked) {
        return 9;
    }
    if sys::handle_close(self_ro).is_err() {
        return 10;
    }
    // The console is still alive: revocation is per object.
    if sys::console_write(dup, b"the console is still alive\n").is_err() {
        return 11;
    }
    21
}

aricode_rt::entry!(main);
