// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Kernel objects (Phase 12) with revocation (Phase 13). Applications only
//! reach them through handles with rights (`libs/handles`); never through
//! pointers or global identifiers.

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
    /// Capability to create processes from the initrd.
    Spawner,
    Irq(crate::hw::IrqLine),
    Mmio(crate::hw::Region),
    Dma(crate::hw::DmaBuffer),
    /// Capability to reserve hardware resources (DMA buffers).
    Hardware,
    /// PCI root: enumerate and claim devices.
    PciRoot,
    /// System state: information (`READ`) and shutdown/reboot (`MANAGE`).
    System,
}

/// Object shared by every handle that refers to it. Revoking it invalidates
/// all of those handles at once, in every process, without having to find
/// them (section 25: lifetime and revocation).
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

    /// Informational object identifier (tid, pid). It grants no access: only
    /// the handle does.
    pub fn id(&self) -> u64 {
        match self.kind {
            ObjectKind::Thread(t) => t.0 as u64,
            ObjectKind::Process(p) => p.0 as u64,
            ObjectKind::Irq(ref l) => l.gsi() as u64,
            _ => 0,
        }
    }

    pub fn kind_code(&self) -> u64 {
        match self.kind {
            ObjectKind::Thread(_) => kind::THREAD,
            ObjectKind::Process(_) => kind::PROCESS,
            ObjectKind::Console => kind::CONSOLE,
            ObjectKind::Endpoint(_) => kind::ENDPOINT,
            ObjectKind::Spawner => kind::SPAWNER,
            ObjectKind::Irq(_) => kind::IRQ,
            ObjectKind::Mmio(_) => kind::MMIO,
            ObjectKind::Dma(_) => kind::DMA,
            ObjectKind::Hardware => kind::HARDWARE,
            ObjectKind::PciRoot => kind::PCI_ROOT,
            ObjectKind::System => kind::SYSTEM,
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

/// Handles per process (section 52: resource limits).
pub const HANDLE_LIMIT: usize = 256;

/// Initial table of a process: itself, its main thread and the console, in
/// that order, so they match `aricode_abi::handle`.
pub fn initial_table(pid: Pid, main_thread: Tid) -> Handles {
    let mut t = HandleTable::new(HANDLE_LIMIT);
    let self_h = t
        .insert(
            KernelObject::new(ObjectKind::Process(pid)),
            Rights::MANAGE.union(Rights::READ).union(Rights::DUPLICATE),
        )
        .expect("empty table");
    let thread_h = t
        .insert(
            KernelObject::new(ObjectKind::Thread(main_thread)),
            Rights::READ.union(Rights::WAIT).union(Rights::DUPLICATE),
        )
        .expect("empty table");
    let console_h = t
        .insert(
            KernelObject::new(ObjectKind::Console),
            Rights::WRITE.union(Rights::DUPLICATE),
        )
        .expect("empty table");
    debug_assert_eq!(self_h, Handle(aricode_abi::handle::SELF_PROCESS));
    debug_assert_eq!(thread_h, Handle(aricode_abi::handle::MAIN_THREAD));
    debug_assert_eq!(console_h, Handle(aricode_abi::handle::CONSOLE));
    t
}

/// Object behind a handle that holds `required` and is not revoked.
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

/// Translates table errors into ABI errors.
pub fn abi_error(e: HandleError) -> aricode_abi::Error {
    match e {
        HandleError::BadHandle => aricode_abi::Error::BadHandle,
        HandleError::Denied => aricode_abi::Error::Denied,
        HandleError::Escalation => aricode_abi::Error::Escalation,
        HandleError::TableFull => aricode_abi::Error::Limit,
    }
}
