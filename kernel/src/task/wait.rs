// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Wait queue: blocks the current thread until a condition holds and someone
//! calls `wake_all`/`wake_one`, with an optional deadline.
//!
//! Checking the condition, enqueueing and the context switch happen with
//! interrupts disabled, so a `wake` from a handler never lands between
//! "check" and "sleep": it finds the thread already asleep or blocked and
//! wakes it.

use super::scheduler;
use crate::sync::IrqLock;
use alloc::collections::VecDeque;
use aricode_sched::Tid;

pub struct WaitQueue {
    waiters: IrqLock<VecDeque<Tid>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            waiters: IrqLock::new(VecDeque::new()),
        }
    }

    /// Blocks until `cond()` is true. `cond` is evaluated under the lock.
    pub fn wait_until(&self, cond: impl FnMut() -> bool) {
        let timed_out = self.wait_until_deadline(cond, None);
        debug_assert!(!timed_out);
    }

    /// Like `wait_until`, but with a deadline in ticks: returns `true` if the
    /// deadline expired without the condition holding. With `Some(deadline)`
    /// the thread sleeps until that tick and an early `wake` wakes it sooner.
    pub fn wait_until_deadline(
        &self,
        mut cond: impl FnMut() -> bool,
        deadline: Option<u64>,
    ) -> bool {
        use crate::sync::KernelIrq;
        use aricode_sync::IrqPolicy;
        let me = scheduler::current_tid();
        loop {
            // Interrupts disabled from the check until the context switch: a
            // `wake` from a handler on this CPU can then only arrive once the
            // thread is asleep or blocked. A `wake` from another CPU in that
            // window stays pending in the policy and turns the block into a
            // yield (Phase 32).
            let irq = KernelIrq::save_and_disable();
            {
                let mut w = self.waiters.lock();
                // When re-evaluating after an early wake-up or a deadline, the
                // previous entry is stale: this avoids spurious wake-ups for
                // whoever later waits on another queue.
                w.retain(|t| *t != me);
                if cond() {
                    KernelIrq::restore(irq);
                    return false;
                }
                // A process asked to terminate does not go back to sleep: the
                // wait expires and the syscall terminates it on exit.
                if crate::user::process::current_kill_requested() {
                    KernelIrq::restore(irq);
                    return true;
                }
                if deadline.is_some_and(|d| crate::time::ticks() >= d) {
                    KernelIrq::restore(irq);
                    return true;
                }
                w.push_back(me);
            }
            match deadline {
                Some(d) => scheduler::sleep_until(d),
                None => scheduler::block_current(),
            }
            KernelIrq::restore(irq);
        }
    }

    /// Registers `me` once (dropping a stale entry first).
    fn register(&self, me: Tid) {
        let mut w = self.waiters.lock();
        w.retain(|t| *t != me);
        w.push_back(me);
    }

    /// Removes `me` if it is still registered.
    fn unregister(&self, me: Tid) {
        self.waiters.lock().retain(|t| *t != me);
    }

    /// `tid` is registered (for the self-tests: a finished wait leaves no
    /// entry behind, ADR 0044).
    pub fn contains(&self, tid: Tid) -> bool {
        self.waiters.lock().contains(&tid)
    }

    /// Wakes all waiters.
    pub fn wake_all(&self) {
        let waiters: VecDeque<Tid> = core::mem::take(&mut *self.waiters.lock());
        for tid in waiters {
            // If the thread has not blocked yet, it will retry on its own.
            let _ = scheduler::wake(tid);
        }
    }

    /// Wakes one (the oldest).
    pub fn wake_one(&self) {
        let tid = self.waiters.lock().pop_front();
        if let Some(t) = tid {
            let _ = scheduler::wake(t);
        }
    }
}

/// Like `WaitQueue::wait_until_deadline`, but woken by a `wake` on any of
/// `queues` (ADR 0044: one wait for the endpoints and the interrupt lines
/// of a service). Returns `true` if the deadline expired (or the process
/// is being terminated) without the condition holding.
///
/// The thread registers on every queue before it evaluates `cond`, and
/// `cond` runs outside the queues' locks (holding several at once would
/// need an order between them). No wakeup is lost: a waker changes the
/// state and then takes the queue's lock to wake its waiters. If it takes
/// that lock after this thread registered, it finds the thread there; if
/// before, its change happened before the registration (the lock orders
/// both) and `cond` sees it. A wake that arrives between the check and the
/// block turns the block into a yield, as in the single-queue wait. On
/// return the thread is removed from every queue, so no stale entry wakes
/// it later from a wait on another object.
pub fn wait_any_until_deadline(
    queues: &[&WaitQueue],
    mut cond: impl FnMut() -> bool,
    deadline: Option<u64>,
) -> bool {
    use crate::sync::KernelIrq;
    use aricode_sync::IrqPolicy;
    // Something is ready already, or nothing may be waited for (a poll
    // with timeout 0, a process being terminated): no registration needed.
    if cond() {
        return false;
    }
    if deadline.is_some_and(|d| crate::time::ticks() >= d)
        || crate::user::process::current_kill_requested()
    {
        return true;
    }
    let me = scheduler::current_tid();
    let timed_out = loop {
        // Interrupts disabled from the registration until the context
        // switch, as in `wait_until_deadline`: a handler on this CPU (an
        // interrupt line firing) can only wake the thread once it is
        // asleep or blocked.
        let irq = KernelIrq::save_and_disable();
        for q in queues {
            q.register(me);
        }
        if cond() {
            KernelIrq::restore(irq);
            break false;
        }
        if deadline.is_some_and(|d| crate::time::ticks() >= d)
            || crate::user::process::current_kill_requested()
        {
            KernelIrq::restore(irq);
            break true;
        }
        match deadline {
            Some(d) => scheduler::sleep_until(d),
            None => scheduler::block_current(),
        }
        KernelIrq::restore(irq);
    };
    for q in queues {
        q.unregister(me);
    }
    timed_out
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}
