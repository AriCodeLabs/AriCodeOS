// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! One wait for IPC, IRQ and deadline (ADR 0044): the end of a service
//! loop's round.
//!
//! A single-threaded service (the network stack, an input driver)
//! alternates bounded rounds of work — its device's events, a budget of IPC
//! requests, its timers — with a wait. That wait ends on whichever comes
//! first: a message on any of its endpoints, an interrupt of its device, or
//! its next deadline; and it does not happen at all while the round's
//! budget left work behind or a deadline is already due. A fixed timeout
//! instead (the former `irq_wait(IRQ, 10)`) made a request that arrived
//! after the round's poll wait for the next interrupt or for the timeout,
//! and woke an idle service a hundred times a second for nothing.
//!
//! The loop shape, with the round's own work elided:
//!
//! ```ignore
//! loop {
//!     let now = clock.now();
//!     let rx_left = /* device events, at most a budget */;
//!     let ipc_left = /* IPC requests polled with timeout 0, at most a budget */;
//!     /* timers */
//!     let mut next = Deadline::NONE;
//!     next.include(/* each pending deadline */);
//!     match wait::end_of_round(set.as_slice(), clock.now(), rx_left || ipc_left, next, &mut msg, &mut handles)? {
//!         Woken::Message { index, len, handles } => /* serve it */,
//!         Woken::Irq { .. } | Woken::Round => {} // the next round looks
//!     }
//! }
//! ```

use crate::{abi, wait_any, Error, Handle, TIMEOUT_INFINITE};
use abi::receive_any::Ready;

/// Members of one wait: every endpoint a service may serve and its
/// interrupt lines.
pub const MAX_MEMBERS: usize = abi::IPC_MAX_ANY + abi::IPC_MAX_ANY_IRQS;

/// The earliest of a service's deadlines, in milliseconds of its clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Deadline(Option<u64>);

impl Deadline {
    /// Nothing is due: only a message or an interrupt ends the wait.
    pub const NONE: Self = Self(None);

    pub const fn at(ms: u64) -> Self {
        Self(Some(ms))
    }

    /// Keeps the earlier of this deadline and `ms`.
    pub fn include(&mut self, ms: u64) {
        self.0 = Some(self.0.map_or(ms, |d| d.min(ms)));
    }

    /// Like `include`, for a deadline that may not exist.
    pub fn include_opt(&mut self, ms: Option<u64>) {
        if let Some(ms) = ms {
            self.include(ms);
        }
    }

    pub const fn get(self) -> Option<u64> {
        self.0
    }
}

/// How long the wait at the end of a round may block, in milliseconds;
/// `None` when it must not block at all, so the next round follows at
/// once: the round's budget left work (`work_left`) or the next deadline
/// is due. With no deadline the wait has no timeout.
pub fn timeout(now: u64, work_left: bool, next: Deadline) -> Option<u64> {
    if work_left {
        return None;
    }
    match next.get() {
        None => Some(TIMEOUT_INFINITE),
        Some(due) if due <= now => None,
        Some(due) => Some(due - now),
    }
}

/// What ended the end-of-round wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woken {
    /// There was no wait (work left, a deadline due) or the deadline came:
    /// the next round does what is due.
    Round,
    /// A message on member `index`, already in the caller's buffers: it was
    /// taken from its queue and must be served. It may have arrived long
    /// after the `now` the wait was computed from: a deadline it sets
    /// counts from the clock read again, or it would expire early.
    Message {
        index: usize,
        len: usize,
        handles: usize,
    },
    /// The interrupt line at `index` fired `count` times; the next round
    /// services the device.
    Irq { index: usize, count: u32 },
}

