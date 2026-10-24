// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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
    /// A console whose text goes to an endpoint (ADR 0047): the capability
    /// to its send side, held here so the side stays open while any handle
    /// to this console lives. Only `console_write` uses it.
    SessionConsole(Cap),
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
            ObjectKind::Console | ObjectKind::SessionConsole(_) => kind::CONSOLE,
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

    /// Invalidates every handle to the object. An endpoint also closes both
    /// of its sides, so nobody keeps waiting on it (ADR 0030).
    pub fn revoke(self: &Arc<Self>) {
        self.revoked.store(true, Ordering::Release);
        if let ObjectKind::Endpoint(ep) = &self.kind {
            if ep.shut_down() {
                crate::ipc::settle_later(self.clone());
            }
        }
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
}

pub type ObjectRef = Arc<KernelObject>;

/// A capability: the object plus its share of the endpoint sides its rights
/// open (ADR 0030). Every capability lives in exactly one place (a handle
/// table, a message in flight, a list of handles for a new process) and
/// moves between them without being copied, so the endpoint counts exactly
/// how many `READ` and `WRITE` capabilities exist. Dropping the last one of
/// a side closes it; the work that follows (discarding the queue, waking
/// waiters) is deferred to a point without locks (`ipc::settle_closed`),
/// because a capability may be dropped with a process table or a queue
/// locked.
pub struct Cap {
    obj: ObjectRef,
    receive: bool,
    send: bool,
}

impl Cap {
    pub fn new(obj: ObjectRef, rights: Rights) -> Self {
        let (receive, send) = match &obj.kind {
            ObjectKind::Endpoint(ep) => {
                let sides = (
                    rights.contains(Rights::READ),
                    rights.contains(Rights::WRITE),
                );
                ep.open_sides(sides.0, sides.1);
                sides
            }
            _ => (false, false),
        };
        Self { obj, receive, send }
    }

    pub fn object(&self) -> &ObjectRef {
        &self.obj
    }
}

impl Drop for Cap {
    fn drop(&mut self) {
        if let ObjectKind::Endpoint(ep) = &self.obj.kind {
            if ep.close_sides(self.receive, self.send) {
                crate::ipc::settle_later(self.obj.clone());
            }
        }
    }
}

pub type Handles = HandleTable<Cap>;

/// Handles per process (section 52: resource limits).
pub const HANDLE_LIMIT: usize = 256;

/// Initial table of a process: itself, its main thread and the console, in
/// that order, so they match `aricode_abi::handle`. The console is the
/// kernel's unless `session_console` brings an endpoint's send side
/// (ADR 0047).
pub fn initial_table(pid: Pid, main_thread: Tid, session_console: Option<Cap>) -> Handles {
    let mut t = HandleTable::new(HANDLE_LIMIT);
    let self_h = insert(
        &mut t,
        KernelObject::new(ObjectKind::Process(pid)),
        Rights::MANAGE.union(Rights::READ).union(Rights::DUPLICATE),
    )
    .expect("empty table");
    let thread_h = insert(
        &mut t,
        KernelObject::new(ObjectKind::Thread(main_thread)),
        Rights::READ.union(Rights::WAIT).union(Rights::DUPLICATE),
    )
    .expect("empty table");
    let console = match session_console {
        Some(cap) => ObjectKind::SessionConsole(cap),
        None => ObjectKind::Console,
    };
    let console_h = insert(
        &mut t,
        KernelObject::new(console),
        Rights::WRITE.union(Rights::DUPLICATE),
    )
    .expect("empty table");
    debug_assert_eq!(self_h, Handle(aricode_abi::handle::SELF_PROCESS));
    debug_assert_eq!(thread_h, Handle(aricode_abi::handle::MAIN_THREAD));
    debug_assert_eq!(console_h, Handle(aricode_abi::handle::CONSOLE));
    t
}

/// New handle with `rights` to `obj` (a capability made here).
pub fn insert(
    t: &mut Handles,
    obj: ObjectRef,
    rights: Rights,
) -> Result<Handle, aricode_abi::Error> {
    t.insert(Cap::new(obj, rights), rights).map_err(abi_error)
}

/// Object behind a handle that holds `required` and is not revoked.
pub fn lookup(
    t: &Handles,
    handle: Handle,
    required: Rights,
) -> Result<&ObjectRef, aricode_abi::Error> {
    let obj = t.get_checked(handle, required).map_err(abi_error)?.object();
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
