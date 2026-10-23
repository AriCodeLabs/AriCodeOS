// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Syscall dispatch (ABI in `sdk/abi`). Resources are named by handles with
//! rights; the syscall number only selects the operation.

use super::process;
use crate::arch::x86_64::syscall::SyscallFrame;
use crate::ipc::Message;
use crate::object::{abi_error, insert, lookup, Cap, KernelObject, ObjectKind, ObjectRef};
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
            t.duplicate(h, Rights(frame.rsi as u32), |cap, rights| {
                Cap::new(cap.object().clone(), rights)
            })
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
            insert(t, ep, rights).map(|h| h.0 as u64)
        }),
        nr::ENDPOINT_CREATE_PAIR => with_handles(endpoint_create_pair),
        nr::ENDPOINT_CLOSE => endpoint_close(h),
        nr::IRQ_WAIT => irq_wait(h, frame.rsi),
        nr::MMIO_MAP => mmio_map(h),
        nr::MMIO_UNMAP => process::unmap_foreign(frame.rdi).map(|()| 0),
        nr::REGION_LEN => with_handles(|t| match lookup(t, h, Rights::READ)?.kind {
            ObjectKind::Mmio(ref r) => Ok(r.len),
            ObjectKind::Dma(ref d) => Ok(d.region.len),
            _ => Err(Error::Invalid),
        }),
        nr::DMA_ALLOC => dma_alloc(h, frame.rsi),
        nr::DMA_PHYS => with_handles(|t| match lookup(t, h, Rights::READ)?.kind {
            ObjectKind::Dma(ref d) => Ok(d.region.phys),
            _ => Err(Error::Invalid),
        }),
        nr::PROCESS_SPAWN => process_spawn(h, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9),
        nr::PROCESS_SPAWN_FROM => process_spawn_from(h, frame.rsi, frame.rdx, frame.r10, frame.r8),
        nr::PCI_COUNT => with_handles(|t| {
            require_pci_root(t, h, Rights::READ)?;
            Ok(crate::pci::count() as u64)
        }),
        nr::PCI_INFO => with_handles(|t| {
            require_pci_root(t, h, Rights::READ)?;
            crate::pci::info_packed(frame.rsi as usize).ok_or(Error::NotFound)
        }),
        nr::PCI_INFO_EXT => with_handles(|t| {
            require_pci_root(t, h, Rights::READ)?;
            crate::pci::info_ext_packed(frame.rsi as usize).ok_or(Error::NotFound)
        }),
        nr::PCI_CLAIM => pci_claim(h, frame.rsi as usize, frame.rdx),
        nr::PROCESS_WAIT => process_wait(h, frame.rsi),
        nr::IPC_SEND => ipc_send(h, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9),
        nr::IPC_RECEIVE => ipc_receive(h, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9),
        nr::IPC_RECEIVE_ANY => ipc_receive_any(
            frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
        ),
        nr::PROCESS_KILL => process_kill(h),
        nr::CONSOLE_READ => console_read(h, frame.rsi, frame.rdx, frame.r10),
        nr::CONSOLE_INJECT => console_inject(h, frame.rsi, frame.rdx),
        nr::EVENT_RECORD => event_record(h, frame.rsi, frame.rdx),
        nr::SYSTEM_INFO => system_info(h, frame.rsi, frame.rdx, frame.r10),
        nr::SYSTEM_POWER => system_power(h, frame.rsi),
        nr::INITRD_READ => initrd_read(h, frame.rsi, frame.rdx, frame.r10, frame.r8),
        n => {
            warn!("unknown syscall {n} from rip={:#x}", frame.user_rip);
            Err(Error::NoSys)
        }
    };
    // Endpoints whose last capability of a side this syscall dropped
    // (ADR 0030): their queues and waiters are settled with no lock held.
    crate::ipc::settle_closed();
    // A termination requested by another process runs here, with the
    // syscall already complete and no locks held (audit 6ee19fb, finding 4).
    if process::current_kill_requested() {
        process::kill_current(process::KILLED_BY_SUPERVISOR);
    }
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
    let console = with_handles(|t| lookup(t, handle, Rights::WRITE).cloned())?;
    if !matches!(
        console.kind,
        ObjectKind::Console | ObjectKind::SessionConsole(_)
    ) {
        return Err(Error::Invalid);
    }
    if len > MAX {
        return Err(Error::Invalid);
    }
    let bytes = process::copy_from_user(ptr, len)?;
    let text = core::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
    match &console.kind {
        ObjectKind::SessionConsole(output) => session_console_write(output, text.as_bytes())?,
        _ => process::console_write_current(text),
    }
    Ok(len)
}

