//! Despacho de syscalls (ABI en `sdk/abi`). Los recursos se nombran por
//! handles con derechos; el número de syscall solo elige la operación.

use super::process;
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::ipc::Message;
use crate::object::{abi_error, lookup, KernelObject, ObjectKind, ObjectRef};
use crate::task::scheduler;
use alloc::vec::Vec;
use aricode_abi::{nr, Error, ABI_VERSION, IPC_MAX_DATA, IPC_MAX_HANDLES};
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
        nr::ENDPOINT_CREATE => with_handles(|t| {
            let ep = KernelObject::new(ObjectKind::Endpoint(crate::ipc::Endpoint::new()));
            let rights = Rights::READ
                .union(Rights::WRITE)
                .union(Rights::DUPLICATE)
                .union(Rights::TRANSFER)
                .union(Rights::MANAGE);
            t.insert(ep, rights).map(|h| h.0 as u64).map_err(abi_error)
        }),
        nr::IPC_SEND => ipc_send(h, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9),
        nr::IPC_RECEIVE => ipc_receive(h, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9),
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

/// Endpoint de un handle con `required`, o `Invalid` si el objeto no es un endpoint.
fn endpoint_of(h: Handle, required: Rights) -> Result<ObjectRef, Error> {
    with_handles(|t| {
        let obj = lookup(t, h, required)?;
        match obj.kind {
            ObjectKind::Endpoint(_) => Ok(obj.clone()),
            _ => Err(Error::Invalid),
        }
    })
}

fn ipc_send(
    ep: Handle,
    data_ptr: u64,
    data_len: u64,
    handles_ptr: u64,
    handles_len: u64,
    timeout_ms: u64,
) -> Result<u64, Error> {
    if data_len as usize > IPC_MAX_DATA || handles_len as usize > IPC_MAX_HANDLES {
        return Err(Error::TooBig);
    }
    let obj = endpoint_of(ep, Rights::WRITE)?;
    let data = process::copy_from_user(data_ptr, data_len)?;
    let raw = process::copy_from_user(handles_ptr, handles_len * 4)?;
    let user_handles: Vec<Handle> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| Handle(u32::from_le_bytes(*c)))
        .collect();
    // Comprobar ahora que todos son transferibles (error temprano) y retirarlos
    // solo cuando el endpoint confirme la plaza: un timeout no pierde nada.
    with_handles(|t| {
        for uh in &user_handles {
            lookup(t, *uh, Rights::TRANSFER)?;
        }
        Ok(())
    })?;
    let ObjectKind::Endpoint(endpoint) = &obj.kind else {
        unreachable!()
    };
    endpoint.send_with(timeout_ms, || {
        let handles: Vec<(ObjectRef, Rights)> = with_handles(|t| {
            user_handles
                .iter()
                .map(|uh| t.take(*uh).map_err(abi_error))
                .collect()
        })?;
        Ok(Message { data, handles })
    })?;
    Ok(0)
}

fn ipc_receive(
    ep: Handle,
    buf_ptr: u64,
    buf_len: u64,
    out_ptr: u64,
    out_max: u64,
    timeout_ms: u64,
) -> Result<u64, Error> {
    if buf_len as usize > IPC_MAX_DATA || out_max as usize > IPC_MAX_HANDLES {
        return Err(Error::TooBig);
    }
    let obj = endpoint_of(ep, Rights::READ)?;
    // Comprobar los buffers antes de consumir un mensaje.
    process::copy_to_user(buf_ptr, &[])?;
    let ObjectKind::Endpoint(endpoint) = &obj.kind else {
        unreachable!()
    };
    let msg = endpoint.receive(timeout_ms)?;
    let n = msg.data.len().min(buf_len as usize);
    process::copy_to_user(buf_ptr, &msg.data[..n])?;
    // Insertar las capabilities recibidas; las que no quepan se descartan.
    let received: Vec<u32> = with_handles(|t| {
        Ok(msg
            .handles
            .into_iter()
            .take(out_max as usize)
            .filter_map(|(o, r)| t.insert(o, r).ok())
            .map(|h| h.0)
            .collect())
    })?;
    let raw: Vec<u8> = received.iter().flat_map(|h| h.to_le_bytes()).collect();
    process::copy_to_user(out_ptr, &raw)?;
    Ok(n as u64 | ((received.len() as u64) << 32))
}
