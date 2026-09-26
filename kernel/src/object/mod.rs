//! Objetos del kernel (Fase 12) con revocación (Fase 13). Las aplicaciones
//! solo los alcanzan por handles con derechos (`libs/handles`); nunca por
//! punteros ni por identificadores globales.

use crate::user::process::Pid;
use alloc::sync::Arc;
use aricode_abi::kind;
use aricode_handles::{Handle, HandleError, HandleTable, Rights};
use aricode_sched::Tid;
use core::sync::atomic::{AtomicBool, Ordering};

pub enum ObjectKind {
    Thread(Tid),
    Process(Pid),
    Console,
    Endpoint(crate::ipc::Endpoint),
}

/// Objeto compartido por todos los handles que lo referencian. Revocarlo
/// invalida cada uno de esos handles a la vez, en todos los procesos, sin
/// tener que encontrarlos (sección 25: lifetime y revocación).
pub struct KernelObject {
    pub kind: ObjectKind,
    revoked: AtomicBool,
}

impl KernelObject {
    pub fn new(kind: ObjectKind) -> ObjectRef {
        Arc::new(Self {
            kind,
            revoked: AtomicBool::new(false),
        })
    }

    /// Identificador informativo del objeto (tid, pid). No da acceso: solo
    /// el handle lo da.
    pub fn id(&self) -> u64 {
        match self.kind {
            ObjectKind::Thread(t) => t.0 as u64,
            ObjectKind::Process(p) => p.0 as u64,
            ObjectKind::Console | ObjectKind::Endpoint(_) => 0,
        }
    }

    pub fn kind_code(&self) -> u64 {
        match self.kind {
            ObjectKind::Thread(_) => kind::THREAD,
            ObjectKind::Process(_) => kind::PROCESS,
            ObjectKind::Console => kind::CONSOLE,
            ObjectKind::Endpoint(_) => kind::ENDPOINT,
        }
    }

    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
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
            KernelObject::new(ObjectKind::Process(pid)),
            Rights::MANAGE.union(Rights::READ).union(Rights::DUPLICATE),
        )
        .expect("tabla vacia");
    let thread_h = t
        .insert(
            KernelObject::new(ObjectKind::Thread(main_thread)),
            Rights::READ.union(Rights::WAIT).union(Rights::DUPLICATE),
        )
        .expect("tabla vacia");
    let console_h = t
        .insert(
            KernelObject::new(ObjectKind::Console),
            Rights::WRITE.union(Rights::DUPLICATE),
        )
        .expect("tabla vacia");
    debug_assert_eq!(self_h, Handle(aricode_abi::handle::SELF_PROCESS));
    debug_assert_eq!(thread_h, Handle(aricode_abi::handle::MAIN_THREAD));
    debug_assert_eq!(console_h, Handle(aricode_abi::handle::CONSOLE));
    t
}

/// Objeto de un handle que posee `required` y no está revocado.
pub fn lookup(
    t: &Handles,
    handle: Handle,
    required: Rights,
) -> Result<&ObjectRef, aricode_abi::Error> {
    let obj = t.get_checked(handle, required).map_err(abi_error)?;
    if obj.is_revoked() {
        return Err(aricode_abi::Error::Revoked);
    }
    Ok(obj)
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
