// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Service supervision (stage 1 of the post-roadmap plan):
//! the common service lifecycle contract that `init` applies to every driver
//! and resident service. The policy knows nothing about processes or IPC: it receives
//! facts (`starting`, `ready`, `died`, `tick`) and returns the action that
//! is due (`Restart`, `GiveUp`, `Timeout`). This way it is tested on the host.
//!
//! - States: `Stopped`, `Starting`, `Ready`, `Degraded`, `Failed`.
//! - Generation: every start gets a new number; a client that keeps
//!   the generation knows whether the service it was talking to is a different one.
//! - Restart budget with growing backoff (doubles up to a cap)
//!   that is restored if the service stays stable for a while.
//! - Startup deadline: a service that does not announce `ready` in time counts
//!   as dead.
//! - An optional service that exhausts its budget leaves the system in degraded
//!   mode; a mandatory one leaves it `Failed`.

#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Ready,
    /// Still alive but with a lost function (announced by the service itself or by init).
    Degraded,
    /// No restart budget left: it is not tried again.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(u64),
    Killed,
    /// Did not announce `ready` within the startup deadline.
    StartTimeout,
}

/// What the supervisor must do after a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Launch the service again at the given instant (ms).
    Restart {
        at: u64,
    },
    /// Launch it now (call `starting` after creating the process).
    Start,
    /// Budget exhausted: the service stays `Failed`.
    GiveUp,
    /// Startup exceeded its deadline: treat as a death (`died(StartTimeout)`).
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Restarts allowed before giving up.
    pub max_restarts: u32,
    pub backoff_initial_ms: u64,
    pub backoff_max_ms: u64,
    /// Maximum time between `starting` and `ready`.
    pub start_timeout_ms: u64,
    /// After this long in `Ready` the budget is full again.
    pub stable_ms: u64,
}

