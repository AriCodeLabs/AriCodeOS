//! Despacho de syscalls (ABI en `sdk/abi`). Los recursos se nombran por
//! handles con derechos; el número de syscall solo elige la operación.

use super::process;
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::object::{abi_error, KernelObject};
use crate::task::scheduler;
use aricode_abi::{nr, Error, ABI_VERSION};
use aricode_handles::{Handle, Rights};
use log::warn;

pub fn dispatch(frame: &mut SyscallFrame) -> i64 {
    let result: Result<u64, Error> = match frame.number {
        nr::PROCESS_EXIT => process::exit_current(frame.rdi),
        nr::CONSOLE_WRITE => console_write(Handle(frame.rdi as u32), frame.rsi, frame.rdx),
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
        nr::HANDLE_CLOSE => with_handles(|t| t.remove(Handle(frame.rdi as u32)).map(|_| 0)),
        nr::HANDLE_DUPLICATE => with_handles(|t| {
            t.duplicate(Handle(frame.rdi as u32), Rights(frame.rsi as u32))
                .map(|h| h.0 as u64)
        }),
        nr::OBJECT_KIND => with_handles(|t| t.get(Handle(frame.rdi as u32)).map(|(o, _)| o.kind())),
        nr::OBJECT_ID => with_handles(|t| {
            t.get_checked(Handle(frame.rdi as u32), Rights::READ)
                .map(|o| o.id())
        }),
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

fn with_handles<R>(
    f: impl FnOnce(&mut crate::object::Handles) -> Result<R, aricode_handles::HandleError>,
) -> Result<R, Error> {
    process::with_current_handles(f)
        .ok_or(Error::Fault)?
        .map_err(abi_error)
}

fn console_write(handle: Handle, ptr: u64, len: u64) -> Result<u64, Error> {
    const MAX: u64 = 4096;
    let is_console = with_handles(|t| {
        t.get_checked(handle, Rights::WRITE)
            .map(|o| matches!(**o, KernelObject::Console))
    })?;
    if !is_console {
        return Err(Error::Invalid);
    }
    if len > MAX {
        return Err(Error::Invalid);
    }
    let bytes = process::copy_from_user(ptr, len)?;
    let text = core::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
    crate::print!("{text}");
    Ok(len)
}
