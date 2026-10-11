//! Burns CPU forever (stage 3, quotas and saturation): launched in the
//! background with `spawn` to check that the rest of the system keeps making
//! progress under the round-robin scheduler. Never exits; dies at shutdown.
#![no_std]
#![no_main]

use aricode_rt::sys;

fn main(_arg: u64) -> u64 {
    sys::print(b"cpu_hog: burning CPU\n");
    let mut x = 0u64;
    loop {
        // The value is hidden from the compiler so the loop is not optimized away.
        x = core::hint::black_box(x)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
    }
}

aricode_rt::entry!(main);
