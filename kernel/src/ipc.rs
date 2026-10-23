// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Endpoint IPC (Phase 14): bounded queues of small messages with handle
//! transfer, blocking with timeout and wake-up through wait queues. The
//! centerpiece of the microkernel (section 23).

use crate::hw::IrqLine;
use crate::object::{Cap, ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use aricode_abi::{Error, IPC_MAX_ANY_IRQS, IPC_QUEUE_CAPACITY, TIMEOUT_INFINITE};
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
            // The oldest waiter, whether it waits on this endpoint alone or
            // on a set that holds it (ADR 0044: `receive_any` registers on
            // the queue of every endpoint in its set).
            self.receivers.wake_one();
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

/// A member of the set `receive_any` waits on (ADR 0044).
pub enum Member<'a> {
    Endpoint(&'a Endpoint),
    Irq(&'a IrqLine),
}

impl Member<'_> {
    fn is_ready(&self) -> bool {
        match self {
            Member::Endpoint(e) => e.has_message(),
            Member::Irq(l) => l.is_pending(),
        }
    }

    /// Takes what made the member ready, if it still is.
    fn take(&self) -> Option<Ready> {
        match self {
            Member::Endpoint(e) => e.try_receive().map(Ready::Message),
            Member::Irq(l) => l.take_pending().map(Ready::Irq),
        }
    }
}

/// What `receive_any` took from the member that was ready.
pub enum Ready {
    Message(Message),
    /// Interrupts accumulated on the line since they were last taken.
    Irq(u32),
}

/// Members of one `receive_any` set at most: its endpoints and lines.
const MAX_MEMBERS: usize = aricode_abi::IPC_MAX_ANY + IPC_MAX_ANY_IRQS;

/// Waits until the first member of `members` (in order) is ready — an
/// endpoint with a message (ADR 0020) or an interrupt line that fired
/// (ADR 0044) — and returns its index and what it gave: the oldest message,
/// or the interrupts accumulated on the line (taken, as `irq_wait` does).
/// Waits up to `timeout_ms`. `PeerClosed` only when no member is ready and
/// every endpoint is empty and has lost its send side: one closed endpoint
/// among live ones is just one that stays empty, and a line never closes
/// but serves nobody once its service's endpoints are gone. At most
/// `IPC_MAX_ANY + IPC_MAX_ANY_IRQS` members (`Invalid` beyond).
///
/// The thread waits on the receive queue of each endpoint in the set and
/// on each line's own queue: a send wakes the oldest waiter of its own
/// endpoint only, an interrupt the waiters of its own line, never the
/// other services in the system (finding E of the performance audit of
/// 731c398). A line's count is a latch: an interrupt that fired before the
/// wait leaves it ready, so the wait returns at once.
///
/// A send's `wake_one` may pick this thread while it is already awake for
/// another member, and the thread then takes that other member: the
/// wakeup meant for the endpoint would be lost for its other waiters. So
/// once the thread has left every queue and taken its member, it passes
/// the wakeup on (`wake_one`) for every other endpoint of the set that
/// still holds a message.
pub fn receive_any(members: &[Member], timeout_ms: u64) -> Result<(usize, Ready), Error> {
    let deadline = Endpoint::deadline(timeout_ms);
    if members.len() > MAX_MEMBERS {
        return Err(Error::Invalid);
    }
    let mut queues: [&WaitQueue; MAX_MEMBERS] = [&NO_QUEUE; MAX_MEMBERS];
    for (q, m) in queues.iter_mut().zip(members) {
        *q = match m {
            Member::Endpoint(e) => &e.receivers,
            Member::Irq(l) => l.waiters(),
        };
    }
    let queues = &queues[..members.len()];
    let has_endpoints = members.iter().any(|m| matches!(m, Member::Endpoint(_)));
    let all_closed = || {
        has_endpoints
            && members.iter().all(|m| match m {
                Member::Endpoint(e) => e.drained_and_closed(),
                Member::Irq(_) => true,
            })
    };
    loop {
        let timed_out = crate::task::wait::wait_any_until_deadline(
            queues,
            || all_closed() || members.iter().any(Member::is_ready),
            deadline,
        );
        for (i, m) in members.iter().enumerate() {
            if let Some(ready) = m.take() {
                pass_wakeups_on(members, i);
                return Ok((i, ready));
            }
        }
        if all_closed() {
            return Err(Error::PeerClosed);
        }
        if timed_out {
            return Err(Error::Timeout);
        }
        // Another receiver took the message or the interrupts: wait again.
    }
}

/// See `receive_any`: wakes the next waiter of every endpoint in the set,
/// other than member `taken`, that still holds a message.
fn pass_wakeups_on(members: &[Member], taken: usize) {
    for (i, m) in members.iter().enumerate() {
        if let Member::Endpoint(e) = m {
            if i != taken && e.has_message() {
                e.receivers.wake_one();
            }
        }
    }
}

/// Filler of `receive_any`'s queue array past its members; never waited on.
static NO_QUEUE: WaitQueue = WaitQueue::new();

/// The self-test's endpoint and line, for its helper threads.
static SELFTEST_OBJECTS: IrqLock<Option<(ObjectRef, ObjectRef)>> = IrqLock::new(None);
/// Round the stress helper may fire for (0: none yet).
static SELFTEST_ARMED: AtomicUsize = AtomicUsize::new(0);
/// The second endpoint of the hand-on case (6) of `wait_set_selftest`.
static SELFTEST_SECOND: IrqLock<Option<ObjectRef>> = IrqLock::new(None);
/// What the single receiver of case 6 got: its byte (`u64::MAX` for no
/// message, 0 until it returns) and how long it waited.
static SELFTEST_SINGLE_GOT: AtomicU64 = AtomicU64::new(0);
static SELFTEST_SINGLE_MS: AtomicU64 = AtomicU64::new(0);

fn endpoint_of_object(obj: &ObjectRef) -> &Endpoint {
    match &obj.kind {
        ObjectKind::Endpoint(e) => e,
        _ => unreachable!(),
    }
}

/// Helper of `wait_set_selftest`, case 6: after 20 ms (so the self-test
/// registered on the first endpoint before it), waits on that endpoint
/// alone, as a second receiver of it.
extern "C" fn selftest_single_receiver(_: usize) {
    crate::task::scheduler::sleep_ms(20);
    let (ep, _) = selftest_objects();
    let t0 = crate::time::uptime_ms();
    let got = match endpoint_of_object(&ep).receive(5_000) {
        Ok(m) => m.data.first().map_or(u64::MAX, |b| *b as u64),
        Err(_) => u64::MAX,
    };
    SELFTEST_SINGLE_MS.store(crate::time::uptime_ms() - t0, Ordering::Release);
    SELFTEST_SINGLE_GOT.store(got, Ordering::Release);
}

/// Helper of `wait_set_selftest`, case 6: after 60 ms, sends to the
/// second endpoint and then to the first, back to back.
extern "C" fn selftest_sender(_: usize) {
    crate::task::scheduler::sleep_ms(60);
    let (ep, _) = selftest_objects();
    let second = SELFTEST_SECOND.lock().clone().expect("second endpoint");
    for (obj, byte) in [(&second, 21u8), (&ep, 22u8)] {
        let _ = endpoint_of_object(obj).send_with(0, || {
            Ok(Message {
                data: alloc::vec![byte],
                handles: Vec::new(),
            })
        });
    }
}

fn selftest_objects() -> (ObjectRef, ObjectRef) {
    SELFTEST_OBJECTS
        .lock()
        .clone()
        .expect("the self-test publishes its objects first")
}

/// Helper of `wait_set_selftest`: after 30 ms, fires the line (`action`
/// 1) or sends a message (otherwise), while the self-test is blocked.
extern "C" fn selftest_later(action: usize) {
    crate::task::scheduler::sleep_ms(30);
    let (ep, line) = selftest_objects();
    match (&ep.kind, &line.kind, action) {
        (_, ObjectKind::Irq(l), 1) => l.fire(),
        (ObjectKind::Endpoint(e), _, _) => {
            let _ = e.send_with(0, || {
                Ok(Message {
                    data: alloc::vec![9],
                    handles: Vec::new(),
                })
            });
        }
        _ => unreachable!(),
    }
}

/// Helper of `wait_set_selftest`: fires the line once per round, as soon
/// as the self-test arms it and after a delay that varies with the round,
/// so the interrupt lands before, during and after the waiter's block.
extern "C" fn selftest_firer(rounds: usize) {
    let (_, line) = selftest_objects();
    let ObjectKind::Irq(l) = &line.kind else {
        unreachable!()
    };
    for round in 1..=rounds {
        while SELFTEST_ARMED.load(Ordering::Acquire) < round {
            crate::task::scheduler::yield_now();
        }
        for _ in 0..(round % 16) * 64 {
            core::hint::spin_loop();
        }
        l.fire();
    }
}

/// Wait-set self-test (`test=wait_set`, ADR 0044): `receive_any` over
/// endpoints and an interrupt line. Interrupts latched before the wait
/// return at once with their count; the first ready member in list order
/// wins, whichever its kind; the deadline holds when nothing comes; a wait
/// blocked on the set wakes for an interrupt or a message from another
/// thread; no wakeup is lost when the line fires around the moment the
/// waiter blocks (from another CPU when there is one); a send's wakeup
/// that picked the set's waiter while it was awake for another member is
/// passed on to the endpoint's other receiver; `PeerClosed` keeps its
/// meaning with a line in the set; and a finished wait leaves no
/// registration behind on any queue.
pub fn wait_set_selftest() {
    use crate::object::KernelObject;
    use crate::task::scheduler;
    use crate::time::{ticks, uptime_ms, MS_PER_TICK};

    const STRESS_ROUNDS: usize = 200;
    let ep_obj = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
    let line_obj = crate::hw::quiet_line_for_selftest();
    *SELFTEST_OBJECTS.lock() = Some((ep_obj.clone(), line_obj.clone()));
    let (ObjectKind::Endpoint(ep), ObjectKind::Irq(line)) = (&ep_obj.kind, &line_obj.kind) else {
        unreachable!()
    };
    let set = [Member::Endpoint(ep), Member::Irq(line)];
    let send = |byte: u8| {
        ep.send_with(0, || {
            Ok(Message {
                data: alloc::vec![byte],
                handles: Vec::new(),
            })
        })
        .expect("room in the queue")
    };
    let describe = |r: &Result<(usize, Ready), Error>| match r {
        Ok((i, Ready::Irq(n))) => alloc::format!("member {i}: {n} interrupts"),
        Ok((i, Ready::Message(m))) => alloc::format!("member {i}: message {:?}", m.data),
        Err(e) => alloc::format!("{e:?}"),
    };
    let me = scheduler::current_tid();
    let registered = || ep.receivers.contains(me) || line.waiters().contains(me);

    // 1. Latched before the wait: at once, with the count, and taken.
    line.fire();
    line.fire();
    let t0 = ticks();
    let r = receive_any(&set, 5_000);
    assert!(
        matches!(r, Ok((1, Ready::Irq(2)))),
        "latched interrupts: {}",
        describe(&r)
    );
    assert!(ticks() - t0 <= 1, "a latched interrupt must not wait");
    assert!(!line.is_pending(), "the wait takes what it reports");

    // 2. Both ready: list order decides, and the other one stays ready.
    send(7);
    line.fire();
    let (a, b, c) = (
        receive_any(&set, 0),
        receive_any(&set, 0),
        receive_any(&set, 0),
    );
    assert!(
        matches!(&a, Ok((0, Ready::Message(m))) if m.data == [7])
            && matches!(b, Ok((1, Ready::Irq(1))))
            && matches!(c, Err(Error::Timeout)),
        "endpoint listed first: {}, {}, {}",
        describe(&a),
        describe(&b),
        describe(&c)
    );
    let reversed = [Member::Irq(line), Member::Endpoint(ep)];
    send(8);
    line.fire();
    let (a, b) = (receive_any(&reversed, 0), receive_any(&reversed, 0));
    assert!(
        matches!(a, Ok((0, Ready::Irq(1))))
            && matches!(&b, Ok((1, Ready::Message(m))) if m.data == [8]),
        "line listed first: {}, {}",
        describe(&a),
        describe(&b)
    );

    // 3. Nothing comes: the deadline holds, neither early nor stuck.
    let t0 = uptime_ms();
    let r = receive_any(&set, 50);
    let waited = uptime_ms() - t0;
    assert!(
        matches!(r, Err(Error::Timeout)),
        "nothing ready: {}",
        describe(&r)
    );
    assert!(
        waited + MS_PER_TICK >= 50 && waited < 1_000,
        "a 50 ms deadline took {waited} ms"
    );
    assert!(!registered(), "a timed-out wait left a registration");

    // 4. Blocked on the set: another thread's interrupt or message wakes
    // it long before its deadline.
    let mut woken_ms = [0u64; 2];
    for (action, woken) in [1usize, 2].into_iter().zip(woken_ms.iter_mut()) {
        let t0 = uptime_ms();
        let helper = scheduler::spawn("wait_set_later", selftest_later, action).expect("helper");
        let r = receive_any(&set, 5_000);
        *woken = uptime_ms() - t0;
        let expected = match (&r, action) {
            (Ok((1, Ready::Irq(1))), 1) => true,
            (Ok((0, Ready::Message(m))), 2) => m.data == [9],
            _ => false,
        };
        assert!(
            expected && *woken < 2_000,
            "woken by action {action} after {} ms: {}",
            *woken,
            describe(&r)
        );
        assert!(!registered(), "a woken wait left a registration");
        while scheduler::state(helper).is_some() {
            scheduler::yield_now();
        }
    }

    // 5. No lost wakeup: every round's interrupt ends a wait whose
    // deadline is far away, wherever it lands relative to the block.
    let cpus = crate::cpu::online_count();
    let firer = if cpus > 1 {
        scheduler::spawn_on("wait_set_firer", selftest_firer, STRESS_ROUNDS, 1)
    } else {
        scheduler::spawn("wait_set_firer", selftest_firer, STRESS_ROUNDS)
    }
    .expect("firer");
    let only_line = [Member::Irq(line)];
    let t0 = uptime_ms();
    let mut interrupts = 0;
    for round in 1..=STRESS_ROUNDS {
        SELFTEST_ARMED.store(round, Ordering::Release);
        match receive_any(&only_line, 5_000) {
            Ok((0, Ready::Irq(n))) => interrupts += n as usize,
            r => panic!("round {round}: {}", describe(&r)),
        }
        // A later round's interrupt never arrives early: the helper waits
        // for each arming, so each wait sees exactly its own.
    }
    let stress_ms = uptime_ms() - t0;
    while scheduler::state(firer).is_some() {
        scheduler::yield_now();
    }
    assert_eq!(interrupts, STRESS_ROUNDS, "every interrupt reported once");
    assert!(!line.is_pending() && !registered());

    // 6. Hand-on: the self-test waits on [second, first] and registered on
    // the first before a single receiver of it did; the sender then sends
    // to the second and to the first back to back. The first's `wake_one`
    // may pick the self-test, already awake for the second (one CPU: it
    // always does); the self-test takes the second (list order) and must
    // pass the first's wakeup on, or the single receiver would wait for
    // its 5 s deadline with a message queued.
    let second_obj = KernelObject::new(ObjectKind::Endpoint(Endpoint::new()));
    *SELFTEST_SECOND.lock() = Some(second_obj.clone());
    let second = endpoint_of_object(&second_obj);
    SELFTEST_SINGLE_GOT.store(0, Ordering::Release);
    let single = scheduler::spawn("wait_set_single", selftest_single_receiver, 0).expect("single");
    let sender = scheduler::spawn("wait_set_sender", selftest_sender, 0).expect("sender");
    let r = receive_any(&[Member::Endpoint(second), Member::Endpoint(ep)], 5_000);
    assert!(
        matches!(&r, Ok((0, Ready::Message(m))) if m.data == [21]),
        "hand-on, the set: {}",
        describe(&r)
    );
    while scheduler::state(single).is_some() || scheduler::state(sender).is_some() {
        scheduler::yield_now();
    }
    let single_got = SELFTEST_SINGLE_GOT.load(Ordering::Acquire);
    let single_ms = SELFTEST_SINGLE_MS.load(Ordering::Acquire);
    assert!(
        single_got == 22 && single_ms < 1_000,
        "hand-on: the single receiver got {single_got} after {single_ms} ms (22 well before its 5000 ms deadline expected)"
    );
    assert!(!ep.has_message() && !second.has_message() && !registered());
    *SELFTEST_SECOND.lock() = None;

    // 7. PeerClosed: a set whose endpoints are all gone still reports a
    // pending interrupt first, then PeerClosed; a set of lines alone only
    // ever times out.
    if ep.shut_down() {
        ep.settle();
    }
    line.fire();
    let (a, b) = (receive_any(&set, 1_000), receive_any(&set, 1_000));
    let c = receive_any(&only_line, 20);
    assert!(
        matches!(a, Ok((1, Ready::Irq(1))))
            && matches!(b, Err(Error::PeerClosed))
            && matches!(c, Err(Error::Timeout)),
        "closed endpoint: {}, {}, {}",
        describe(&a),
        describe(&b),
        describe(&c)
    );
    assert!(!registered());
    *SELFTEST_OBJECTS.lock() = None;
    log::info!(
        "wait set selftest ok (latched interrupt at once, list order between a message and an interrupt, 50 ms deadline in {waited} ms, woken by an interrupt in {} ms and by a message in {} ms, {STRESS_ROUNDS} interrupts around the block on {cpus} CPUs without a lost wakeup in {stress_ms} ms, a wakeup taken for another member passed on to a single receiver in {single_ms} ms, PeerClosed with a line in the set)",
        woken_ms[0],
        woken_ms[1]
    );
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
