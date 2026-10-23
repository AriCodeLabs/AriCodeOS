// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Idle time of one CPU (incident 0020), kept by the scheduler at its
//! context switches.
//!
//! Definition: a CPU is idle while its idle thread is its running thread.
//! The interval opens at the switch to the idle thread and closes at the
//! switch away from it, wherever that switch happens (in the idle loop or
//! inside an interrupt handler taken from `hlt`). Interrupt handlers that
//! run on top of the idle thread and return to it without switching count
//! as idle: they do not end the interval. Another thread's time is never
//! idle, because that thread is the running one.
//!
//! Sampling: [`IdleClock::total`] includes the open interval up to the
//! sampling instant, so a sample never lags behind a long idle stretch and
//! never receives an interval that started before the previous sample all
//! at once. Sampling every CPU at one instant `now` gives counters that
//! are each at most `now - boot` when every interval starts after `boot`.

/// Idle time of one CPU, in the caller's time unit (TSC cycles in the
/// kernel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IdleClock {
    /// Sum of the closed idle intervals.
    closed: u64,
    /// Start of the open interval while the idle thread runs.
    since: Option<u64>,
}

impl IdleClock {
    pub const fn new() -> Self {
        Self {
            closed: 0,
            since: None,
        }
    }

    /// The CPU's running thread changes at `now`: `from_idle` if the thread
    /// leaving is the idle thread, `to_idle` if the one entering is.
    pub fn switch(&mut self, from_idle: bool, to_idle: bool, now: u64) {
        if from_idle {
            if let Some(start) = self.since.take() {
                self.closed = self.closed.saturating_add(now.saturating_sub(start));
            }
        }
        if to_idle {
            self.since = Some(now);
        }
    }

    /// Idle time up to `now`, the open interval included.
    pub fn total(&self, now: u64) -> u64 {
        let open = self.since.map_or(0, |start| now.saturating_sub(start));
        self.closed.saturating_add(open)
    }

    /// Start of the open interval, if the idle thread is running.
    pub fn idle_since(&self) -> Option<u64> {
        self.since
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_interval_between_idle_stretches_is_excluded() {
        // The audit's control on one CPU: idle, a thread runs 500, idle.
        let mut c = IdleClock::new();
        c.switch(false, true, 100); // to idle
        c.switch(true, false, 150); // a thread is switched in (from a handler)
        assert_eq!(c.total(150), 50);
        assert_eq!(c.total(650), 50, "the running thread's time is not idle");
        c.switch(false, true, 650); // it sleeps
        assert_eq!(c.total(850), 250);
        c.switch(true, false, 850);
        assert_eq!(c.total(10_000), 250);
    }

    #[test]
    fn sample_includes_the_open_interval() {
        let mut c = IdleClock::new();
        c.switch(false, true, 1_000);
        assert_eq!(c.idle_since(), Some(1_000));
        // Two samples while idle see the interval grow between them, not
        // all at once when it closes.
        assert_eq!(c.total(1_400), 400);
        assert_eq!(c.total(1_900), 900);
        c.switch(true, false, 2_000);
        assert_eq!(c.idle_since(), None);
        assert_eq!(c.total(5_000), 1_000);
    }

    #[test]
    fn interrupts_without_a_switch_stay_idle() {
        // Handlers run on the idle thread and return to it: no switch is
        // reported, so the whole stretch is idle, to the unit.
        let mut c = IdleClock::new();
        c.switch(false, true, 10);
        let before = c.total(20);
        let after = c.total(1_020);
        assert_eq!(after - before, 1_000);
    }

    #[test]
    fn switches_between_other_threads_do_not_touch_idle() {
        let mut c = IdleClock::new();
        c.switch(false, false, 10);
        c.switch(false, false, 20);
        assert_eq!(c.total(1_000), 0);
        assert_eq!(c, IdleClock::new());
    }

    #[test]
    fn an_open_restarts_the_stretch() {
        // An open without a close (the thread leaving was not the idle
        // thread) starts the stretch at the new switch: the time since the
        // stale start was not idle.
        let mut c = IdleClock::new();
        c.switch(false, true, 100);
        c.switch(false, true, 300);
        assert_eq!(c.idle_since(), Some(300));
        assert_eq!(c.total(400), 100);
    }

    #[test]
    fn idle_to_idle_keeps_the_count() {
        let mut c = IdleClock::new();
        c.switch(false, true, 0);
        c.switch(true, true, 300);
        assert_eq!(c.total(500), 500);
    }

    #[test]
    fn never_exceeds_elapsed_time_and_never_underflows() {
        let mut c = IdleClock::new();
        // Intervals that start after `boot = 0`: idle <= now at any sample.
        let mut t = 0;
        for step in 1..50u64 {
            t += step * 7;
            c.switch(step % 2 == 0, step % 2 == 1, t);
            assert!(c.total(t) <= t);
            assert!(c.total(t + 3) <= t + 3);
        }
        // A sample taken with an earlier timestamp than the open interval's
        // start (another CPU's TSC read a moment earlier) counts nothing open.
        let mut c = IdleClock::new();
        c.switch(false, true, 1_000);
        assert_eq!(c.total(999), 0);
        // A spurious close without an open interval adds nothing.
        c.switch(true, false, 2_000);
        c.switch(true, false, 3_000);
        assert_eq!(c.total(4_000), 1_000);
    }
}
