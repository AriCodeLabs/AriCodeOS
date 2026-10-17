// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Driver that crashes on its first start (arg = 0) and works on the second.
#![no_std]
#![no_main]

fn main(arg: u64) -> u64 {
    if arg == 0 {
        // SAFETY: deliberate fault; page 0 is never mapped and the address
        // is hidden from the compiler so that it emits the read.
        let bad = core::hint::black_box(0x40u64) as *const u64;
        return unsafe { bad.read_volatile() };
    }
    0
}

aricode_rt::entry!(main);
