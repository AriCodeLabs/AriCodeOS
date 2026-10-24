// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Scheduling policy (Phases 9, 10 and 32): round robin over thread
//! identifiers, with states, tick-based sleep, blocking, exit,
//! multiple CPUs and affinity.
//!
//! It knows nothing about stacks or registers: the kernel asks it "who is next
//! on this CPU?" and performs the context switch. This way the policy is tested
//! deterministically on the host.
//!
//! With multiple CPUs there is a single shared ready queue and, per CPU, a running
//! thread and its own idle thread (idle is never queued). A thread with
//! affinity is only taken by the given CPU. A `wake` arriving while the
//! thread is still `Running` (another CPU wakes it between "joining the
//! wait queue" and "blocking") is kept pending and turns the next
//! block into a yield: no wakeup is lost.

#![no_std]

extern crate alloc;

pub mod idle;

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tid(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Blocked,
    Sleeping,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedError {
    UnknownThread,
    /// The operation is not valid in the thread's current state.
    WrongState,
    UnknownCpu,
}

/// One timer tick of a CPU's quantum: `left` is what the running thread
/// still has; returns what it has after the tick and whether the quantum
/// expired. On expiry the quantum starts again at once, whether or not the
/// scheduler then switches threads (incident 0023): a thread alone on its
/// CPU keeps getting an expiry every `quantum` ticks, so a thread that
/// becomes ready for that CPU later waits one quantum at most. Saturating:
/// a counter at zero cannot wrap.
pub fn quantum_tick(left: u32, quantum: u32) -> (u32, bool) {
    match left.saturating_sub(1) {
        0 => (quantum.max(1), true),
        n => (n, false),
    }
}

struct CpuSlot {
    current: Tid,
    idle: Tid,
}

pub struct RoundRobin {
    states: BTreeMap<Tid, State>,
    ready: VecDeque<Tid>,
    /// (wakeup tick, thread), in no particular order.
    sleeping: Vec<(u64, Tid)>,
    cpus: Vec<CpuSlot>,
    /// Thread -> the only CPU it may run on.
    affinity: BTreeMap<Tid, usize>,
    /// Wakeups received by threads that were still `Running`.
    pending_wake: BTreeSet<Tid>,
    next_tid: u32,
}

impl RoundRobin {
    /// `current` is the thread already running on CPU 0 (the boot
    /// context); `idle` is what that CPU picks when nothing is ready.
    /// Both get registered: `current` as `Running`, `idle` as
    /// `Ready` but outside the queue (it is never queued).
    pub fn new(current: Tid, idle: Tid) -> Self {
        let mut states = BTreeMap::new();
        states.insert(current, State::Running);
        states.insert(idle, State::Ready);
        Self {
            states,
            ready: VecDeque::new(),
            sleeping: Vec::new(),
            cpus: alloc::vec![CpuSlot { current, idle }],
            affinity: BTreeMap::new(),
            pending_wake: BTreeSet::new(),
            next_tid: current.0.max(idle.0) + 1,
        }
    }

    /// Registers CPU `cpu` (the next one in order) with its idle thread, which
    /// is already running on it.
    pub fn add_cpu(&mut self, cpu: usize, idle: Tid) -> Result<(), SchedError> {
        if cpu != self.cpus.len() {
            return Err(SchedError::UnknownCpu);
        }
        self.states.insert(idle, State::Running);
        self.cpus.push(CpuSlot {
            current: idle,
            idle,
        });
        Ok(())
    }

    pub fn cpu_count(&self) -> usize {
        self.cpus.len()
    }

    /// New identifier, unique for the lifetime of the scheduler.
    pub fn allocate_tid(&mut self) -> Tid {
        let t = Tid(self.next_tid);
        self.next_tid += 1;
        t
    }

    /// Registers a new thread and puts it at the end of the queue.
    pub fn add(&mut self, tid: Tid) {
        self.states.insert(tid, State::Ready);
        self.ready.push_back(tid);
    }

    /// Sets (or clears) the CPU `tid` may run on.
    pub fn set_affinity(&mut self, tid: Tid, cpu: Option<usize>) -> Result<(), SchedError> {
        if !self.states.contains_key(&tid) {
            return Err(SchedError::UnknownThread);
        }
        match cpu {
            Some(c) if c >= self.cpus.len() => Err(SchedError::UnknownCpu),
            Some(c) => {
                self.affinity.insert(tid, c);
                Ok(())
            }
            None => {
                self.affinity.remove(&tid);
                Ok(())
            }
        }
    }

    pub fn affinity(&self, tid: Tid) -> Option<usize> {
        self.affinity.get(&tid).copied()
    }

    pub fn current(&self, cpu: usize) -> Tid {
        self.cpus[cpu].current
    }
    pub fn idle(&self, cpu: usize) -> Tid {
        self.cpus[cpu].idle
    }
    /// `true` if the CPU is running its idle thread.
    pub fn is_idle(&self, cpu: usize) -> bool {
        self.cpus[cpu].current == self.cpus[cpu].idle
    }
    pub fn state(&self, tid: Tid) -> Option<State> {
        self.states.get(&tid).copied()
    }
    pub fn ready_len(&self) -> usize {
        self.ready.len()
    }
    pub fn thread_count(&self) -> usize {
        self.states.len()
    }

    fn eligible(&self, tid: Tid, cpu: usize) -> bool {
        self.affinity.get(&tid).is_none_or(|c| *c == cpu)
    }

    /// `true` if there is some ready thread `cpu` can run.
    pub fn has_ready_for(&self, cpu: usize) -> bool {
        self.ready.iter().any(|t| self.eligible(*t, cpu))
    }

    /// Idle CPUs to wake (IPI) for the ready queue, seen from CPU `me`:
    /// each ready thread, in queue order, claims the first idle CPU other
    /// than `me`, accepted by `online` and not yet claimed, that may run it
    /// (its own CPU if pinned). So a batch of wakeups wakes as many idle
    /// CPUs as can take a thread, and a pinned thread wakes its CPU, not
    /// one that cannot run it (incident 0023). Returned as a bit mask (bit
    /// `c` for CPU `c`; CPUs from 64 on are never chosen), so the timer
    /// interrupt that calls it allocates nothing.
    pub fn idle_cpus_to_wake(&self, me: usize, online: impl Fn(usize) -> bool) -> u64 {
        let idle = (0..self.cpus.len().min(64))
            .filter(|&c| c != me && self.is_idle(c) && online(c))
            .fold(0u64, |m, c| m | 1 << c);
        let mut chosen = 0u64;
        for &t in &self.ready {
            let free = idle & !chosen;
            if free == 0 {
                break;
            }
            if let Some(c) = (0..64).find(|&c| free & 1 << c != 0 && self.eligible(t, c)) {
                chosen |= 1 << c;
            }
        }
        chosen
    }

    /// Removes from the queue the first thread `cpu` can run.
    fn pop_ready_for(&mut self, cpu: usize) -> Option<Tid> {
        let pos = self.ready.iter().position(|t| self.eligible(*t, cpu))?;
        self.ready.remove(pos)
    }

    /// Next thread to run on `cpu` after leaving its current thread in
    /// `state`. Returns `None` if nothing changes (the same thread continues).
    fn switch(&mut self, cpu: usize, current_state: State) -> Option<Tid> {
        let cur = self.cpus[cpu].current;
        let idle = self.cpus[cpu].idle;
        // A pending wakeup turns the block or sleep into a yield.
        let current_state = match current_state {
            State::Blocked | State::Sleeping if self.pending_wake.remove(&cur) => {
                self.sleeping.retain(|(_, t)| *t != cur);
                State::Ready
            }
            s => s,
        };
        if current_state == State::Ready && cur != idle {
            self.ready.push_back(cur);
        }
        self.states.insert(cur, current_state);
        let next = self.pop_ready_for(cpu).unwrap_or(idle);
        self.states.insert(next, State::Running);
        self.cpus[cpu].current = next;
        if next == cur {
            None
        } else {
            Some(next)
        }
    }

    /// The current thread of `cpu` yields the CPU (goes back to the end of the queue).
    pub fn yield_current(&mut self, cpu: usize) -> Option<Tid> {
        self.switch(cpu, State::Ready)
    }

    /// Quantum-expiry preemption: same as `yield`, but idle is not queued.
    pub fn preempt(&mut self, cpu: usize) -> Option<Tid> {
        self.switch(cpu, State::Ready)
    }

    /// The current thread of `cpu` blocks until someone wakes it.
    pub fn block_current(&mut self, cpu: usize) -> Option<Tid> {
        self.switch(cpu, State::Blocked)
    }

    /// Wakes a blocked thread and queues it.
    pub fn unblock(&mut self, tid: Tid) -> Result<(), SchedError> {
        match self.states.get(&tid) {
            None => Err(SchedError::UnknownThread),
            Some(State::Blocked) => {
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                Ok(())
            }
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// Wakes a blocked or sleeping thread (before its deadline) and queues it.
    /// Serves wait queues with timeout: the thread sleeps with a deadline and an
    /// early `wake` removes it from the sleep list. If the thread is still
    /// `Running` (about to block on another CPU), the wakeup is kept
    /// pending and its next block does not block.
    pub fn wake(&mut self, tid: Tid) -> Result<(), SchedError> {
        match self.states.get(&tid) {
            None => Err(SchedError::UnknownThread),
            Some(State::Blocked) => {
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                Ok(())
            }
            Some(State::Sleeping) => {
                self.sleeping.retain(|(_, t)| *t != tid);
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                Ok(())
            }
            Some(State::Running) => {
                self.pending_wake.insert(tid);
                Ok(())
            }
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// The current thread of `cpu` sleeps until tick `until`.
    pub fn sleep_current(&mut self, cpu: usize, until: u64) -> Option<Tid> {
        let cur = self.cpus[cpu].current;
        self.sleeping.push((until, cur));
        self.switch(cpu, State::Sleeping)
    }

    /// Queues the threads whose wakeup tick has arrived. Returns how many.
    pub fn wake_due(&mut self, now: u64) -> usize {
        let mut woken = 0;
        let mut i = 0;
        while i < self.sleeping.len() {
            if self.sleeping[i].0 <= now {
                let (_, tid) = self.sleeping.swap_remove(i);
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                woken += 1;
            } else {
                i += 1;
            }
        }
        woken
    }

    /// The current thread of `cpu` exits. Returns the next one (there is always a switch).
    pub fn exit_current(&mut self, cpu: usize) -> Tid {
        assert!(!self.is_idle(cpu), "the idle thread cannot exit");
        let cur = self.cpus[cpu].current;
        self.pending_wake.remove(&cur);
        self.affinity.remove(&cur);
        self.switch(cpu, State::Terminated)
            .expect("exit always switches threads")
    }

    /// Removes a terminated thread from the registry (to free its resources).
    pub fn reap(&mut self, tid: Tid) -> Result<(), SchedError> {
        match self.states.get(&tid) {
            None => Err(SchedError::UnknownThread),
            Some(State::Terminated) => {
                self.states.remove(&tid);
                Ok(())
            }
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// Terminated threads awaiting `reap`.
    pub fn terminated(&self) -> impl Iterator<Item = Tid> + '_ {
        self.states
            .iter()
            .filter(|(_, s)| **s == State::Terminated)
            .map(|(t, _)| *t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Tid = Tid(0);
    const IDLE: Tid = Tid(1);

    fn rr() -> RoundRobin {
        RoundRobin::new(MAIN, IDLE)
    }

    #[test]
    fn round_robin_order_is_fair_and_cyclic() {
        let mut s = rr();
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        assert_eq!((a, b), (Tid(2), Tid(3)));
        s.add(a);
        s.add(b);
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.yield_current(0), Some(b));
        assert_eq!(s.yield_current(0), Some(MAIN));
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Ready));
        assert_eq!(s.state(a), Some(State::Running));
    }

    #[test]
    fn idle_runs_when_nothing_is_ready_and_is_never_queued() {
        let mut s = rr();
        assert_eq!(s.block_current(0), Some(IDLE));
        assert_eq!(s.current(0), IDLE);
        assert!(s.is_idle(0));
        assert_eq!(
            s.yield_current(0),
            None,
            "idle continues if nobody else is there"
        );
        assert_eq!(s.ready_len(), 0);
        s.unblock(MAIN).unwrap();
        assert!(s.has_ready_for(0));
        assert_eq!(s.yield_current(0), Some(MAIN));
        assert_eq!(s.state(IDLE), Some(State::Ready));
        assert_eq!(s.ready_len(), 0, "idle is not queued on yield");
    }

    #[test]
    fn block_and_unblock() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.block_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Blocked));
        assert_eq!(s.unblock(MAIN), Ok(()));
        assert_eq!(s.unblock(MAIN), Err(SchedError::WrongState));
        assert_eq!(s.unblock(Tid(99)), Err(SchedError::UnknownThread));
        assert_eq!(s.yield_current(0), Some(MAIN));
    }

    #[test]
    fn sleep_wakes_at_tick_in_queue_order() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.sleep_current(0, 10), Some(a)); // MAIN sleeps until 10
        assert_eq!(s.sleep_current(0, 5), Some(IDLE)); // a sleeps until 5
        assert_eq!(s.wake_due(4), 0);
        assert_eq!(s.wake_due(5), 1);
        assert_eq!(s.state(a), Some(State::Ready));
        assert_eq!(s.state(MAIN), Some(State::Sleeping));
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.wake_due(100), 1);
        assert_eq!(s.yield_current(0), Some(MAIN));
    }

    #[test]
    fn wake_handles_blocked_and_sleeping() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.sleep_current(0, 100), Some(a)); // MAIN sleeps
        assert_eq!(
            s.wake(MAIN),
            Ok(()),
            "sleeping -> ready before the deadline"
        );
        assert_eq!(s.wake_due(100), 0, "no longer in the sleep list");
        assert_eq!(s.wake(MAIN), Err(SchedError::WrongState), "already ready");
        assert_eq!(s.block_current(0), Some(MAIN)); // a blocks
        assert_eq!(s.wake(a), Ok(()));
        assert_eq!(s.state(a), Some(State::Ready));
        assert_eq!(s.wake(Tid(77)), Err(SchedError::UnknownThread));
    }

    #[test]
    fn wake_of_running_thread_is_kept_until_it_blocks() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        // MAIN is still Running: the wakeup stays pending...
        assert_eq!(s.wake(MAIN), Ok(()));
        // ...and its block turns into a yield: back to the queue.
        assert_eq!(s.block_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Ready));
        assert_eq!(s.yield_current(0), Some(MAIN));
        // Consumed: the next block does block.
        assert_eq!(s.block_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Blocked));
        // Same with a sleep: it does not stay in the sleep list.
        assert_eq!(s.wake(a), Ok(()));
        assert_eq!(
            s.sleep_current(0, 50),
            None,
            "a yields but it is the only ready one"
        );
        assert_eq!(s.state(a), Some(State::Running));
        assert_eq!(s.wake_due(50), 0);
    }

    #[test]
    fn exit_and_reap() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.exit_current(0), MAIN);
        assert_eq!(s.state(a), Some(State::Terminated));
        assert_eq!(s.terminated().collect::<Vec<_>>(), [a]);
        assert_eq!(s.reap(MAIN), Err(SchedError::WrongState));
        assert_eq!(s.reap(a), Ok(()));
        assert_eq!(s.state(a), None);
        assert_eq!(s.thread_count(), 2);
    }

    #[test]
    #[should_panic(expected = "idle thread cannot exit")]
    fn idle_cannot_exit() {
        let mut s = rr();
        s.block_current(0);
        s.exit_current(0);
    }

    #[test]
    fn cpus_share_one_ready_queue_and_have_their_own_idle() {
        let mut s = rr();
        let idle1 = s.allocate_tid();
        assert_eq!(s.add_cpu(2, idle1), Err(SchedError::UnknownCpu), "in order");
        s.add_cpu(1, idle1).unwrap();
        assert_eq!(s.cpu_count(), 2);
        assert_eq!(s.current(1), idle1);
        assert!(s.is_idle(1));
        assert_eq!(s.state(idle1), Some(State::Running));
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        s.add(a);
        s.add(b);
        // CPU 1 takes the first one in the queue; CPU 0 the second.
        assert_eq!(s.yield_current(1), Some(a));
        assert_eq!(s.state(idle1), Some(State::Ready));
        assert_eq!(s.ready_len(), 1);
        assert_eq!(s.yield_current(0), Some(b));
        assert_eq!(s.ready_len(), 1, "main went back to the queue");
        // a exits on CPU 1: main follows; then idle1 once nothing is left.
        assert_eq!(s.exit_current(1), MAIN);
        assert_eq!(s.block_current(1), Some(idle1));
        assert_eq!(s.yield_current(1), None);
        assert!(s.is_idle(1));
        assert_eq!(s.thread_count(), 5);
    }

    #[test]
    fn affinity_restricts_which_cpu_takes_a_thread() {
        let mut s = rr();
        let idle1 = s.allocate_tid();
        s.add_cpu(1, idle1).unwrap();
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        s.add(a);
        s.add(b);
        assert_eq!(s.set_affinity(a, Some(1)), Ok(()));
        assert_eq!(s.set_affinity(a, Some(7)), Err(SchedError::UnknownCpu));
        assert_eq!(
            s.set_affinity(Tid(99), Some(0)),
            Err(SchedError::UnknownThread)
        );
        assert_eq!(s.affinity(a), Some(1));
        assert!(!s.has_ready_for(0) || s.ready_len() == 2);
        // CPU 0 skips a and takes b, even though a was earlier in the queue.
        assert_eq!(s.yield_current(0), Some(b));
        assert_eq!(s.yield_current(0), Some(MAIN));
        assert!(s.has_ready_for(1));
        assert_eq!(s.yield_current(1), Some(a));
        // Without affinity, any CPU will do.
        s.set_affinity(a, None).unwrap();
        assert_eq!(s.affinity(a), None);
        assert_eq!(s.yield_current(1), Some(b));
        assert_eq!(s.yield_current(0), Some(a));
        // A thread with affinity to another CPU does not stop this one from idling.
        s.set_affinity(b, Some(0)).unwrap();
        assert_eq!(s.exit_current(0), MAIN);
        // b yields on CPU 1 and it cannot take it again: it goes idle.
        assert_eq!(s.yield_current(1), Some(idle1));
        assert_eq!(s.state(b), Some(State::Ready), "b keeps waiting for CPU 0");
        assert!(!s.has_ready_for(1));
        assert!(s.has_ready_for(0));
    }

    /// Incident 0023: the old `fetch_sub(1) <= 1` expired at 1 -> 0 and,
    /// with nothing else ready (no switch, no reset), wrapped to u32::MAX
    /// on the next tick and never expired again.
    #[test]
    fn a_lone_thread_keeps_getting_quantum_expiries() {
        const Q: u32 = 2;
        let mut left = Q;
        let mut expiries = 0;
        for tick in 1..=1000u32 {
            let (next, expired) = quantum_tick(left, Q);
            assert!(next >= 1 && next <= Q, "tick {tick}: left {next}");
            if expired {
                expiries += 1;
                assert_eq!(tick % Q, 0, "expiry every {Q} ticks");
            }
            // Nothing else is ready: the scheduler does not switch, so
            // nobody resets the counter but `quantum_tick` itself.
            left = next;
        }
        assert_eq!(expiries, 1000 / Q);
        // A counter at zero (never set) expires and restarts, no wrap.
        assert_eq!(quantum_tick(0, Q), (Q, true));
        assert_eq!(quantum_tick(1, Q), (Q, true));
        assert_eq!(quantum_tick(Q, Q), (Q - 1, false));
        // A zero quantum still restarts at one tick.
        assert_eq!(quantum_tick(0, 0), (1, true));
    }

    fn four_cpus() -> RoundRobin {
        let mut s = rr();
        for cpu in 1..4 {
            let idle = s.allocate_tid();
            s.add_cpu(cpu, idle).unwrap();
        }
        s
    }

    /// The CPUs of an `idle_cpus_to_wake` mask, in order.
    fn cpus_of(mask: u64) -> Vec<usize> {
        (0..64).filter(|c| mask & 1 << c != 0).collect()
    }

    #[test]
    fn a_pinned_thread_wakes_its_own_idle_cpu() {
        let mut s = four_cpus();
        let t = s.allocate_tid();
        s.add(t);
        s.set_affinity(t, Some(3)).unwrap();
        // CPU 0 runs main; 1, 2 and 3 are idle. Only 3 can run t.
        assert_eq!(cpus_of(s.idle_cpus_to_wake(0, |_| true)), [3]);
        // Not online yet: nobody to wake.
        assert!(cpus_of(s.idle_cpus_to_wake(0, |c| c != 3)).is_empty());
        // Pinned to the CPU doing the wakeup (busy): no IPI helps.
        s.set_affinity(t, Some(0)).unwrap();
        assert!(cpus_of(s.idle_cpus_to_wake(0, |_| true)).is_empty());
    }

    #[test]
    fn a_batch_of_wakeups_wakes_one_idle_cpu_per_thread() {
        let mut s = four_cpus();
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        s.add(a);
        assert_eq!(
            cpus_of(s.idle_cpus_to_wake(0, |_| true)),
            [1],
            "one thread, one CPU"
        );
        s.add(b);
        assert_eq!(cpus_of(s.idle_cpus_to_wake(0, |_| true)), [1, 2]);
        // A thread pinned to an already claimed CPU claims nothing more;
        // the next unpinned one claims the last idle CPU.
        let p = s.allocate_tid();
        s.add(p);
        s.set_affinity(p, Some(1)).unwrap();
        let c = s.allocate_tid();
        s.add(c);
        assert_eq!(cpus_of(s.idle_cpus_to_wake(0, |_| true)), [1, 2, 3]);
        // Never more than the idle CPUs, and never the caller's.
        assert_eq!(cpus_of(s.idle_cpus_to_wake(2, |_| true)), [1, 3]);
        // A busy CPU is not woken.
        assert_eq!(s.yield_current(1), Some(a));
        assert_eq!(cpus_of(s.idle_cpus_to_wake(0, |_| true)), [2, 3]);
    }
}
