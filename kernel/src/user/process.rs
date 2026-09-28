//! Procesos: espacio de direcciones propio (mitad del kernel compartida),
//! un hilo principal que entra en ring 3 y un estado de salida.

use crate::arch::x86_64::usermode;
use crate::memory::{phys, pmm, vmm};
use crate::sync::IrqLock;
use crate::task::scheduler;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_abi::USER_SPACE_END;
use aricode_pmm::Frame;
use aricode_x86_paging::{Mapper, OffsetMap, NO_EXECUTE, PAGE_SIZE, USER, WRITABLE};
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub u32);

/// Base mínima del código de usuario (los ELF se enlazan en 0x40_0000) y pila.
pub const USER_CODE_BASE: u64 = 0x40_0000;

/// `true` si `addr` cae en la guard page bajo la pila de usuario.
pub fn in_user_stack_guard(addr: u64) -> bool {
    let bottom = USER_STACK_TOP - USER_STACK_SIZE;
    (bottom - PAGE_SIZE..bottom).contains(&addr)
}
pub const USER_STACK_TOP: u64 = 0x80_0000;
pub const USER_STACK_SIZE: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Exited(u64),
    /// Terminado por el kernel: excepción en ring 3.
    Killed(&'static str),
}

pub struct Process {
    pub name: String,
    pub handles: crate::object::Handles,
    /// Argumento que recibe `_start` en `rdi`.
    pub arg: u64,
    /// Despierta a quien espera la salida.
    pub exit_wait: alloc::sync::Arc<crate::task::wait::WaitQueue>,
    /// Siguiente dirección libre para mapeos MMIO/DMA.
    mmio_next: u64,
    /// Rangos virtuales que apuntan a memoria ajena al proceso (MMIO, DMA):
    /// se desmapean sin liberar al destruir el espacio.
    foreign: Vec<(u64, u64)>,
    /// `None` una vez liberado el espacio de direcciones.
    mapper: Option<Mapper<vmm::PmmFrames, OffsetMap>>,
    pub entry: u64,
    pub status: Option<ExitStatus>,
    /// Límite de memoria DMA (bytes) que el proceso puede reservar en total;
    /// 0 = sin límite. Lo fija quien lo crea (`spawn_limits`).
    pub dma_quota: u64,
    pub dma_used: u64,
    /// Salida de consola pendiente de una línea completa (Fase 32): con
    /// varias CPUs, dos procesos que imprimen una línea en varios trozos se
    /// mezclarían; el kernel emite cada línea entera.
    console_line: Vec<u8>,
    /// El planificador ya recogió su hilo y liberó su espacio y su dominio
    /// DMA: solo entonces `wait` puede retirar el proceso (auditoría
    /// 3eb4965, hallazgo 1: con varias CPUs el estado del hilo no basta).
    reaped: bool,
}

/// Tamaño a partir del cual una línea incompleta se emite tal cual.
const CONSOLE_LINE_MAX: usize = 1024;

static PROCESSES: IrqLock<BTreeMap<Pid, Process>> = IrqLock::new(BTreeMap::new());
static NEXT_PID: IrqLock<u32> = IrqLock::new(1);

/// Crea un proceso a partir de un ELF64 estático de usuario.
pub fn spawn(name: &str, elf_bytes: &[u8]) -> Result<Pid, &'static str> {
    spawn_with_handles(name, elf_bytes, Vec::new())
}

/// Capabilities extra y argumento de `_start`.
pub fn spawn_with_handles_and_arg(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
    arg: u64,
    dma_quota: u64,
) -> Result<Pid, &'static str> {
    spawn_full(name, elf_bytes, extra, arg, dma_quota)
}

/// Base de los mapeos MMIO/DMA de usuario.
const USER_MMIO_BASE: u64 = 0x1000_0000;

/// Como `spawn`, con capabilities adicionales insertadas tras las tres
/// iniciales (la primera recibe el handle `0x0001_0004`, y así sucesivamente).
pub fn spawn_with_handles(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
) -> Result<Pid, &'static str> {
    spawn_full(name, elf_bytes, extra, 0, 0)
}

