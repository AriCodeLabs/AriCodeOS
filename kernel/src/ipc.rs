//! Endpoint IPC (Phase 14): bounded queues of small messages with handle
//! transfer, blocking with timeout and wake-up through wait queues. The
//! centerpiece of the microkernel (section 23).

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
    /// Transferred capabilities: already removed from the sender.
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

    /// Total messages sent (for the benchmark).
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

    /// Enqueues a message built by `build` only when there is room, inside the
    /// queue's critical section: capabilities are thus removed from the
    /// sender only if the send happens, and a timeout leaves them alone.
    /// `build` must not block (it runs with the queue lock held).
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
            // Another sender may have taken the slot between the wait and here.
            if q.len() >= IPC_QUEUE_CAPACITY {
                continue;
            }
            q.push_back(build()?);
            drop(q);
            self.sent.fetch_add(1, Ordering::Relaxed);
            self.receivers.wake_one();
            // Receivers on several endpoints (ADR 0020) wait on a shared queue
            // and re-check their own endpoints.
            ANY_RECEIVERS.wake_all();
            return Ok(());
        }
    }

    /// Takes the oldest message without waiting.
    fn try_receive(&self) -> Option<Message> {
        let msg = self.queue.lock().pop_front()?;
        self.senders.wake_one();
        Some(msg)
    }

    fn has_message(&self) -> bool {
        !self.queue.lock().is_empty()
    }

    /// Takes the oldest message; if there is none, waits up to `timeout_ms`.
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

/// Wait queue for `receive_any`: woken by any send.
static ANY_RECEIVERS: WaitQueue = WaitQueue::new();

/// Receives from the first (in order) of `endpoints` that has a message;
/// returns its index. Waits up to `timeout_ms` (ADR 0020).
pub fn receive_any(endpoints: &[&Endpoint], timeout_ms: u64) -> Result<(usize, Message), Error> {
    let deadline = Endpoint::deadline(timeout_ms);
    loop {
        let timed_out = ANY_RECEIVERS
            .wait_until_deadline(|| endpoints.iter().any(|e| e.has_message()), deadline);
        if timed_out {
            return Err(Error::Timeout);
        }
        // Another receiver may have taken the message: wait again.
        for (i, e) in endpoints.iter().enumerate() {
            if let Some(msg) = e.try_receive() {
                return Ok((i, msg));
            }
        }
    }
}

/// Benchmark (`test=ipc`): a user server and client play ping-pong through
/// an endpoint created by the kernel. Measures the round trip with the TSC.
/// See `user/testprogs/src/bin/ipc_{server,client}.rs`.
/// Correlation self-test (`test=ipc_late`, stage 1B): a reply that arrives
/// after the client's deadline is never mistaken for the reply to the next
/// request, because each request uses its own reply endpoint.
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
    .expect("server");
    let client = process::spawn_with_handles(
        "late_client",
        programs::by_name("late_client").expect("late_client"),
        vec![(ep, Rights::WRITE.union(Rights::DUPLICATE))],
    )
    .expect("client");
    let client_status = process::wait(client);
    let server_status = process::wait(server);
    assert_eq!(
        client_status,
        process::ExitStatus::Exited(0),
        "client: {client_status:?}"
    );
    assert_eq!(
        server_status,
        process::ExitStatus::Exited(0),
        "server: {server_status:?}"
    );
    log::info!("ipc late reply selftest ok (deadline expired, late reply discarded, no mix-up)");
}

pub fn selftest() {
    use crate::object::{KernelObject, ObjectKind};
    use crate::user::{process, programs};
    use alloc::vec;

    let ep = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
    let server_rights = Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE);
    let client_rights = Rights::WRITE.union(Rights::DUPLICATE); // no READ: cannot receive
    let server = process::spawn_with_handles(
        "ipc_server",
        programs::by_name("ipc_server").expect("ipc_server"),
        vec![(ep.clone(), server_rights)],
    )
    .expect("server");
    // SAFETY: rdtsc only reads the time stamp counter.
    let t0 = unsafe { core::arch::x86_64::_rdtsc() };
    let client = process::spawn_with_handles(
        "ipc_client",
        programs::by_name("ipc_client").expect("ipc_client"),
        vec![(ep.clone(), client_rights)],
    )
    .expect("client");
    let client_status = process::wait(client);
    // SAFETY: same as above.
    let t1 = unsafe { core::arch::x86_64::_rdtsc() };
    let server_status = process::wait(server);
    let sent = match &ep.kind {
        ObjectKind::Endpoint(e) => e.sent(),
        _ => unreachable!(),
    };
    assert_eq!(
        client_status,
        process::ExitStatus::Exited(0),
        "client: {client_status:?}"
    );
    assert_eq!(
        server_status,
        process::ExitStatus::Exited(0),
        "server: {server_status:?}"
    );
    let round_trips = sent.saturating_sub(1); // the "quit" does not count
    log::info!(
        "ipc selftest ok: {round_trips} round trips, {} TSC cycles per round trip (includes creating the client)",
        (t1 - t0) / round_trips.max(1)
    );
}
