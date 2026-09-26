//! Despacho de syscalls (ABI en `sdk/abi`). Los recursos se nombran por
//! handles con derechos; el número de syscall solo elige la operación.

use super::process;
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::object::{abi_error, lookup, ObjectKind};
use crate::task::scheduler;
use aricode_abi::{nr, Error, ABI_VERSION};
use aricode_handles::{Handle, Rights};
use log::warn;

pub fn dispatch(frame: &mut SyscallFrame) -> i64 {
    let h = Handle(frame.rdi as u32);
    let result: Result<u64, Error> = match frame.number {
        nr::PROCESS_EXIT => process::exit_current(frame.rdi),
        nr::CONSOLE_WRITE => console_write(h, frame.rsi, frame.rdx),
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
        nr::HANDLE_CLOSE => with_handles(|t| t.remove(h).map(|_| 0).map_err(abi_error)),
        nr::HANDLE_DUPLICATE => with_handles(|t| {
            lookup(t, h, Rights::NONE)?;
            t.duplicate(h, Rights(frame.rsi as u32))
                .map(|n| n.0 as u64)
                .map_err(abi_error)
        }),
        nr::OBJECT_KIND => with_handles(|t| lookup(t, h, Rights::NONE).map(|o| o.kind_code())),
        nr::OBJECT_ID => with_handles(|t| lookup(t, h, Rights::READ).map(|o| o.id())),
        nr::OBJECT_REVOKE => with_handles(|t| {
            lookup(t, h, Rights::MANAGE)?.revoke();
            Ok(0)
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
    f: impl FnOnce(&mut crate::object::Handles) -> Result<R, Error>,
) -> Result<R, Error> {
    process::with_current_handles(f).ok_or(Error::Fault)?
}

fn console_write(handle: Handle, ptr: u64, len: u64) -> Result<u64, Error> {
    const MAX: u64 = 4096;
    let is_console = with_handles(|t| {
        lookup(t, handle, Rights::WRITE).map(|o| matches!(o.kind, ObjectKind::Console))
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