/// The end of a round: unless `timeout` says not to block, waits on
/// `members` (endpoints and interrupt lines, see [`crate::wait_any`]) until
/// one of them is ready or `next` comes. A timeout is not an error here
/// (`Woken::Round`); any other error is the caller's (`PeerClosed` once
/// every endpoint is gone: nobody can send to the service again).
pub fn end_of_round(
    members: &[Handle],
    now: u64,
    work_left: bool,
    next: Deadline,
    buf: &mut [u8],
    handles_out: &mut [Handle; abi::IPC_MAX_HANDLES],
) -> Result<Woken, Error> {
    let Some(ms) = timeout(now, work_left, next) else {
        return Ok(Woken::Round);
    };
    match wait_any(members, buf, handles_out, ms) {
        Ok(Ready::Message {
            index,
            len,
            handles,
        }) => Ok(Woken::Message {
            index,
            len,
            handles,
        }),
        Ok(Ready::Irq { index, count }) => Ok(Woken::Irq { index, count }),
        Err(Error::Timeout) => Ok(Woken::Round),
        Err(e) => Err(e),
    }
}

/// The handles of one wait, built in place (no allocation): a service
/// lists its endpoints first and its lines after them, so the endpoints
/// alone are a prefix it can poll with `ipc_receive_any`.
#[derive(Debug, Clone, Copy)]
pub struct Members {
    handles: [Handle; MAX_MEMBERS],
    len: usize,
}

impl Default for Members {
    fn default() -> Self {
        Self::new()
    }
}

impl Members {
    pub const fn new() -> Self {
        Self {
            handles: [Handle(0); MAX_MEMBERS],
            len: 0,
        }
    }

    /// Appends `h` and returns its index; `None` when the set is full.
    pub fn push(&mut self, h: Handle) -> Option<usize> {
        let slot = self.handles.get_mut(self.len)?;
        *slot = h;
        self.len += 1;
        Some(self.len - 1)
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn as_slice(&self) -> &[Handle] {
        &self.handles[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_left_never_waits() {
        assert_eq!(timeout(100, true, Deadline::NONE), None);
        assert_eq!(timeout(100, true, Deadline::at(5_000)), None);
    }

    #[test]
    fn a_due_deadline_never_waits() {
        assert_eq!(timeout(100, false, Deadline::at(100)), None);
        assert_eq!(timeout(100, false, Deadline::at(40)), None);
        assert_eq!(timeout(100, false, Deadline::at(0)), None);
    }

    #[test]
    fn waits_until_the_next_deadline() {
        assert_eq!(timeout(100, false, Deadline::at(101)), Some(1));
        assert_eq!(timeout(100, false, Deadline::at(1_100)), Some(1_000));
    }

    /// No deadline: only a message or an interrupt can end the wait, and
    /// `TIMEOUT_INFINITE` is the kernel's "no timeout", never a long one.
    #[test]
    fn without_a_deadline_the_wait_has_no_timeout() {
        assert_eq!(timeout(100, false, Deadline::NONE), Some(TIMEOUT_INFINITE));
        assert_eq!(
            timeout(0, false, Deadline::default()),
            Some(TIMEOUT_INFINITE)
        );
    }

    /// A deadline far beyond `now` must not be mistaken for "no timeout".
    #[test]
    fn a_far_deadline_is_not_infinite() {
        let t = timeout(1, false, Deadline::at(u64::MAX)).unwrap();
        assert_eq!(t, u64::MAX - 1);
        assert_ne!(t, TIMEOUT_INFINITE);
    }

    #[test]
    fn deadline_keeps_the_earliest() {
        let mut d = Deadline::NONE;
        d.include_opt(None);
        assert_eq!(d.get(), None);
        d.include(500);
        d.include(900);
        assert_eq!(d.get(), Some(500));
        d.include_opt(Some(120));
        d.include_opt(None);
        assert_eq!(d.get(), Some(120));
        d.include(120);
        assert_eq!(d, Deadline::at(120));
    }

    #[test]
    fn members_keep_order_and_bound() {
        let mut m = Members::new();
        assert!(m.is_empty());
        for i in 0..MAX_MEMBERS {
            assert_eq!(m.push(Handle(i as u32 + 1)), Some(i));
        }
        assert_eq!(m.push(Handle(99)), None);
        assert_eq!(m.len(), MAX_MEMBERS);
        assert_eq!(m.as_slice()[0], Handle(1));
        assert_eq!(m.as_slice()[MAX_MEMBERS - 1], Handle(MAX_MEMBERS as u32));
        m.clear();
        assert!(m.as_slice().is_empty());
    }
}