impl Policy {
    pub const DRIVER: Policy = Policy {
        max_restarts: 5,
        backoff_initial_ms: 100,
        backoff_max_ms: 5_000,
        start_timeout_ms: 5_000,
        stable_ms: 60_000,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub generation: u32,
    pub restarts: u32,
    pub last_exit: Option<Exit>,
    pub backoff_ms: u64,
    /// Instant (ms) of the last `ready`, if any.
    pub ready_at: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Service {
    policy: Policy,
    optional: bool,
    state: State,
    generation: u32,
    restarts: u32,
    last_exit: Option<Exit>,
    started_at: u64,
    ready_at: Option<u64>,
    backoff_ms: u64,
    restart_at: Option<u64>,
    budget_reset: bool,
    /// Retired on purpose (`stop`): a later death is recorded, not
    /// restarted, until the next explicit `starting`.
    retired: bool,
}

impl Service {
    pub const fn new(policy: Policy, optional: bool) -> Self {
        Self {
            policy,
            optional,
            state: State::Stopped,
            generation: 0,
            restarts: 0,
            last_exit: None,
            started_at: 0,
            ready_at: None,
            backoff_ms: 0,
            restart_at: None,
            budget_reset: false,
            retired: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    pub fn status(&self) -> Status {
        Status {
            state: self.state,
            generation: self.generation,
            restarts: self.restarts,
            last_exit: self.last_exit,
            backoff_ms: self.backoff_ms,
            ready_at: self.ready_at,
        }
    }

    /// The process was just created: new generation, startup deadline running.
    pub fn starting(&mut self, now: u64) -> u32 {
        self.generation += 1;
        self.state = State::Starting;
        self.started_at = now;
        self.ready_at = None;
        self.restart_at = None;
        self.budget_reset = false;
        self.retired = false;
        self.generation
    }

    /// The service answers (probe or its own announcement).
    pub fn ready(&mut self, now: u64) {
        if matches!(self.state, State::Starting | State::Degraded) {
            self.state = State::Ready;
            self.ready_at = Some(now);
        }
    }

    /// Retire the service on purpose: it stays `Stopped` with no pending
    /// restart, even if its process dies afterwards (audit 2026-10-25 C, L5).
    pub fn stop(&mut self) {
        self.state = State::Stopped;
        self.restart_at = None;
        self.ready_at = None;
        self.retired = true;
    }

    /// The process outlived a kill: it may still hold its devices and
    /// endpoints, so another generation must not compete with it. The
    /// service is `Failed` whatever budget it had left.
    pub fn abandon(&mut self, exit: Exit) -> Action {
        self.last_exit = Some(exit);
        self.ready_at = None;
        self.state = State::Failed;
        self.restart_at = None;
        Action::GiveUp
    }

    /// Alive, but with a lost function.
    pub fn degraded(&mut self) {
        if matches!(self.state, State::Starting | State::Ready) {
            self.state = State::Degraded;
        }
    }

    /// The process exited (or its startup expired). Decides whether to restart.
    pub fn died(&mut self, now: u64, exit: Exit) -> Action {
        self.last_exit = Some(exit);
        self.ready_at = None;
        if self.retired {
            self.state = State::Stopped;
            return Action::Nothing;
        }
        self.restarts += 1;
        if self.restarts > self.policy.max_restarts {
            self.state = State::Failed;
            self.restart_at = None;
            return Action::GiveUp;
        }
        let shift = (self.restarts - 1).min(20);
        self.backoff_ms = self
            .policy
            .backoff_initial_ms
            .saturating_mul(1u64 << shift)
            .min(self.policy.backoff_max_ms);
        self.state = State::Stopped;
        let at = now + self.backoff_ms;
        self.restart_at = Some(at);
        Action::Restart { at }
    }

    /// Time step: a pending restart that is now due, an expired startup
    /// deadline or stability reached (budget restored).
    pub fn tick(&mut self, now: u64) -> Action {
        match self.state {
            State::Stopped => match self.restart_at {
                Some(at) if now >= at => {
                    self.restart_at = None;
                    Action::Start
                }
                _ => Action::Nothing,
            },
            State::Starting
                if now.saturating_sub(self.started_at) >= self.policy.start_timeout_ms =>
            {
                Action::Timeout
            }
            State::Ready => {
                if !self.budget_reset
                    && self
                        .ready_at
                        .is_some_and(|r| now.saturating_sub(r) >= self.policy.stable_ms)
                {
                    self.restarts = 0;
                    self.backoff_ms = 0;
                    self.budget_reset = true;
                }
                Action::Nothing
            }
            _ => Action::Nothing,
        }
    }

    /// How a `Failed` service affects the system: optional -> degraded,
    /// mandatory -> failure.
    pub fn system_state_if_failed(&self) -> State {
        if self.optional {
            State::Degraded
        } else {
            State::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            max_restarts: 200,
            backoff_initial_ms: 10,
            backoff_max_ms: 160,
            start_timeout_ms: 1000,
            stable_ms: 5000,
        }
    }

    #[test]
    fn hundred_restarts_with_capped_backoff() {
        let mut s = Service::new(policy(), false);
        let mut now = 0;
        let mut generations = alloc_free_vec();
        let g = s.starting(now);
        generations.push(g);
        s.ready(now);
        for i in 1..=100u32 {
            now += 50;
            match s.died(now, Exit::Killed) {
                Action::Restart { at } => {
                    let expected = (10u64 << (i - 1).min(20)).min(160);
                    assert_eq!(at - now, expected, "backoff of restart {i}");
                    assert_eq!(s.tick(at - 1), Action::Nothing);
                    assert_eq!(s.tick(at), Action::Start);
                    now = at;
                    generations.push(s.starting(now));
                    s.ready(now);
                }
                other => panic!("restart {i}: {other:?}"),
            }
        }
        assert_eq!(s.status().restarts, 100);
        assert_eq!(s.generation(), 101);
        assert!(generations.windows(2).all(|w| w[1] == w[0] + 1));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn budget_exhaustion_fails_and_never_restarts() {
        let mut s = Service::new(
            Policy {
                max_restarts: 2,
                ..policy()
            },
            true,
        );
        s.starting(0);
        assert!(matches!(s.died(0, Exit::Code(3)), Action::Restart { .. }));
        s.starting(10);
        assert!(matches!(s.died(10, Exit::Code(3)), Action::Restart { .. }));
        s.starting(40);
        assert_eq!(s.died(40, Exit::Killed), Action::GiveUp);
        assert_eq!(s.state(), State::Failed);
        assert_eq!(s.tick(100_000), Action::Nothing);
        assert_eq!(s.system_state_if_failed(), State::Degraded, "optional");
        assert_eq!(s.status().last_exit, Some(Exit::Killed));
        let mandatory = Service::new(policy(), false);
        assert_eq!(mandatory.system_state_if_failed(), State::Failed);
    }

    #[test]
    fn start_timeout_counts_as_death() {
        let mut s = Service::new(policy(), false);
        s.starting(0);
        assert_eq!(s.tick(999), Action::Nothing);
        assert_eq!(s.tick(1000), Action::Timeout);
        assert!(matches!(
            s.died(1000, Exit::StartTimeout),
            Action::Restart { at: 1010 }
        ));
        assert_eq!(s.status().last_exit, Some(Exit::StartTimeout));
    }

    #[test]
    fn stable_service_recovers_its_budget() {
        let mut s = Service::new(policy(), false);
        s.starting(0);
        s.died(0, Exit::Killed);
        s.starting(10);
        s.died(10, Exit::Killed);
        s.starting(30);
        s.ready(30);
        assert_eq!(s.status().restarts, 2);
        assert_eq!(s.tick(30 + 4999), Action::Nothing);
        assert_eq!(s.status().restarts, 2);
        assert_eq!(s.tick(30 + 5000), Action::Nothing);
        assert_eq!(s.status().restarts, 0, "budget restored");
        assert_eq!(s.status().backoff_ms, 0);
        // The next death goes back to the initial backoff.
        assert!(matches!(
            s.died(6000, Exit::Killed),
            Action::Restart { at: 6010 }
        ));
    }

    #[test]
    fn stopped_on_purpose_is_not_restarted() {
        let mut s = Service::new(policy(), true);
        s.starting(0);
        assert!(matches!(
            s.died(0, Exit::Killed),
            Action::Restart { at: 10 }
        ));
        s.stop();
        assert_eq!(s.state(), State::Stopped);
        assert_eq!(s.tick(10), Action::Nothing);
        assert_eq!(s.tick(10_000), Action::Nothing);
        // A new explicit start is still possible.
        assert_eq!(s.starting(20), 2);
    }

    #[test]
    fn degraded_and_ready_transitions() {
        let mut s = Service::new(policy(), true);
        assert_eq!(s.state(), State::Stopped);
        s.ready(0);
        assert_eq!(
            s.state(),
            State::Stopped,
            "ready without starting changes nothing"
        );
        s.starting(0);
        s.degraded();
        assert_eq!(s.state(), State::Degraded);
        s.ready(5);
        assert_eq!(s.state(), State::Ready);
        assert_eq!(s.status().ready_at, Some(5));
    }

    #[test]
    fn a_retired_or_abandoned_service_is_not_restarted() {
        let mut s = Service::new(policy(), false);
        s.starting(0);
        s.ready(5);
        s.stop();
        assert_eq!(s.died(10, Exit::Killed), Action::Nothing);
        assert_eq!(s.state(), State::Stopped);
        assert_eq!(s.status().last_exit, Some(Exit::Killed));
        assert_eq!(s.tick(10_000), Action::Nothing);
        // An explicit start lifts the retirement.
        s.starting(20);
        assert!(matches!(s.died(30, Exit::Code(1)), Action::Restart { .. }));

        let mut t = Service::new(policy(), false);
        t.starting(0);
        assert_eq!(t.tick(1000), Action::Timeout);
        assert_eq!(t.abandon(Exit::StartTimeout), Action::GiveUp);
        assert_eq!(t.state(), State::Failed);
        assert_eq!(t.tick(100_000), Action::Nothing);
    }

    extern crate std;
    fn alloc_free_vec() -> std::vec::Vec<u32> {
        std::vec::Vec::new()
    }
}