fn spawn_full(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
    arg: u64,
    dma_quota: u64,
) -> Result<Pid, &'static str> {
    let elf = aricode_elf::Elf::parse(elf_bytes).map_err(|_| "process: ELF invalido")?;
    let (lo, hi) = elf.virt_range();
    if lo < USER_CODE_BASE || hi > USER_STACK_TOP - USER_STACK_SIZE - PAGE_SIZE {
        return Err("process: el ELF no cabe en la region de usuario");
    }
    let pid = {
        let mut n = NEXT_PID.lock();
        let p = Pid(*n);
        *n += 1;
        p
    };
    let mut m =
        Mapper::new(vmm::PmmFrames, OffsetMap(phys::offset())).map_err(|_| "process: pml4")?;
    let kernel_root = vmm::with(|k| k.root_phys());
    // SAFETY: el PML4 del kernel es válido y accesible por el mapa físico.
    unsafe { m.share_kernel_half(kernel_root) };

    for seg in &elf.segments {
        let mut flags = USER;
        if seg.writable() {
            flags |= WRITABLE;
        }
        if !seg.executable() {
            flags |= NO_EXECUTE;
        }
        let (start, end) = seg.page_range();
        let bytes = elf.segment_bytes(seg);
        for page in (start..end).step_by(PAGE_SIZE as usize) {
            let frame = pmm::with(|p| p.allocate()).ok_or("process: sin frames")?;
            let dst = phys::phys_to_virt(frame.addr()) as *mut u8;
            // SAFETY: frame recién asignado, accesible por el mapa físico.
            unsafe { core::ptr::write_bytes(dst, 0, PAGE_SIZE as usize) };
            // Parte del contenido del fichero que cae en esta página.
            let file_start = seg.vaddr.max(page);
            let file_end = (seg.vaddr + seg.filesz).min(page + PAGE_SIZE);
            if file_start < file_end {
                let src =
                    &bytes[(file_start - seg.vaddr) as usize..(file_end - seg.vaddr) as usize];
                // SAFETY: destino dentro del frame; `src` dentro del segmento.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src.as_ptr(),
                        dst.add((file_start - page) as usize),
                        src.len(),
                    )
                };
            }
            m.map_4k(page, frame.addr(), flags)
                .map_err(|_| "process: map segmento")?;
        }
    }
    // Pila: RW, NX, con la página inferior sin mapear como guard.
    for virt in (USER_STACK_TOP - USER_STACK_SIZE..USER_STACK_TOP).step_by(PAGE_SIZE as usize) {
        let frame = pmm::with(|p| p.allocate()).ok_or("process: sin frames")?;
        // SAFETY: frame recién asignado.
        unsafe {
            core::ptr::write_bytes(
                phys::phys_to_virt(frame.addr()) as *mut u8,
                0,
                PAGE_SIZE as usize,
            )
        };
        m.map_4k(virt, frame.addr(), USER | WRITABLE | NO_EXECUTE)
            .map_err(|_| "process: map stack")?;
    }

    // El hilo se prepara ahora y se encola solo cuando el proceso ya está en
    // la tabla: su primer código consulta la entrada y el salto a ring 3.
    let thread = scheduler::prepare_for_process(
        name,
        user_thread_entry,
        pid.0 as usize,
        pid.0,
        m.root_phys(),
    )?;
    let tid = thread.tid;
    let mut handles = crate::object::initial_table(pid, tid);
    for (obj, rights) in extra {
        handles
            .insert(obj, rights)
            .map_err(|_| "process: tabla de handles llena")?;
    }
    PROCESSES.lock().insert(
        pid,
        Process {
            name: String::from(name),
            handles,
            arg,
            exit_wait: alloc::sync::Arc::new(crate::task::wait::WaitQueue::new()),
            mmio_next: USER_MMIO_BASE,
            foreign: Vec::new(),
            mapper: Some(m),
            entry: elf.entry,
            status: None,
            dma_quota,
            dma_used: 0,
            console_line: Vec::new(),
            reaped: false,
        },
    );
    scheduler::start_thread(thread);
    info!(
        "user: proceso {} '{name}' creado ({} segmentos, {} KiB, entrada {:#x}, hilo {tid:?})",
        pid.0,
        elf.segments.len(),
        (hi - lo) / 1024,
        elf.entry
    );
    Ok(pid)
}

