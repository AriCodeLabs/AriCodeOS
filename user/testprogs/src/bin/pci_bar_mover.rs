// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Driver that moves a BAR of its function and dies (`test=pci_stale_line`,
//! incident 0050): through its writable configuration page it turns memory
//! decoding off (so the moved BAR overlaps nothing) and writes a new
//! address into a BAR (the kernel's test picks the one that holds the
//! MSI-X table, or another), then faults. The next claim must not program
//! the table where the function does not decode it.
//! Argument: the BAR register's offset in the high 32 bits, the value to
//! write in the low 32 bits.
//! Handles: 0x10004 = the function's configuration (claim). Exit code
//! (only if something fails before the fault): 1 = no mapping.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle};

const CFG: Handle = Handle(0x0001_0004);
/// Command register and its Memory Space Enable bit (PCI Local Bus 6.2.2).
const COMMAND: u64 = 0x04;
const MEMORY: u16 = 1 << 1;

fn main(arg: u64) -> u64 {
    let Ok(virt) = sys::mmio_map(CFG) else {
        return 1;
    };
    let command = (virt + COMMAND) as *mut u16;
    let bar = (virt + (arg >> 32)) as *mut u32;
    // SAFETY: the function's configuration page, mapped uncached; a 16-bit
    // access leaves the status register (write one to clear) alone, and the
    // BAR offset comes from the kernel's test.
    unsafe {
        command.write_volatile(command.read_volatile() & !MEMORY);
        bar.write_volatile(arg as u32);
    }
    // SAFETY: deliberate fault; page 0 is never mapped and the address is
    // hidden from the compiler so that it emits the read.
    let bad = core::hint::black_box(0x40u64) as *const u64;
    unsafe { bad.read_volatile() }
}

aricode_rt::entry!(main);
