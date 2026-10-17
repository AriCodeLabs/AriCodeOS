// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Endpoint IPC (Phase 14): bounded queues of small messages with handle
//! transfer, blocking with timeout and wake-up through wait queues. The
//! centerpiece of the microkernel (section 23).

use crate::object::{Cap, ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use aricode_abi::{Error, IPC_QUEUE_CAPACITY, TIMEOUT_INFINITE};
use aricode_handles::Rights;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub struct Message {
    pub data: Vec<u8>,
    /// Transferred capabilities: already removed from the sender.
    pub handles: Vec<(Cap, Rights)>,
}

/// An endpoint has two sides (ADR 0030): the receive side is every
/// capability with `READ` on it, the send side every capability with
/// `WRITE`. When the last capability of a side is dropped (handle closed,
/// process reaped, message discarded) that side closes for good: no
/// capability can be made for it again, since duplicating needs one.
pub struct Endpoint {
    queue: IrqLock<VecDeque<Message>>,
    receivers: WaitQueue,
    senders: WaitQueue,
    sent: AtomicU64,
    receive_caps: AtomicUsize,
    send_caps: AtomicUsize,
    /// Set once (last `READ` capability gone, or revoked): sends fail with
    /// `PeerClosed` and the queue is discarded.
    receive_closed: AtomicBool,
    /// Set once (last `WRITE` capability gone, or revoked): an empty queue
    /// means `PeerClosed` for receivers.
    send_closed: AtomicBool,
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
            receive_caps: AtomicUsize::new(0),
            send_caps: AtomicUsize::new(0),
            receive_closed: AtomicBool::new(false),
            send_closed: AtomicBool::new(false),
        }
    }

    /// Total messages sent (for the benchmark).
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    /// A capability with these sides was made (`Cap::new`).
    pub fn open_sides(&self, receive: bool, send: bool) {
        if receive {
            self.receive_caps.fetch_add(1, Ordering::AcqRel);
        }
        if send {
            self.send_caps.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// A capability with these sides was dropped. `true` if a side closed
    /// now: the caller hands the endpoint to `settle_later`. Only atomics
    /// here, since the caller may hold any lock.
    pub fn close_sides(&self, receive: bool, send: bool) -> bool {
        let mut closed = false;
        if receive && self.receive_caps.fetch_sub(1, Ordering::AcqRel) == 1 {
            closed |= !self.receive_closed.swap(true, Ordering::AcqRel);
        }
        if send && self.send_caps.fetch_sub(1, Ordering::AcqRel) == 1 {
            closed |= !self.send_closed.swap(true, Ordering::AcqRel);
        }
        closed
    }

    /// Closes both sides (revocation). `true` if one of them was open.
    pub fn shut_down(&self) -> bool {
        let receive = !self.receive_closed.swap(true, Ordering::AcqRel);
        let send = !self.send_closed.swap(true, Ordering::AcqRel);
        receive || send
    }

    fn receive_closed(&self) -> bool {
        self.receive_closed.load(Ordering::Acquire)
    }

    fn send_closed(&self) -> bool {
        self.send_closed.load(Ordering::Acquire)
    }

    /// After a side closed: discards the queue if nobody can receive it any
    /// more (the capabilities inside are dropped, which may close other
    /// endpoints: a reply endpoint whose only `WRITE` travelled in a
    /// request) and wakes every waiter so it sees the new state.
    fn settle(&self) {
        if self.receive_closed() {
            let discarded = core::mem::take(&mut *self.queue.lock());
            drop(discarded);
        }
        self.senders.wake_all();
        self.receivers.wake_all();
        ANY_RECEIVERS.wake_all();
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
    /// `PeerClosed` once the receive side is closed, also for a sender
    /// already waiting for room.
    pub fn send_with(
        &self,
        timeout_ms: u64,
        build: impl FnOnce() -> Result<Message, Error>,
    ) -> Result<(), Error> {
        let deadline = Self::deadline(timeout_ms);
        loop {
            let timed_out = self.senders.wait_until_deadline(
                || self.receive_closed() || self.queue.lock().len() < IPC_QUEUE_CAPACITY,
                deadline,
            );
            if self.receive_closed() {
                return Err(Error::PeerClosed);
            }
            if timed_out {
                return Err(Error::Timeout);
            }
            let mut q = self.queue.lock();
            // Checked under the queue lock: `settle` discards the queue under
            // the same lock after the flag is set, so no message is left
            // behind in a queue nobody will read.
            if self.receive_closed() {
                return Err(Error::PeerClosed);
            }
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

    /// Nothing queued and nobody left to send: a receive would wait forever.
    fn drained_and_closed(&self) -> bool {
        self.send_closed() && !self.has_message()
    }

    /// Takes the oldest message; if there is none, waits up to `timeout_ms`.
    /// Queued messages are delivered first; then `PeerClosed` once the send
    /// side is closed.
    pub fn receive(&self, timeout_ms: u64) -> Result<Message, Error> {
        let deadline = Self::deadline(timeout_ms);
        // Woken without a message (another receiver took it, or the
        // deadline passed): `Timeout`, as before.
        let _ = self.receivers.wait_until_deadline(
            || self.send_closed() || !self.queue.lock().is_empty(),
            deadline,
        );
        if let Some(msg) = self.try_receive() {
            return Ok(msg);
        }
        if self.send_closed() {
            return Err(Error::PeerClosed);
        }
        Err(Error::Timeout)
    }
}

/// Wait queue for `receive_any`: woken by any send.
static ANY_RECEIVERS: WaitQueue = WaitQueue::new();

/// Receives from the first (in order) of `endpoints` that has a message;
/// returns its index. Waits up to `timeout_ms` (ADR 0020). `PeerClosed`
/// only when every endpoint is empty and has lost its send side: one closed
/// endpoint among live ones is just one that stays empty.
pub fn receive_any(endpoints: &[&Endpoint], timeout_ms: u64) -> Result<(usize, Message), Error> {
    let deadline = Endpoint::deadline(timeout_ms);
    loop {
        let all_closed = || endpoints.iter().all(|e| e.drained_and_closed());
        let timed_out = ANY_RECEIVERS.wait_until_deadline(
            || all_closed() || endpoints.iter().any(|e| e.has_message()),
            deadline,
        );
        for (i, e) in endpoints.iter().enumerate() {
            if let Some(msg) = e.try_receive() {
                return Ok((i, msg));
            }
        }
        if all_closed() {
            return Err(Error::PeerClosed);
        }
        if timed_out {
            return Err(Error::Timeout);
        }
        // Another receiver took the message: wait again.
    }
}

/// Endpoints with a side closed since the last `settle_closed`.
static PENDING: IrqLock<Vec<ObjectRef>> = IrqLock::new(Vec::new());
static PENDING_ANY: AtomicBool = AtomicBool::new(false);

/// Queues `obj` (an endpoint that just lost a side) for `settle_closed`.
/// Safe under any lock: `PENDING` is a leaf lock.
pub fn settle_later(obj: ObjectRef) {
    PENDING.lock().push(obj);
    PENDING_ANY.store(true, Ordering::Release);
}

/// Completes the closing of the endpoints queued by `settle_later`:
/// discards their queues and wakes their waiters. Called with no lock held
/// (end of every syscall, after reaping finished threads). A capability a
/// kernel thread drops outside both (a self-test) is settled at the next
/// syscall or reap on any CPU. Discarding a
/// queue drops capabilities, which may queue more endpoints: handled
/// iteratively here, never by recursion.
pub fn settle_closed() {
    // A plain load first: this runs at the end of every syscall, and a swap
    // would take the cache line exclusively on every CPU each time.
    if !PENDING_ANY.load(Ordering::Relaxed) || !PENDING_ANY.swap(false, Ordering::AcqRel) {
        return;
    }
    loop {
        let batch = core::mem::take(&mut *PENDING.lock());
        if batch.is_empty() {
            return;
        }
        for obj in batch {
            if let ObjectKind::Endpoint(ep) = &obj.kind {
                ep.settle();
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

/// Peer-gone self-test (`test=ipc_peer`, ADR 0030): a client whose server
/// dies learns it at once with `PeerClosed`, whether the server died
/// holding the request, with the request still queued or with the client
/// blocked on a full queue; plus the endpoint-local cases (`peer_client`
/// case 4). The client waits 30 s everywhere; each case must end well
/// within `CASE_BOUND_MS`.
pub fn peer_closed_selftest() {
    use crate::object::KernelObject;
    use crate::user::{process, programs};
    use alloc::vec;

    const CASE_BOUND_MS: u64 = 5_000;
    let server_elf = programs::by_name("peer_server").expect("peer_server");
    let client_elf = programs::by_name("peer_client").expect("peer_client");
    let mut took = [0u64; 4];
    for case in 1..=4u64 {
        let t0 = crate::time::uptime_ms();
        let server = (case < 4).then(|| {
            // Fresh endpoints per case: a closed side never reopens.
            let ep = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
            let sync = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
            let server = process::spawn_with_handles_and_arg(
                "peer_server",
                server_elf,
                vec![(ep.clone(), Rights::READ), (sync.clone(), Rights::READ)],
                case,
                0,
            )
            .expect("peer_server");
            (server, ep, sync)
        });
        let handles = match &server {
            Some((_, ep, sync)) => vec![
                (ep.clone(), Rights::WRITE.union(Rights::DUPLICATE)),
                (sync.clone(), Rights::WRITE),
            ],
            None => vec![],
        };
        let client =
            process::spawn_with_handles_and_arg("peer_client", client_elf, handles, case, 0)
                .expect("peer_client");
        let client_status = process::wait(client);
        if let Some((server, _, _)) = server {
            let server_status = process::wait(server);
            assert_eq!(
                server_status,
                process::ExitStatus::Exited(0),
                "peer_server case {case}: {server_status:?}"
            );
        }
        let elapsed = crate::time::uptime_ms() - t0;
        assert_eq!(
            client_status,
            process::ExitStatus::Exited(0),
            "peer_client case {case}: {client_status:?} after {elapsed} ms"
        );
        assert!(
            elapsed < CASE_BOUND_MS,
            "peer_client case {case} took {elapsed} ms: the dead peer was not reported at once"
        );
        took[case as usize - 1] = elapsed;
    }
    log::info!(
        "ipc peer closed selftest ok (server died holding the request, with it queued, with a sender blocked; local cases; ms per case {took:?}, client waits 30000)"
    );
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
