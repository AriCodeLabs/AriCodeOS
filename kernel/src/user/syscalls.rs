//! Despacho de syscalls (ABI en `sdk/abi`).

use super::process;
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::task::scheduler;
use aricode_abi::{nr, Error, ABI_VERSION};
use log::warn;

pub fn dispatch(frame: &mut SyscallFrame) -> i64 {
    let result: Result<u64, Error> = match frame.number {
        nr::PROCESS_EXIT => process::exit_current(frame.rdi),
        nr::CONSOLE_WRITE => console_write(frame.rdi, frame.rsi),
        nr::THREAD_YIELD => {
            scheduler::yield_now();
            Ok(0)
        }
        nr::THREAD_SLEEP => {
            scheduler::sleep_ms(frame.rdi);
            Ok(0)
        }
        nr::THREAD_GET_TID => Ok(scheduler::current_tid().0 as u64),
        nr::ABI_VERSION => Ok(ABI_VERSION as u64),
        n => {
            warn!("syscall desconocida {n} desde rip={:#x}", frame.user_rip);
            Err(Error::NoSys)
        }
    };
    match result {
        Ok(v) => v as i64,
        Err(e) => e.to_raw(),
    }
}

fn console_write(ptr: u64, len: u64) -> Result<u64, Error> {
    const MAX: u64 = 4096;
    if len > MAX {
        return Err(Error::Invalid);
    }
    let bytes = process::copy_from_user(ptr, len)?;
    let text = core::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
    crate::print!("{text}");
    Ok(len)
}
