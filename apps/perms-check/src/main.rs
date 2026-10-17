// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! `perms_check`: regression for the SDK audit (finding 2). It receives three
//! extra handles (`SYSTEM`, endpoint, `HARDWARE`), closes the first and then
//! the middle one, and requires `permissions::granted` to keep listing the
//! remaining ones: a gap does not hide later permissions.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, kind, permissions, Handle};

/// Returns how many extra handles `granted` lists and a flag per kind.
fn survey() -> (u32, bool, bool, bool) {
    let (mut extra, mut system, mut endpoint, mut hardware) = (0, false, false, false);
    permissions::granted(|h, _| {
        if h.0 < permissions::FIRST_EXTRA {
            return;
        }
        extra += 1;
        system |= permissions::is(h, kind::SYSTEM);
        endpoint |= permissions::is(h, kind::ENDPOINT);
        hardware |= permissions::is(h, kind::HARDWARE);
    });
    (extra, system, endpoint, hardware)
}

fn report(step: &[u8], s: (u32, bool, bool, bool)) {
    sys::print(b"perms_check: ");
    sys::print(step);
    sys::print(b":");
    if s.1 {
        sys::print(b" system");
    }
    if s.2 {
        sys::print(b" endpoint");
    }
    if s.3 {
        sys::print(b" hardware");
    }
    if s.0 == 0 {
        sys::print(b" (none)");
    }
    sys::print(b"\n");
}

fn main(_arg: u64) -> u64 {
    let start = survey();
    report(b"at start", start);
    if start != (3, true, true, true) {
        return 1;
    }
    // Drop the first one (SYSTEM, 0x10004): the rest must still be visible.
    if permissions::drop_handle(Handle(permissions::FIRST_EXTRA)).is_err() {
        return 2;
    }
    let after_first = survey();
    report(b"without system", after_first);
    if after_first != (2, false, true, true) {
        sys::print(b"perms_check: a gap hid the remaining permissions\n");
        return 3;
    }
    // Gap in the middle: close the endpoint (0x10005) and keep HARDWARE.
    if permissions::drop_handle(Handle(permissions::FIRST_EXTRA + 1)).is_err() {
        return 4;
    }
    let after_middle = survey();
    report(b"without endpoint", after_middle);
    if after_middle != (1, false, false, true) {
        return 5;
    }
    sys::print(b"perms_check: enumeration skips gaps ok\n");
    0
}

aricode_rt::entry!(main);
