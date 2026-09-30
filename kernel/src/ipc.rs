//! IPC por endpoints (Fase 14): colas acotadas de mensajes pequeños con
//! transferencia de handles, bloqueo con timeout y despertar por cola de
//! espera. La pieza central del microkernel (sección 23).

use crate::object::ObjectRef;
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use aricode_abi::{Error, IPC_QUEUE_CAPACITY, TIMEOUT_INFINITE};
use aricode_handles::Rights;
use core::sync::atomic::{AtomicU64, Ordering};

pub struct Message {
    pub data: Vec<u8>,
    /// Capabilities transferidas: ya retiradas del emisor.
    pub handles: Vec<(ObjectRef, Rights)>,
}

pub struct Endpoint {
    queue: IrqLock<VecDeque<Message>>,
    receivers: WaitQueue,
    senders: WaitQueue,
    sent: AtomicU64,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl Endpoint {
    pub const fn new() -> Self {
        Self {
            queue: IrqLock::new(VecDeque::new()),
            receivers: WaitQueue::new(),
            senders: WaitQueue::new(),
            sent: AtomicU64::new(0),
        }
    }

    /// Mensajes enviados en total (para el benchmark).
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    fn deadline(timeout_ms: u64) -> Option<u64> {
        if timeout_ms == TIMEOUT_INFINITE {
            None
        } else {
            Some(crate::time::ticks() + timeout_ms.div_ceil(crate::time::MS_PER_TICK))
        }
    }

    /// Encola un mensaje construido por `build` solo cuando hay sitio, dentro
    /// de la sección crítica de la cola: así las capabilities se retiran del
    /// emisor únicamente si el envío se produce, y un timeout no las toca.
    /// `build` no debe bloquear (se ejecuta con el lock de la cola tomado).
    pub fn send_with(
        &self,
        timeout_ms: u64,
        build: impl FnOnce() -> Result<Message, Error>,
    ) -> Result<(), Error> {
        let deadline = Self::deadline(timeout_ms);
        loop {
            let timed_out = self
                .senders
                .wait_until_deadline(|| self.queue.lock().len() < IPC_QUEUE_CAPACITY, deadline);
            if timed_out {
                return Err(Error::Timeout);
            }
            let mut q = self.queue.lock();
            // Otro emisor puede haber ocupado la plaza entre la espera y aquí.
            if q.len() >= IPC_QUEUE_CAPACITY {
                continue;
            }
            q.push_back(build()?);
            drop(q);
            self.sent.fetch_add(1, Ordering::Relaxed);
            self.receivers.wake_one();
            // Los receptores de varios endpoints (ADR 0020) esperan en una
            // cola común y vuelven a mirar los suyos.
            ANY_RECEIVERS.wake_all();
            return Ok(());
        }
    }

    /// Saca el mensaje más antiguo sin esperar.
    fn try_receive(&self) -> Option<Message> {
        let msg = self.queue.lock().pop_front()?;
        self.senders.wake_one();
        Some(msg)
    }

    fn has_message(&self) -> bool {
        !self.queue.lock().is_empty()
    }

    /// Saca el mensaje más antiguo; si no hay espera hasta `timeout_ms`.
    pub fn receive(&self, timeout_ms: u64) -> Result<Message, Error> {
        let deadline = Self::deadline(timeout_ms);
        let timed_out = self
            .receivers
            .wait_until_deadline(|| !self.queue.lock().is_empty(), deadline);
        if timed_out {
            return Err(Error::Timeout);
        }
        let msg = self.queue.lock().pop_front().ok_or(Error::Timeout)?;
        self.senders.wake_one();
        Ok(msg)
    }
}

/// Cola de espera de `receive_any`: la despierta cualquier envío.
static ANY_RECEIVERS: WaitQueue = WaitQueue::new();

/// Recibe del primero (por orden) de `endpoints` que tenga mensaje; devuelve
/// su índice. Espera hasta `timeout_ms` (ADR 0020).
pub fn receive_any(endpoints: &[&Endpoint], timeout_ms: u64) -> Result<(usize, Message), Error> {
    let deadline = Endpoint::deadline(timeout_ms);
    loop {
        let timed_out = ANY_RECEIVERS
            .wait_until_deadline(|| endpoints.iter().any(|e| e.has_message()), deadline);
        if timed_out {
            return Err(Error::Timeout);
        }
        // Otro receptor puede haberse llevado el mensaje: volver a esperar.
        for (i, e) in endpoints.iter().enumerate() {
            if let Some(msg) = e.try_receive() {
                return Ok((i, msg));
            }
        }
    }
}

/// Banco de medida (`test=ipc`): un servidor y un cliente de usuario hacen
/// ping-pong a través de un endpoint creado por el kernel. Mide el round
/// trip con el TSC. Ver `user/testprogs/src/bin/ipc_{server,client}.rs`.
/// Autoprueba de correlación (`test=ipc_late`, etapa 1B): una respuesta que
/// llega después del plazo del cliente nunca se confunde con la de la
/// petición siguiente, porque cada petición usa su propio endpoint de
/// respuesta.
pub fn late_reply_selftest() {
    use crate::object::{KernelObject, ObjectKind};
    use crate::user::{process, programs};
    use alloc::vec;

    let ep = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
    let server = process::spawn_with_handles(
        "late_server",
        programs::by_name("late_server").expect("late_server"),
        vec![(
            ep.clone(),
            Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE),
        )],
    )
    .expect("servidor");
    let client = process::spawn_with_handles(
        "late_client",
        programs::by_name("late_client").expect("late_client"),
        vec![(ep, Rights::WRITE.union(Rights::DUPLICATE))],
    )
    .expect("cliente");
    let client_status = process::wait(client);
    let server_status = process::wait(server);
    assert_eq!(
        client_status,
        process::ExitStatus::Exited(0),
        "cliente: {client_status:?}"
    );
    assert_eq!(
        server_status,
        process::ExitStatus::Exited(0),
        "servidor: {server_status:?}"
    );
    log::info!(
        "ipc late reply selftest ok (plazo vencido, respuesta tardia descartada, sin confusion)"
    );
}