/// How long a session console write waits for room in the session's queue,
/// per message (the session shell's own writes wait as long).
const SESSION_CONSOLE_WAIT_MS: u64 = 2000;

/// Text for a session console goes to its endpoint in messages of at most
/// `IPC_MAX_DATA` bytes and never as an empty message, which the session
/// reader takes as the end of the session (ADR 0047).
fn session_console_write(output: &Cap, text: &[u8]) -> Result<(), Error> {
    let endpoint = output.object();
    if endpoint.is_revoked() {
        return Err(Error::Revoked);
    }
    let ObjectKind::Endpoint(endpoint) = &endpoint.kind else {
        // `spawn_from_bytes` only wraps endpoints.
        return Err(Error::Invalid);
    };
    for chunk in text.chunks(IPC_MAX_DATA) {
        endpoint.send_with(SESSION_CONSOLE_WAIT_MS, || {
            Ok(Message {
                data: chunk.to_vec(),
                handles: Vec::new(),
            })
        })?;
    }
    Ok(())
}

fn console_read(handle: Handle, ptr: u64, len: u64, timeout_ms: u64) -> Result<u64, Error> {
    const MAX: u64 = 4096;
    let is_console = with_handles(|t| {
        lookup(t, handle, Rights::READ).map(|o| matches!(o.kind, ObjectKind::Console))
    })?;
    if !is_console || len == 0 || len > MAX {
        return Err(Error::Invalid);
    }
    // Validate the destination before blocking: an invalid pointer fails right away.
    process::copy_to_user(ptr, &[])?;
    // The prompt (unterminated line) must be visible before waiting for input.
    if let Some(pid) = process::current_pid() {
        process::console_flush(pid);
    }
    let mut buf = alloc::vec![0u8; len as usize];
    let n = crate::console_input::read(&mut buf, deadline_from(timeout_ms));
    if n == 0 {
        return Err(Error::Timeout);
    }
    process::copy_to_user(ptr, &buf[..n])?;
    Ok(n as u64)
}

/// Text from a user-space keyboard into the console ring (`MANAGE` on a
/// `Console`; ADR 0013). Returns the bytes that fit.
fn console_inject(handle: Handle, ptr: u64, len: u64) -> Result<u64, Error> {
    const MAX: u64 = 1024;
    let is_console = with_handles(|t| {
        lookup(t, handle, Rights::MANAGE).map(|o| matches!(o.kind, ObjectKind::Console))
    })?;
    if !is_console || len == 0 || len > MAX {
        return Err(Error::Invalid);
    }
    let bytes = process::copy_from_user(ptr, len)?;
    Ok(crate::console_input::inject(&bytes) as u64)
}

/// Copies an initrd file into the user buffer (`READ` on the `SPAWNER`).
fn initrd_read(h: Handle, name_ptr: u64, name_len: u64, buf: u64, len: u64) -> Result<u64, Error> {
    if name_len > 64 {
        return Err(Error::TooBig);
    }
    let is_spawner = with_handles(|t| {
        lookup(t, h, Rights::READ).map(|o| matches!(o.kind, ObjectKind::Spawner))
    })?;
    if !is_spawner {
        return Err(Error::Invalid);
    }
    let raw = process::copy_from_user(name_ptr, name_len)?;
    let name = core::str::from_utf8(&raw).map_err(|_| Error::Invalid)?;
    let data = crate::user::programs::by_name(name).ok_or(Error::NotFound)?;
    if data.len() as u64 > len {
        return Err(Error::TooBig);
    }
    process::copy_to_user(buf, data)?;
    Ok(data.len() as u64)
}

