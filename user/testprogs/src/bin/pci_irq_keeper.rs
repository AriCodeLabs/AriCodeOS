// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Driver whose interrupt line outlives its claim (`test=pci_stale_line`,
//! incident 0050): it sends its IRQ handle (which carries `TRANSFER`) in a
//! message nobody receives yet and faults. The claim ends with it, the
//! line stays in the queue, and the kernel claims the function again
//! before dropping that message: the old line must not mask the interrupts
//! of the new claim.
//! Handles: 0x10004 = the function's configuration (claim), 0x10005 = its
//! IRQ, 0x10006 = an endpoint (WRITE). Exit code (only if something fails
//! before the fault): 1 = the send failed.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};

const IRQ: Handle = Handle(0x0001_0005);
const MAILBOX: Handle = Handle(0x0001_0006);

fn main(_arg: u64) -> u64 {
    if sys::ipc_send(MAILBOX, b"irq", &[IRQ], 0).is_err() {
        return 1;
    }
    // SAFETY: deliberate fault; page 0 is never mapped and the address is
    // hidden from the compiler so that it emits the read.
    let bad = core::hint::black_box(0x40u64) as *const u64;
    unsafe { bad.read_volatile() }
}

aricode_rt::entry!(main);