pub fn selftest() {
    use crate::object::{KernelObject, ObjectKind};
    use crate::user::{process, programs};
    use alloc::vec;

    let ep = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
    let server_rights = Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE);
    let client_rights = Rights::WRITE.union(Rights::DUPLICATE); // sin READ: no puede recibir
    let server = process::spawn_with_handles(
        "ipc_server",
        programs::by_name("ipc_server").expect("ipc_server"),
        vec![(ep.clone(), server_rights)],
    )
    .expect("servidor");
    // SAFETY: rdtsc solo lee el contador de tiempo.
    let t0 = unsafe { core::arch::x86_64::_rdtsc() };
    let client = process::spawn_with_handles(
        "ipc_client",
        programs::by_name("ipc_client").expect("ipc_client"),
        vec![(ep.clone(), client_rights)],
    )
    .expect("cliente");
    let client_status = process::wait(client);
    // SAFETY: ídem.
    let t1 = unsafe { core::arch::x86_64::_rdtsc() };
    let server_status = process::wait(server);
    let sent = match &ep.kind {
        ObjectKind::Endpoint(e) => e.sent(),
        _ => unreachable!(),
    };
    assert_eq!(
        client_status,
        process::ExitStatus::Exited(0),
        "cliente: {client_status:?}"
    );
    assert_eq!(
        server_status,
        process::ExitStatus::Exited(0),
        "servidor: {server_status:?}"
    );
    let round_trips = sent.saturating_sub(1); // el "quit" no cuenta
    log::info!(
        "ipc selftest ok: {round_trips} round trips, {} ciclos TSC por round trip (incluye crear el cliente)",
        (t1 - t0) / round_trips.max(1)
    );
}
