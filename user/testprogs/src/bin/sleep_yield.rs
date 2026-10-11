//! sleep, yield, get_tid and abi_version from ring 3; exits with 3 if everything checks out.
#![no_std]
#![no_main]

use aricode_rt::sys;

fn main(_arg: u64) -> u64 {
    sys::thread_sleep(20);
    sys::thread_yield();
    let tid = sys::thread_get_tid();
    if sys::abi_version() != sys::abi::ABI_VERSION as u64 || tid <= 1 {
        return 4;
    }
    3
}

aricode_rt::entry!(main);
