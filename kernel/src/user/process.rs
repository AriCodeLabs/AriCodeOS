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
use aricode_sched::Tid;
use aricode_x86_paging::{Mapper, OffsetMap, NO_EXECUTE, PAGE_SIZE, USER, WRITABLE};
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub u32);

/// Dónde se carga el código y dónde va la pila de usuario.
pub const USER_CODE_BASE: u64 = 0x40_0000;
pub const USER_STACK_TOP: u64 = 0x80_0000;
pub const USER_STACK_SIZE: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Exited(u64),
    /// Terminado por el kernel: excepción en ring 3.
    Killed(&'static str),
}

pub struct Process {
    pub name: String,
    /// `None` una vez liberado el espacio de direcciones.
    mapper: Option<Mapper<vmm::PmmFrames, OffsetMap>>,
    pub main_thread: Tid,
    pub entry: u64,
    pub status: Option<ExitStatus>,
}

static PROCESSES: IrqLock<BTreeMap<Pid, Process>> = IrqLock::new(BTreeMap::new());
static NEXT_PID: IrqLock<u32> = IrqLock::new(1);

/// Crea un proceso a partir de un blob de código independiente de posición.
pub fn spawn(name: &str, code: &[u8]) -> Result<Pid, &'static str> {
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

    // Código: páginas de solo lectura y ejecución.
    let code_pages = (code.len() as u64).div_ceil(PAGE_SIZE).max(1);
    for i in 0..code_pages {
        let frame = pmm::with(|p| p.allocate()).ok_or("process: sin frames")?;
        let dst = phys::phys_to_virt(frame.addr()) as *mut u8;
        let off = (i * PAGE_SIZE) as usize;
        let chunk = &code[off.min(code.len())..(off + PAGE_SIZE as usize).min(code.len())];
        // SAFETY: frame recién asignado, accesible por el mapa físico.
        unsafe {
            core::ptr::write_bytes(dst, 0xCC, PAGE_SIZE as usize); // int3 como relleno
            core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst, chunk.len());
        }
        m.map_4k(USER_CODE_BASE + i * PAGE_SIZE, frame.addr(), USER)
            .map_err(|_| "process: map code")?;
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

    let tid = scheduler::spawn_for_process(
        name,
        user_thread_entry,
        pid.0 as usize,
        pid.0,
        m.root_phys(),
    )?;
    PROCESSES.lock().insert(
        pid,
        Process {
            name: String::from(name),
            mapper: Some(m),
            main_thread: tid,
            entry: USER_CODE_BASE,
            status: None,
        },
    );
    info!(
        "user: proceso {} '{name}' creado ({} bytes de codigo, hilo {tid:?})",
        pid.0,
        code.len()
    );
    Ok(pid)
}

/// Primer código del hilo principal, ya con el espacio del proceso activo:
/// fija la pila de kernel del hilo y salta a ring 3.
extern "C" fn user_thread_entry(pid: usize) {
    let pid = Pid(pid as u32);
    let entry = PROCESSES.lock().get(&pid).expect("proceso").entry;
    // SAFETY: código y pila mapeados con USER en `spawn`; la pila de kernel
    // la fijó el planificador al conmutar a este hilo.
    unsafe { usermode::enter(entry, USER_STACK_TOP) }
}

/// Proceso del hilo actual, si es de usuario.
pub fn current_pid() -> Option<Pid> {
    scheduler::current_process().map(Pid)
}

fn finish(pid: Pid, status: ExitStatus) -> ! {
    let name = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).expect("proceso en la tabla");
        p.status = Some(status);
        p.name.clone()
    };
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

/// Libera el espacio de direcciones de un proceso terminado. Lo llama el
/// planificador al recoger su hilo, siempre desde otro hilo y con otro CR3.
pub fn release_address_space(pid: u32) {
    let mapper = PROCESSES
        .lock()
        .get_mut(&Pid(pid))
        .and_then(|p| p.mapper.take());
    if let Some(mut m) = mapper {
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
}

/// Espera (cediendo) a que el proceso termine y devuelve su estado,
/// retirándolo de la tabla.
pub fn wait(pid: Pid) -> ExitStatus {
    loop {
        let done = PROCESSES.lock().get(&pid).and_then(|p| p.status);
        let thread_gone = PROCESSES
            .lock()
            .get(&pid)
            .map(|p| scheduler::state(p.main_thread).is_none());
        if let (Some(status), Some(true)) = (done, thread_gone) {
            PROCESSES.lock().remove(&pid);
            return status;
        }
        scheduler::yield_now();
    }
}

/// Copia `[ptr, ptr+len)` del espacio del proceso actual, comprobando que
/// cada página está mapeada con USER. El espacio del proceso es el activo.
pub fn copy_from_user(ptr: u64, len: u64) -> Result<Vec<u8>, aricode_abi::Error> {
    if !aricode_abi::user_range_ok(ptr, len) {
        return Err(aricode_abi::Error::Fault);
    }
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let procs = PROCESSES.lock();
    let m = procs
        .get(&pid)
        .and_then(|p| p.mapper.as_ref())
        .ok_or(aricode_abi::Error::Fault)?;
    let mut page = ptr & !(PAGE_SIZE - 1);
    while page < ptr + len {
        match m.translate(page) {
            Some((_, flags)) if flags & USER != 0 => {}
            _ => return Err(aricode_abi::Error::Fault),
        }
        page += PAGE_SIZE;
    }
    debug_assert!(ptr + len <= USER_SPACE_END);
    // SAFETY: rango validado página a página en el espacio activo.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    Ok(bytes.to_vec())
}