/// Primer código del hilo principal, ya con el espacio del proceso activo:
/// fija la pila de kernel del hilo y salta a ring 3.
extern "C" fn user_thread_entry(pid: usize) {
    let pid = Pid(pid as u32);
    let (entry, arg) = {
        let procs = PROCESSES.lock();
        let p = procs.get(&pid).expect("proceso");
        (p.entry, p.arg)
    };
    // SAFETY: código y pila mapeados con USER en `spawn`; la pila de kernel
    // la fijó el planificador al conmutar a este hilo.
    unsafe { usermode::enter(entry, USER_STACK_TOP, arg) }
}

/// Proceso del hilo actual, si es de usuario.
pub fn current_pid() -> Option<Pid> {
    scheduler::current_process().map(Pid)
}

/// Acumula `text` en la línea pendiente del proceso actual y emite por la
/// consola todo lo que ya forme líneas completas (o todo, si la línea
/// pendiente supera `CONSOLE_LINE_MAX`).
pub fn console_write_current(text: &str) {
    let Some(pid) = current_pid() else {
        crate::print!("{text}");
        return;
    };
    let ready: Vec<u8> = {
        let mut procs = PROCESSES.lock();
        let Some(p) = procs.get_mut(&pid) else {
            drop(procs);
            crate::print!("{text}");
            return;
        };
        p.console_line.extend_from_slice(text.as_bytes());
        match p.console_line.iter().rposition(|&b| b == b'\n') {
            Some(i) => {
                let rest = p.console_line.split_off(i + 1);
                core::mem::replace(&mut p.console_line, rest)
            }
            None if p.console_line.len() >= CONSOLE_LINE_MAX => {
                core::mem::take(&mut p.console_line)
            }
            None => Vec::new(),
        }
    };
    if !ready.is_empty() {
        crate::print!("{}", String::from_utf8_lossy(&ready));
    }
}

/// Emite la línea incompleta pendiente de `pid` (prompt antes de leer la
/// consola, salida del proceso).
pub fn console_flush(pid: Pid) {
    let pending = PROCESSES
        .lock()
        .get_mut(&pid)
        .map(|p| core::mem::take(&mut p.console_line))
        .unwrap_or_default();
    if !pending.is_empty() {
        crate::print!("{}", String::from_utf8_lossy(&pending));
    }
}

fn finish(pid: Pid, status: ExitStatus) -> ! {
    console_flush(pid);
    let (name, waiters) = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).expect("proceso en la tabla");
        p.status = Some(status);
        (p.name.clone(), p.exit_wait.clone())
    };
    waiters.wake_all();
    match status {
        ExitStatus::Exited(code) => {
            info!("user: proceso {} '{name}' salio con codigo {code}", pid.0)
        }
        ExitStatus::Killed(why) => {
            warn!(
                "user: proceso {} '{name}' terminado por fallo: {why}",
                pid.0
            )
        }
    }
    scheduler::exit()
}

/// `process_exit` desde una syscall.
pub fn exit_current(code: u64) -> ! {
    let pid = current_pid().expect("exit desde un hilo de usuario");
    finish(pid, ExitStatus::Exited(code))
}

/// Excepción en ring 3: termina el proceso, nunca el kernel.
pub fn kill_current(why: &'static str) -> ! {
    let pid = current_pid().expect("fallo de usuario en un hilo de usuario");
    finish(pid, ExitStatus::Killed(why))
}

/// Libera el espacio de direcciones y el dominio DMA de un proceso
/// terminado y lo marca recogido, despertando a quien lo espera. Lo llama el
/// planificador al recoger su hilo, siempre desde otro hilo y con otro CR3.
pub fn release_address_space(pid: u32) {
    let taken = PROCESSES.lock().get_mut(&Pid(pid)).and_then(|p| {
        p.mapper
            .take()
            .map(|m| (m, core::mem::take(&mut p.foreign)))
    });
    if let Some((mut m, foreign)) = taken {
        // Memoria ajena (MMIO, DMA): se desmapea sin liberar; sus dueños son
        // el dispositivo o el objeto DMA.
        for (virt, len) in foreign {
            for page in (virt..virt + len).step_by(PAGE_SIZE as usize) {
                let _ = m.unmap_4k(page);
            }
        }
        let mut frames = Vec::new();
        m.free_user_half(|f| frames.push(f));
        frames.push(m.into_root_frame());
        let n = frames.len();
        pmm::with(|p| {
            for f in frames {
                p.free(Frame::from_addr(f)).expect("frame del proceso");
            }
        });
        log::debug!("user: espacio del proceso {pid} liberado ({n} frames)");
    }
    crate::iommu::release(pid);
    // Recogido del todo: ahora sí puede retirarse de la tabla.
    let waiters = PROCESSES.lock().get_mut(&Pid(pid)).map(|p| {
        p.reaped = true;
        p.exit_wait.clone()
    });
    if let Some(w) = waiters {
        w.wake_all();
    }
}