/// Checks that `h` is the `System` object with `required`.
fn system_object(h: Handle, required: Rights) -> Result<(), Error> {
    let ok =
        with_handles(|t| lookup(t, h, required).map(|o| matches!(o.kind, ObjectKind::System)))?;
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}

fn system_info(h: Handle, kind: u64, ptr: u64, len: u64) -> Result<u64, Error> {
    use aricode_abi::sysinfo;
    system_object(h, Rights::READ)?;
    let bytes: Vec<u8> = match kind {
        sysinfo::UPTIME => crate::time::uptime_ms().to_le_bytes().to_vec(),
        sysinfo::UPTIME_US => crate::time::uptime_us().to_le_bytes().to_vec(),
        sysinfo::WALLCLOCK => crate::rtc::unix_now().to_le_bytes().to_vec(),
        sysinfo::CMDLINE => crate::boot_args::get().into_bytes(),
        sysinfo::EVENTS => {
            let max = (len as usize) / sysinfo::EventEntry::LEN;
            crate::events::recent(max)
                .iter()
                .flat_map(|e| e.encode())
                .collect()
        }
        sysinfo::CPUS => (crate::cpu::online_count() as u64).to_le_bytes().to_vec(),
        sysinfo::IDLE => {
            let sample = crate::task::scheduler::idle_sample();
            let mut v = sample.elapsed.to_le_bytes().to_vec();
            v.extend_from_slice(&(sample.idle.len() as u64).to_le_bytes());
            for idle in sample.idle {
                v.extend_from_slice(&idle.to_le_bytes());
            }
            v
        }
        sysinfo::IOMMU => {
            let (units, domains, faults) = crate::iommu::stats();
            let mut v = units.to_le_bytes().to_vec();
            v.extend_from_slice(&domains.to_le_bytes());
            v.extend_from_slice(&faults.to_le_bytes());
            v
        }
        sysinfo::MEMORY => {
            let s = crate::memory::pmm::stats();
            let mut v = (s.usable_frames as u64).to_le_bytes().to_vec();
            v.extend_from_slice(&(s.free_frames as u64).to_le_bytes());
            v
        }
        sysinfo::RANDOM => {
            let n = len.min(sysinfo::RANDOM_MAX) as usize;
            let mut v = alloc::vec![0u8; n];
            crate::random::fill(&mut v);
            v
        }
        sysinfo::RANDOM_SOURCE => crate::random::source().encode().to_vec(),
        sysinfo::FRAMEBUFFER => match crate::framebuffer::abi_info() {
            Some(fb) => fb.encode().to_vec(),
            None => return Err(Error::NotFound),
        },
        sysinfo::PROCESSES => {
            let max = (len as usize) / sysinfo::ProcessEntry::LEN;
            process::snapshot()
                .iter()
                .take(max)
                .flat_map(|e| e.encode())
                .collect()
        }
        sysinfo::STACKS => {
            let max = (len as usize) / sysinfo::StackEntry::LEN;
            process::stack_snapshot()
                .iter()
                .take(max)
                .flat_map(|e| e.encode())
                .collect()
        }
        _ => return Err(Error::Invalid),
    };
    if bytes.len() as u64 > len {
        return Err(Error::TooBig);
    }
    process::copy_to_user(ptr, &bytes)?;
    Ok(bytes.len() as u64)
}

