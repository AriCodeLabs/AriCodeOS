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
        nr::IRQ_WAIT => irq_wait(h, frame.rsi),
        nr::MMIO_MAP => mmio_map(h),
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
        nr::CONSOLE_READ => console_read(h, frame.rsi, frame.rdx, frame.r10),
        nr::CONSOLE_INJECT => console_inject(h, frame.rsi, frame.rdx),
        nr::SYSTEM_INFO => system_info(h, frame.rsi, frame.rdx, frame.r10),
        nr::SYSTEM_POWER => system_power(h, frame.rsi),
        nr::INITRD_READ => initrd_read(h, frame.rsi, frame.rdx, frame.r10, frame.r8),
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
    process::console_write_current(text);
    Ok(len)
}

fn console_read(handle: Handle, ptr: u64, len: u64, timeout_ms: u64) -> Result<u64, Error> {
    const MAX: u64 = 4096;
    let is_console = with_handles(|t| {
        lookup(t, handle, Rights::READ).map(|o| matches!(o.kind, ObjectKind::Console))
    })?;
    if !is_console || len == 0 || len > MAX {
        return Err(Error::Invalid);
    }
    // Validar el destino antes de bloquear: un puntero inválido falla ya.
    process::copy_to_user(ptr, &[])?;
    // El prompt (línea sin terminar) debe verse antes de esperar la entrada.
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

/// Texto de un teclado de usuario al anillo de la consola (`MANAGE` sobre un
/// `Console`; ADR 0013). Devuelve los bytes que cupieron.
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

/// Copia un fichero del initrd al buffer del usuario (`READ` sobre el `SPAWNER`).
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

/// Comprueba que `h` es el objeto `System` con `required`.
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
        sysinfo::CPUS => (crate::cpu::online_count() as u64).to_le_bytes().to_vec(),
        sysinfo::IDLE => {
            let mut v = crate::time::tsc_since_boot().to_le_bytes().to_vec();
            v.extend_from_slice(&(crate::cpu::online_count() as u64).to_le_bytes());
            for c in crate::cpu::online() {
                v.extend_from_slice(&c.idle_cycles().to_le_bytes());
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
            crate::println!("AriCodeOS: apagando el sistema (pedido por el proceso {who})");
            crate::shutdown()
        }
        power::REBOOT => {
            crate::println!("AriCodeOS: reiniciando (pedido por el proceso {who})");
            crate::arch::x86_64::power::reboot()
        }
        _ => Err(Error::Invalid),
    }
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

/// `ipc_receive_any` (ADR 0020): como `ipc_receive` sobre el primero de
/// varios endpoints con mensaje; el índice va en los bits 48..63.
fn ipc_receive_any(
    handles_ptr: u64,
    count: u64,
    buf_ptr: u64,
    buf_len: u64,
    out_ptr: u64,
    timeout_ms: u64,
) -> Result<u64, Error> {
    if count == 0 || count as usize > aricode_abi::IPC_MAX_ANY {
        return Err(Error::Invalid);
    }
    if buf_len as usize > IPC_MAX_DATA {
        return Err(Error::TooBig);
    }
    let raw = process::copy_from_user(handles_ptr, count * 4)?;
    // Todos validados antes de esperar; los Arc mantienen vivos los objetos.
    let objs: Vec<ObjectRef> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| endpoint_of(Handle(u32::from_le_bytes(*c)), Rights::READ))
        .collect::<Result<_, _>>()?;
    process::copy_to_user(buf_ptr, &[])?;
    let endpoints: Vec<&crate::ipc::Endpoint> = objs
        .iter()
        .map(|o| match &o.kind {
            ObjectKind::Endpoint(e) => e,
            _ => unreachable!(),
        })
        .collect();
    let (index, msg) = crate::ipc::receive_any(&endpoints, timeout_ms)?;
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
    Ok(n as u64 | ((received.len() as u64) << 32) | ((index as u64) << 48))
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

/// `process_spawn_from` (ADR 0017): como `process_spawn`, con el ELF en un
/// buffer DMA del llamador (`spawn_from::Request` en su memoria). El buffer
/// se lee por el mapa físico mientras se copian los segmentos; después el
/// proceso nuevo no depende de él.
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
    // SAFETY: memoria física del buffer DMA, viva mientras `dma` (Arc) viva y
    // accesible por el mapa lineal; solo se lee, con comprobación de límites
    // en el analizador ELF.
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

/// Parte común de `process_spawn` y `process_spawn_from`: retira los handles
/// transferidos, crea el proceso y devuelve su handle.
fn spawn_from_bytes(
    name: &str,
    elf: &[u8],
    arg: u64,
    handles_ptr: u64,
    limits: u64,
) -> Result<u64, Error> {
    let handles_len = aricode_abi::spawn_limits::handles(limits) as u64;
    let dma_quota = aricode_abi::spawn_limits::memory_kib(limits) as u64 * 1024;
    if handles_len as usize > IPC_MAX_HANDLES * 4 {
        return Err(Error::TooBig);
    }
    let raw_handles = process::copy_from_user(handles_ptr, handles_len * 4)?;
    let user_handles: Vec<Handle> = raw_handles
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| Handle(u32::from_le_bytes(*c)))
        .collect();
    // Validar todos antes de retirar ninguno.
    let extra: Vec<(ObjectRef, Rights)> = with_handles(|t| {
        for uh in &user_handles {
            lookup(t, *uh, Rights::TRANSFER)?;
        }
        user_handles
            .iter()
            .map(|uh| t.take(*uh).map_err(abi_error))
            .collect()
    })?;
    let pid = match process::spawn_with_handles_and_arg(name, elf, extra, arg, dma_quota) {
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
        t.insert(KernelObject::new(ObjectKind::Process(pid)), rights)
            .map(|h| h.0 as u64)
            .map_err(abi_error)
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
                Some((obj, rights)) => t.insert(obj, rights).map_err(abi_error)?.0,
                None => 0,
            };
            out.extend_from_slice(&value.to_le_bytes());
        }
        Ok(out)
    })?;
    process::copy_to_user(out_ptr, &raw)?;
    Ok(aricode_abi::PCI_CLAIM_HANDLES as u64)
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
    let (phys, len, cacheable, source_id, is_dma) =
        with_handles(
            |t| match lookup(t, h, Rights::READ.union(Rights::WRITE))?.kind {
                ObjectKind::Mmio(ref r) => Ok((r.phys, r.len, r.cacheable, r.source_id, false)),
                ObjectKind::Dma(ref d) => {
                    Ok((d.region.phys, d.region.len, d.region.cacheable, None, true))
                }
                _ => Err(Error::Invalid),
            },
        )?;
    let virt = process::map_foreign(phys, len, cacheable)?;
    // IOMMU (Fase 33): mapear un dispositivo lo asigna al dominio del
    // proceso; mapear un buffer DMA ajeno lo hace visible a sus dispositivos.
    if let Some(pid) = process::current_pid() {
        let r = match source_id {
            Some(sid) => crate::iommu::attach(pid.0, sid),
            None if is_dma => crate::iommu::map_region(pid.0, phys, len),
            None => Ok(()),
        };
        if let Err(e) = r {
            warn!("mmio_map: {e}");
            return Err(Error::Limit);
        }
    }
    Ok(virt)
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
    // Cuota del proceso (fijada por quien lo lanzó): se cobra el tamaño
    // redondeado a páginas, que es lo que se reserva.
    if !process::charge_dma(len.next_multiple_of(4096)) {
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
    with_handles(|t| t.insert(dma, rights).map(|h| h.0 as u64).map_err(abi_error))
}
