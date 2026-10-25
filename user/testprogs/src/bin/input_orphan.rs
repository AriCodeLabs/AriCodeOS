// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A reader that dies with its read waiting (`input_orphan`, audit
//! 2026-10-25 C, M4): it starts the longest read it can on the input
//! service and exits without waiting for the reply. The driver holds that
//! read in its one waiting place; the keys typed next must still reach the
//! next reader instead of going to the dead one. Handle 0x10004 = the
//! input endpoint (WRITE). Success = 0.
#![no_std]
#![no_main]

use aricode_rt::sys::input::InputClient;
use aricode_rt::sys::{self, Handle};

const EP: Handle = Handle(0x0001_0004);

fn main(_arg: u64) -> u64 {
    let Ok(dev) = InputClient::new(EP) else {
        return 1;
    };
    if dev.start_read(u32::MAX).is_err() {
        return 2;
    }
    sys::print(b"input_orphan: read started; exiting with it waiting\n");
    0
}

aricode_rt::entry!(main);