fn system_power(h: Handle, action: u64) -> Result<u64, Error> {
    use aricode_abi::power;
    system_object(h, Rights::MANAGE)?;
    let who = process::current_pid().map(|p| p.0).unwrap_or(0);
    match action {
        power::SHUTDOWN => {
            crate::println!("AriCodeOS: shutting down (requested by process {who})");
            crate::shutdown()
        }
        power::REBOOT => {
            crate::println!("AriCodeOS: rebooting (requested by process {who})");
            crate::arch::x86_64::power::reboot()
        }
        _ => Err(Error::Invalid),
    }
}

/// `endpoint_create_pair` (ADR 0030): the receive side and the send side of
/// a new endpoint in two handles, so the creator can give the send side
/// away and keep none of it (a reply endpoint learns that the server is
/// gone when its last send capability is).
fn endpoint_create_pair(t: &mut crate::object::Handles) -> Result<u64, Error> {
    let ep = KernelObject::new(ObjectKind::Endpoint(crate::ipc::Endpoint::new()));
    let receive = insert(
        t,
        ep.clone(),
        Rights::READ
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER)
            .union(Rights::MANAGE),
    )?;
    let send = Rights::WRITE
        .union(Rights::DUPLICATE)
        .union(Rights::TRANSFER);
    match insert(t, ep, send) {
        Ok(s) => {
            let packed = receive.0 as u64 | (s.0 as u64) << 32;
            // `dispatch` returns the value as an i64, and user space reads
            // -4095..=-1 as an error. A handle is `generation << 16 |
            // (index + 1)`, so the value is negative whenever the send
            // handle's generation is >= 0x8000; it is in the error range
            // only if the send handle is 0xFFFF_FFFF and the receive handle
            // is >= 0xFFFF_F001, i.e. index + 1 >= 0xF001 in both, far above
            // `HANDLE_LIMIT` (256). Asserted, not assumed.
            debug_assert!(aricode_abi::decode(packed as i64).is_ok());
            Ok(packed)
        }
        Err(e) => {
            let _ = t.remove(receive);
            Err(e)
        }
    }
}

/// `endpoint_close` (ADR 0030): with `MANAGE`, closes both sides of the
/// endpoint for every holder, without invalidating the handles (unlike
/// revocation, they can still be duplicated and passed on). A supervisor
/// that keeps the receive side for the next generation of a service uses
/// it when it gives the service up.
fn endpoint_close(h: Handle) -> Result<u64, Error> {
    let obj = endpoint_of(h, Rights::MANAGE)?;
    let ObjectKind::Endpoint(endpoint) = &obj.kind else {
        unreachable!()
    };
    if endpoint.shut_down() {
        crate::ipc::settle_later(obj.clone());
    }
    Ok(0)
}

/// Endpoint behind a handle with `required`, or `Invalid` if the object is not an endpoint.
fn endpoint_of(h: Handle, required: Rights) -> Result<ObjectRef, Error> {
    with_handles(|t| {
        let obj = lookup(t, h, required)?;
        match obj.kind {
            ObjectKind::Endpoint(_) => Ok(obj.clone()),
            _ => Err(Error::Invalid),
        }
    })
}