/// Espera a que el proceso termine y esté recogido (espacio y dominio
/// liberados), y devuelve su estado retirándolo de la tabla. `None` si vence
/// el plazo o si otro `wait` lo retiró antes.
pub fn wait_until(pid: Pid, deadline: Option<u64>) -> Option<ExitStatus> {
    let waiters = PROCESSES.lock().get(&pid)?.exit_wait.clone();
    loop {
        {
            let mut procs = PROCESSES.lock();
            let p = procs.get(&pid)?;
            if p.reaped {
                let status = p.status;
                procs.remove(&pid);
                return status;
            }
        }
        let timed_out = waiters.wait_until_deadline(
            || PROCESSES.lock().get(&pid).is_none_or(|p| p.reaped),
            deadline,
        );
        if timed_out {
            return None;
        }
    }
}

/// Espera sin plazo.
pub fn wait(pid: Pid) -> ExitStatus {
    wait_until(pid, None).expect("sin plazo no hay timeout")
}

/// Autoprueba (`test=process_reap`, auditoría 3eb4965, hallazgo 1): crear y
/// esperar procesos por lotes, en todas las CPUs, no pierde frames: `wait`
/// no retira un proceso hasta que su espacio se ha devuelto al PMM.
pub fn reap_selftest() {
    use crate::memory::{heap, pmm};
    use crate::user::programs;

    let code = programs::by_name("hello").expect("hello en el initrd");
    // Calentamiento: lo que el heap vaya a crecer, que crezca antes de medir.
    for _ in 0..4 {
        let pid = spawn("hello", code).expect("spawn");
        assert_eq!(wait(pid), ExitStatus::Exited(42));
    }
    let free_before = pmm::stats().free_frames;
    let heap_before = heap::mapped_bytes();
    const ROUNDS: usize = 30;
    const BATCH: usize = 6;
    for _ in 0..ROUNDS {
        let pids: Vec<Pid> = (0..BATCH)
            .map(|_| spawn("hello", code).expect("spawn"))
            .collect();
        for pid in pids {
            assert_eq!(wait(pid), ExitStatus::Exited(42));
            assert!(!exists(pid), "wait debe retirar el proceso {pid:?}");
        }
    }
    assert!(
        PROCESSES.lock().is_empty(),
        "no debe quedar ningun proceso en la tabla"
    );
    let heap_grown = (heap::mapped_bytes() - heap_before) / PAGE_SIZE;
    let free_after = pmm::stats().free_frames;
    let lost = free_before as i64 - free_after as i64 - heap_grown as i64;
    assert!(
        lost <= 8,
        "frames perdidos tras {} procesos: {lost} (heap +{heap_grown} paginas)",
        ROUNDS * BATCH
    );
    info!(
        "process reap selftest ok ({} procesos creados y esperados en {} CPU(s), {lost} frames de diferencia, heap +{heap_grown} paginas)",
        ROUNDS * BATCH,
        crate::cpu::online_count()
    );
}

/// Nombre de un proceso vivo (para diagnóstico).
/// Carga `len` bytes de DMA a la cuota del proceso actual; `false` si la
/// excedería (la cuota es acumulativa: lo reservado no se devuelve).
pub fn charge_dma(len: u64) -> bool {
    let Some(pid) = current_pid() else {
        return true;
    };
    let mut procs = PROCESSES.lock();
    let Some(p) = procs.get_mut(&pid) else {
        return true;
    };
    if p.dma_quota != 0 && p.dma_used.saturating_add(len) > p.dma_quota {
        return false;
    }
    p.dma_used += len;
    true
}

