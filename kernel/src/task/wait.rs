// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
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

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}
