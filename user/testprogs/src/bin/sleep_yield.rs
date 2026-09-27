//! sleep, yield, get_tid y abi_version desde ring 3; sale con 3 si todo cuadra.
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