/// Instantánea de la tabla de procesos para `system_info`.
pub fn snapshot() -> Vec<aricode_abi::sysinfo::ProcessEntry> {
    use aricode_abi::sysinfo::{ProcessEntry, ProcessState};
    PROCESSES
        .lock()
        .iter()
        .map(|(pid, p)| {
            let (state, code) = match p.status {
                None => (ProcessState::Running, 0),
                Some(ExitStatus::Exited(c)) => {
                    (ProcessState::Exited, c.min(u16::MAX as u64) as u16)
                }
                Some(ExitStatus::Killed(_)) => (ProcessState::Killed, 0),
            };
            ProcessEntry::new(pid.0, state, code, &p.name)
        })
        .collect()
}

pub fn exists(pid: Pid) -> bool {
    PROCESSES.lock().contains_key(&pid)
}

/// Comprueba que `[ptr, ptr+len)` está en espacio de usuario y mapeado con
/// USER (y WRITABLE si `write`) en el proceso actual.
fn check_user_range(ptr: u64, len: u64, write: bool) -> Result<(), aricode_abi::Error> {
    if !aricode_abi::user_range_ok(ptr, len) {
        return Err(aricode_abi::Error::Fault);
    }
    if len == 0 {
        return Ok(());
    }
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let procs = PROCESSES.lock();
    let m = procs
        .get(&pid)
        .and_then(|p| p.mapper.as_ref())
        .ok_or(aricode_abi::Error::Fault)?;
    let needed = USER | if write { WRITABLE } else { 0 };
    let mut page = ptr & !(PAGE_SIZE - 1);
    while page < ptr + len {
        match m.translate(page) {
            Some((_, flags)) if flags & needed == needed => {}
            _ => return Err(aricode_abi::Error::Fault),
        }
        page += PAGE_SIZE;
    }
    debug_assert!(ptr + len <= USER_SPACE_END);
    Ok(())
}

/// Copia `[ptr, ptr+len)` del espacio del proceso actual, comprobando que
/// cada página está mapeada con USER. El espacio del proceso es el activo.
pub fn copy_from_user(ptr: u64, len: u64) -> Result<Vec<u8>, aricode_abi::Error> {
    check_user_range(ptr, len, false)?;
    // SAFETY: rango validado página a página en el espacio activo.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    Ok(bytes.to_vec())
}

/// Escribe `bytes` en `[ptr, ptr+len)` del proceso actual, comprobando que
/// cada página está mapeada con USER y WRITABLE.
pub fn copy_to_user(ptr: u64, bytes: &[u8]) -> Result<(), aricode_abi::Error> {
    check_user_range(ptr, bytes.len() as u64, true)?;
    // SAFETY: rango validado como escribible en el espacio activo.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    Ok(())
}

/// Acceso a la tabla de handles del proceso actual.
pub fn with_current_handles<R>(f: impl FnOnce(&mut crate::object::Handles) -> R) -> Option<R> {
    let pid = current_pid()?;
    let mut procs = PROCESSES.lock();
    procs.get_mut(&pid).map(|p| f(&mut p.handles))
}

/// Mapea `[phys, phys+len)` en el proceso actual como memoria de dispositivo
/// (sin caché) o buffer DMA (con caché). Devuelve la dirección virtual.
pub fn map_foreign(phys: u64, len: u64, cacheable: bool) -> Result<u64, aricode_abi::Error> {
    use aricode_x86_paging::{CACHE_DISABLE, WRITE_THROUGH};
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let mut procs = PROCESSES.lock();
    let p = procs.get_mut(&pid).ok_or(aricode_abi::Error::Fault)?;
    let m = p.mapper.as_mut().ok_or(aricode_abi::Error::Fault)?;
    let offset = phys & (PAGE_SIZE - 1);
    let start = phys - offset;
    let span = (offset + len).next_multiple_of(PAGE_SIZE);
    let virt = p.mmio_next;
    // La zona MMIO/DMA está por encima de la pila de usuario (8 MiB) y crece
    // hacia arriba hasta el fin del espacio de usuario.
    if virt
        .checked_add(span)
        .is_none_or(|end| end > USER_SPACE_END)
    {
        return Err(aricode_abi::Error::Limit);
    }
    let mut flags = USER | WRITABLE | NO_EXECUTE;
    if !cacheable {
        flags |= CACHE_DISABLE | WRITE_THROUGH;
    }
    m.map_range_4k(virt, start, span, flags)
        .map_err(|_| aricode_abi::Error::Limit)?;
    p.mmio_next += span;
    p.foreign.push((virt, span));
    Ok(virt + offset)
}
