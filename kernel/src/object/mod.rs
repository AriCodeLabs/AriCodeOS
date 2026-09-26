//! Objetos del kernel (Fase 12). Las aplicaciones solo los alcanzan por
//! handles con derechos (`libs/handles`); nunca por punteros ni por
//! identificadores globales.

use crate::user::process::Pid;
use alloc::sync::Arc;
use aricode_abi::kind;
use aricode_handles::{Handle, HandleError, HandleTable, Rights};
use aricode_sched::Tid;

#[derive(Debug)]
pub enum KernelObject {
    Thread(Tid),
    Process(Pid),
    Console,
}

impl KernelObject {
    /// Identificador informativo del objeto (tid, pid). No da acceso: solo
    /// el handle lo da.
    pub fn id(&self) -> u64 {
        match self {
            KernelObject::Thread(t) => t.0 as u64,
            KernelObject::Process(p) => p.0 as u64,
            KernelObject::Console => 0,
        }
    }

    pub fn kind(&self) -> u64 {
        match self {
            KernelObject::Thread(_) => kind::THREAD,
            KernelObject::Process(_) => kind::PROCESS,
            KernelObject::Console => kind::CONSOLE,
        }
    }
}

pub type ObjectRef = Arc<KernelObject>;
pub type Handles = HandleTable<ObjectRef>;

/// Handles por proceso (sección 52: límite de recursos).
pub const HANDLE_LIMIT: usize = 256;

/// Tabla inicial de un proceso: él mismo, su hilo principal y la consola,
/// en ese orden, para que coincidan con `aricode_abi::handle`.
pub fn initial_table(pid: Pid, main_thread: Tid) -> Handles {
    let mut t = HandleTable::new(HANDLE_LIMIT);
    let self_h = t
        .insert(
            Arc::new(KernelObject::Process(pid)),
            Rights::MANAGE.union(Rights::READ).union(Rights::DUPLICATE),
        )
        .expect("tabla vacia");
    let thread_h = t
        .insert(
            Arc::new(KernelObject::Thread(main_thread)),
            Rights::READ.union(Rights::WAIT).union(Rights::DUPLICATE),
        )
        .expect("tabla vacia");
    let console_h = t
        .insert(
            Arc::new(KernelObject::Console),
            Rights::WRITE.union(Rights::DUPLICATE),
        )
        .expect("tabla vacia");
    debug_assert_eq!(self_h, Handle(aricode_abi::handle::SELF_PROCESS));
    debug_assert_eq!(thread_h, Handle(aricode_abi::handle::MAIN_THREAD));
    debug_assert_eq!(console_h, Handle(aricode_abi::handle::CONSOLE));
    t
}

/// Traduce errores de la tabla a errores del ABI.
pub fn abi_error(e: HandleError) -> aricode_abi::Error {
    match e {
        HandleError::BadHandle => aricode_abi::Error::BadHandle,
        HandleError::Denied => aricode_abi::Error::Denied,
        HandleError::Escalation => aricode_abi::Error::Escalation,
        HandleError::TableFull => aricode_abi::Error::Limit,
    }
}