/// A handle listed twice would be taken once and then fail, dropping the
/// capability already taken (audit robustness-1, L3): refused up front.
fn distinct(handles: &[Handle]) -> Result<(), Error> {
    if handles
        .iter()
        .enumerate()
        .any(|(i, h)| handles[..i].contains(h))
    {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
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
    // Check now that all are transferable (early error) and only remove them
    // once the endpoint confirms the slot: a timeout loses nothing.
    distinct(&user_handles)?;
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
        let handles: Vec<(Cap, Rights)> = with_handles(|t| {
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
    // Check the buffers before consuming a message.
    process::copy_to_user(buf_ptr, &[])?;
    let ObjectKind::Endpoint(endpoint) = &obj.kind else {
        unreachable!()
    };
    let msg = endpoint.receive(timeout_ms)?;
    let n = msg.data.len().min(buf_len as usize);
    process::copy_to_user(buf_ptr, &msg.data[..n])?;
    // Insert the received capabilities; those that do not fit are dropped.
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

/// A member of an `ipc_receive_any` set (ADR 0044): an interrupt line the
/// caller may wait on (`WAIT`), or else an endpoint it may receive from,
/// checked as `ipc_receive` checks it (`READ`, then `Invalid` for any other
/// object), so the errors of an endpoint-only set are those of ABI 5.
fn wait_member_of(h: Handle) -> Result<ObjectRef, Error> {
    // One look at the table: the kind and the rights of the same entry,
    // even if another thread of the process closes and reuses the handle.
    with_handles(|t| {
        let required = match lookup(t, h, Rights::NONE)?.kind {
            ObjectKind::Irq(_) => Rights::WAIT,
            _ => Rights::READ,
        };
        let obj = lookup(t, h, required)?;
        match obj.kind {
            ObjectKind::Irq(_) | ObjectKind::Endpoint(_) => Ok(obj.clone()),
            _ => Err(Error::Invalid),
        }
    })
}

/// `ipc_receive_any` (ADR 0020, ADR 0044): like `ipc_receive` on the first
/// of several endpoints that has a message, or the interrupts of the first
/// line in the set that fired; the index goes in bits 48..63.
fn ipc_receive_any(
    handles_ptr: u64,
    count: u64,
    buf_ptr: u64,
    buf_len: u64,
    out_ptr: u64,
    timeout_ms: u64,
) -> Result<u64, Error> {
    use crate::ipc::{Member, Ready};
    use aricode_abi::{receive_any, IPC_MAX_ANY, IPC_MAX_ANY_IRQS};
    if count == 0 || count as usize > IPC_MAX_ANY + IPC_MAX_ANY_IRQS {
        return Err(Error::Invalid);
    }
    if buf_len as usize > IPC_MAX_DATA {
        return Err(Error::TooBig);
    }
    let raw = process::copy_from_user(handles_ptr, count * 4)?;
    // All validated before waiting; the Arcs keep the objects alive.
    let objs: Vec<ObjectRef> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| wait_member_of(Handle(u32::from_le_bytes(*c))))
        .collect::<Result<_, _>>()?;
    let members: Vec<Member> = objs
        .iter()
        .map(|o| match &o.kind {
            ObjectKind::Endpoint(e) => Member::Endpoint(e),
            ObjectKind::Irq(l) => Member::Irq(l),
            _ => unreachable!(),
        })
        .collect();
    let irqs = members
        .iter()
        .filter(|m| matches!(m, Member::Irq(_)))
        .count();
    if irqs > IPC_MAX_ANY_IRQS || members.len() - irqs > IPC_MAX_ANY {
        return Err(Error::Invalid);
    }
    process::copy_to_user(buf_ptr, &[])?;
    let (index, msg) = match crate::ipc::receive_any(&members, timeout_ms)? {
        (index, Ready::Irq(n)) => return Ok(receive_any::irq(index, n)),
        (index, Ready::Message(msg)) => (index, msg),
    };
    let n = msg.data.len().min(buf_len as usize);
    process::copy_to_user(buf_ptr, &msg.data[..n])?;
    let received: Vec<u32> = with_handles(|t| {
        Ok(msg
            .handles
            .into_iter()
            .take(IPC_MAX_HANDLES)
            .filter_map(|(o, r)| t.insert(o, r).ok())
            .map(|h| h.0)
            .collect())
    })?;
    let raw: Vec<u8> = received.iter().flat_map(|h| h.to_le_bytes()).collect();
    process::copy_to_user(out_ptr, &raw)?;
    Ok(receive_any::message(index, n, received.len()))
}

fn process_spawn(
    spawner: Handle,
    name_ptr: u64,
    name_len: u64,
    arg: u64,
    handles_ptr: u64,
    limits: u64,
) -> Result<u64, Error> {
    if name_len > 64 {
        return Err(Error::TooBig);
    }
    require_spawner(spawner)?;
    let raw = process::copy_from_user(name_ptr, name_len)?;
    let name = core::str::from_utf8(&raw).map_err(|_| Error::Invalid)?;
    let elf = crate::user::programs::by_name(name).ok_or(Error::NotFound)?;
    spawn_from_bytes(name, elf, arg, handles_ptr, limits)
}

/// `process_spawn_from` (ADR 0017): like `process_spawn`, with the ELF in a
/// DMA buffer of the caller (`spawn_from::Request` in its memory). The
/// buffer is read through the physical map while the segments are copied;
/// afterwards the new process does not depend on it.
fn process_spawn_from(
    spawner: Handle,
    req_ptr: u64,
    arg: u64,
    handles_ptr: u64,
    limits: u64,
) -> Result<u64, Error> {
    use aricode_abi::spawn_from;
    require_spawner(spawner)?;
    let raw = process::copy_from_user(req_ptr, spawn_from::LEN as u64)?;
    let req = spawn_from::Request::decode(&raw).ok_or(Error::Invalid)?;
    if req.name_len == 0 {
        return Err(Error::Invalid);
    }
    let dma: ObjectRef = with_handles(|t| lookup(t, Handle(req.dma), Rights::READ).cloned())?;
    let (phys, len) = match dma.kind {
        ObjectKind::Dma(ref d) => (d.region.phys, d.region.len),
        _ => return Err(Error::Invalid),
    };
    let end = req.elf_offset as u64 + req.elf_len as u64;
    if req.elf_len == 0 || end > len {
        return Err(Error::TooBig);
    }
    // SAFETY: physical memory of the DMA buffer, alive while `dma` (Arc) lives
    // and reachable through the linear map; only read, with bounds checks
    // in the ELF parser.
    let elf = unsafe {
        core::slice::from_raw_parts(
            crate::memory::phys::phys_to_virt(phys + req.elf_offset as u64) as *const u8,
            req.elf_len as usize,
        )
    };
    spawn_from_bytes(req.name_str(), elf, arg, handles_ptr, limits)
}

fn require_spawner(spawner: Handle) -> Result<(), Error> {
    let is_spawner = with_handles(|t| {
        lookup(t, spawner, Rights::MANAGE).map(|o| matches!(o.kind, ObjectKind::Spawner))
    })?;
    if is_spawner {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}

/// Common part of `process_spawn` and `process_spawn_from`: removes the
/// transferred handles, creates the process and returns its handle.
fn spawn_from_bytes(
    name: &str,
    elf: &[u8],
    arg: u64,
    handles_ptr: u64,
    limits: u64,
) -> Result<u64, Error> {
    let handles_len = aricode_abi::spawn_limits::handles(limits) as u64;
    let dma_quota = aricode_abi::spawn_limits::memory_kib(limits) as u64 * 1024;
    let console_first = aricode_abi::spawn_limits::console_first(limits);
    if handles_len as usize > IPC_MAX_HANDLES * 4 {
        return Err(Error::TooBig);
    }
    if console_first && handles_len == 0 {
        return Err(Error::Invalid);
    }
    let raw_handles = process::copy_from_user(handles_ptr, handles_len * 4)?;
    let user_handles: Vec<Handle> = raw_handles
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| Handle(u32::from_le_bytes(*c)))
        .collect();
    // Validate all before removing any.
    distinct(&user_handles)?;
    let mut extra: Vec<(Cap, Rights)> = with_handles(|t| {
        for uh in &user_handles {
            lookup(t, *uh, Rights::TRANSFER)?;
        }
        // A session console wraps an endpoint (`Invalid` otherwise) that it
        // can write to (`Denied` for a side without `WRITE`) (ADR 0047).
        if console_first {
            let first = lookup(t, user_handles[0], Rights::TRANSFER)?;
            if !matches!(first.kind, ObjectKind::Endpoint(_)) {
                return Err(Error::Invalid);
            }
            lookup(t, user_handles[0], Rights::WRITE)?;
        }
        user_handles
            .iter()
            .map(|uh| t.take(*uh).map_err(abi_error))
            .collect()
    })?;
    let session_console = console_first.then(|| extra.remove(0).0);
    let pid = match process::spawn_with_caps(name, elf, session_console, extra, arg, dma_quota) {
        Ok(pid) => pid,
        Err(e) => {
            warn!("process_spawn '{name}': {e}");
            return Err(Error::Limit);
        }
    };
    let rights = Rights::READ
        .union(Rights::WAIT)
        .union(Rights::MANAGE)
        .union(Rights::DUPLICATE);
    with_handles(|t| {
        insert(t, KernelObject::new(ObjectKind::Process(pid)), rights).map(|h| h.0 as u64)
    })
}

fn require_pci_root(t: &crate::object::Handles, h: Handle, required: Rights) -> Result<(), Error> {
    match lookup(t, h, required)?.kind {
        ObjectKind::PciRoot => Ok(()),
        _ => Err(Error::Invalid),
    }
}

fn pci_claim(root: Handle, index: usize, out_ptr: u64) -> Result<u64, Error> {
    with_handles(|t| require_pci_root(t, root, Rights::MANAGE))?;
    process::copy_to_user(out_ptr, &[0u8; aricode_abi::PCI_CLAIM_HANDLES * 4])?;
    let claimed = crate::pci::claim(index).map_err(|e| {
        warn!("pci_claim {index}: {e}");
        Error::NotFound
    })?;
    let raw: Vec<u8> = with_handles(|t| {
        let mut out = Vec::with_capacity(aricode_abi::PCI_CLAIM_HANDLES * 4);
        for slot in claimed.handles {
            let value = match slot {
                Some((obj, rights)) => insert(t, obj, rights)?.0,
                None => 0,
            };
            out.extend_from_slice(&value.to_le_bytes());
        }
        Ok(out)
    })?;
    process::copy_to_user(out_ptr, &raw)?;
    Ok(aricode_abi::PCI_CLAIM_HANDLES as u64)
}

/// `process_kill` (audit 6ee19fb, finding 4): requires `MANAGE` on the
/// process; flags it for termination and wakes its thread.
fn process_kill(h: Handle) -> Result<u64, Error> {
    let pid = with_handles(|t| match lookup(t, h, Rights::MANAGE)?.kind {
        ObjectKind::Process(p) => Ok(p),
        _ => Err(Error::Invalid),
    })?;
    if process::request_kill(pid) {
        Ok(0)
    } else {
        Err(Error::BadHandle)
    }
}

fn process_wait(h: Handle, timeout_ms: u64) -> Result<u64, Error> {
    let pid = with_handles(|t| match lookup(t, h, Rights::WAIT)?.kind {
        ObjectKind::Process(p) => Ok(p),
        _ => Err(Error::Invalid),
    })?;
    match process::wait_until(pid, deadline_from(timeout_ms)) {
        Some(process::ExitStatus::Exited(code)) => Ok(code),
        Some(process::ExitStatus::Killed(_)) => Err(Error::Killed),
        None if process::exists(pid) => Err(Error::Timeout),
        None => Err(Error::BadHandle),
    }
}

fn deadline_from(timeout_ms: u64) -> Option<u64> {
    if timeout_ms == aricode_abi::TIMEOUT_INFINITE {
        None
    } else {
        Some(crate::time::ticks() + timeout_ms.div_ceil(crate::time::MS_PER_TICK))
    }
}

fn irq_wait(h: Handle, timeout_ms: u64) -> Result<u64, Error> {
    let obj = with_handles(|t| {
        let o = lookup(t, h, Rights::WAIT)?;
        match o.kind {
            ObjectKind::Irq(_) => Ok(o.clone()),
            _ => Err(Error::Invalid),
        }
    })?;
    let ObjectKind::Irq(line) = &obj.kind else {
        unreachable!()
    };
    line.wait(deadline_from(timeout_ms))
        .map(|n| n as u64)
        .ok_or(Error::Timeout)
}

fn mmio_map(h: Handle) -> Result<u64, Error> {
    let (phys, len, cacheable, source_id, dma) = with_handles(|t| {
        let o = lookup(t, h, Rights::READ.union(Rights::WRITE))?;
        match o.kind {
            ObjectKind::Mmio(ref r) => Ok((r.phys, r.len, r.cacheable, r.source_id, None)),
            // The mapping holds the buffer (ADR 0038).
            ObjectKind::Dma(ref d) => Ok((
                d.region.phys,
                d.region.len,
                d.region.cacheable,
                None,
                Some(o.clone()),
            )),
            _ => Err(Error::Invalid),
        }
    })?;
    // The buffer stays referenced here until the IOMMU has it, whatever
    // happens to the handle meanwhile (ADR 0038).
    let held = dma.clone();
    let (virt, created) = process::map_foreign(phys, len, cacheable, dma)?;
    // IOMMU (Phase 33): mapping a device assigns it to the process's
    // domain; mapping a foreign DMA buffer makes it visible to its devices.
    if let Some(pid) = process::current_pid() {
        let r = match source_id {
            Some(sid) => crate::iommu::attach(pid.0, sid),
            None if held.is_some() => crate::iommu::map_region(pid.0, phys, len),
            None => Ok(()),
        };
        if let Err(e) = r {
            warn!("mmio_map: {e}");
            // The caller never learns the address: a mapping made by this
            // call goes again (with its reference and quota charge).
            if created {
                let _ = process::unmap_foreign(virt);
            }
            return Err(Error::Limit);
        }
    }
    drop(held);
    Ok(virt)
}

/// `event_record(code, name_ptr, name_len)` (ADR 0028): a `Fault` event
/// for the calling process. `h` carries the reason code (the first
/// argument slot), `name` is at most `EVENT_NAME_LEN` bytes.
fn event_record(code: Handle, name_ptr: u64, name_len: u64) -> Result<u64, Error> {
    if name_len == 0 || name_len > aricode_abi::sysinfo::EVENT_NAME_LEN as u64 {
        return Err(Error::Invalid);
    }
    let raw = process::copy_from_user(name_ptr, name_len)?;
    let name = core::str::from_utf8(&raw).map_err(|_| Error::Invalid)?;
    let pid = process::current_pid().map_or(0, |p| p.0);
    crate::events::record(
        aricode_abi::sysinfo::EventKind::Fault,
        pid,
        code.0 as u64,
        name,
    );
    Ok(0)
}

fn dma_alloc(h: Handle, len: u64) -> Result<u64, Error> {
    let is_hw = with_handles(|t| {
        lookup(t, h, Rights::MANAGE).map(|o| matches!(o.kind, ObjectKind::Hardware))
    })?;
    if !is_hw {
        return Err(Error::Invalid);
    }
    if len == 0 || len > aricode_abi::DMA_MAX_LEN {
        return Err(Error::TooBig);
    }
    // Process quota (set by its launcher): the size rounded up to pages is
    // charged, since that is what gets allocated.
    if !process::charge_dma(len.next_multiple_of(4096)) {
        crate::events::record(
            aricode_abi::sysinfo::EventKind::DmaQuota,
            process::current_pid().map_or(0, |p| p.0),
            len,
            "dma_alloc",
        );
        return Err(Error::Limit);
    }
    let dma = crate::hw::dma_alloc(len).map_err(|_| Error::Limit)?;
    if let (Some(pid), ObjectKind::Dma(d)) = (process::current_pid(), &dma.kind) {
        if let Err(e) = crate::iommu::map_region(pid.0, d.region.phys, d.region.len) {
            warn!("dma_alloc: {e}");
            return Err(Error::Limit);
        }
    }
    let rights = Rights::READ
        .union(Rights::WRITE)
        .union(Rights::DUPLICATE)
        .union(Rights::TRANSFER);
    with_handles(|t| insert(t, dma, rights).map(|h| h.0 as u64))
}
