// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Root port hot-plug (study `docs/audits/2026-11-11-xhci-hot-plug-study.md`):
//! what a Port Status Change means for the driver (xHCI 4.19.2, USB 2.0
//! 11.24.2.7.2), the connect debounce (USB 2.0 7.1.7.3), which reset brings
//! a port's link back (xHCI 4.19.1.2), whether an enumeration still
//! addresses the device on its port, and the pool where the memory of
//! released devices waits to be reused. Pure logic: the driver reads PORTSC
//! and the clock and passes them in, with its timings and counts as a
//! [`Policy`].

use crate::op;

/// Every timing and count of the hot-plug path, in one place: the driver
/// passes it in rather than fixing them (the owner's no-hardcoding rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// A connection is enumerated after this long without a change: USB
    /// 2.0 7.1.7.3 (TATTDB) asks at least 100 ms; Linux and Haiku use 100.
    pub debounce_ms: u64,
    /// Pause between the end of a port reset and the first request to the
    /// device: USB 2.0 7.1.7.5 (TRSTRCY) gives the device at least 10 ms;
    /// Linux waits 10 + 40 ("plus some extra", `hub_port_reset`) for the
    /// devices that need more, and so does this default.
    pub reset_recovery_ms: u64,
    /// Enumeration attempts on one connection (Linux 4, Haiku 2).
    pub attempts: u8,
    /// Pause before the next attempt: this times the attempts that failed.
    pub retry_ms: u64,
}

impl Policy {
    pub const DEFAULT: Self = Self {
        debounce_ms: 100,
        reset_recovery_ms: 50,
        attempts: 3,
        retry_ms: 200,
    };

    /// After `failed` attempts on one connection, at `now`: when to try
    /// again, or `None` once the attempts are spent.
    pub fn retry_at(&self, now: u64, failed: u8) -> Option<u64> {
        (failed < self.attempts).then(|| now + self.retry_ms * failed as u64)
    }
}

/// Whether the device an enumeration addressed is still the one on its
/// port when the enumeration ends: the port has a connection, and no
/// connect change happened since the enumeration began, neither one seen in
/// `portsc` now (its event not handled yet) nor one handled while it ran
/// (`changed`). Otherwise the slot addresses a device that left or was
/// replaced (xHCI 4.19.2) and the enumeration must fail, releasing it.
pub fn still_attached(portsc: u32, changed: bool) -> bool {
    !changed && portsc & (op::PORTSC_CCS | op::PORTSC_CAS) != 0 && portsc & op::PORTSC_CSC == 0
}

/// What a port change asks of the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortAction {
    /// The device the driver had on the port is gone (xHCI 4.19.2: after a
    /// connect status change the slot addresses nothing): its endpoints are
    /// stopped, its slot disabled and its memory released.
    pub detach: bool,
    /// A device is connected: it is enumerated once its connection has
    /// been stable for the debounce interval.
    pub debounce: bool,
}

/// What the PORTSC read when a Port Status Change Event was handled
/// (before its change bits were cleared) means, `tracked` saying whether
/// the driver has a device on that port.
///
/// - A connect status change (CSC) on a port with a device: that device
///   is gone, whatever CCS says now (a quick unplug and replug sets CSC
///   once and leaves CCS set: the device there is a new one, USB 2.0
///   11.24.2.7.2.1). Linux tries to keep the old device when its
///   descriptors read the same; the driver does not, because a HID
///   enumerated again loses nothing (its keys were released).
/// - A port enable change (PEC) that left the port disabled under a
///   device: an error (babble, a port fault) took the port away (xHCI
///   4.19.1.1, USB 2.0 11.24.2.7.2.2); the device is enumerated again, as
///   Linux does ("disabled by hub (EMI?), re-enabling").
/// - A connection present after such a change, on a USB3 port Cold
///   Attach Status included, is debounced and then enumerated.
/// - Anything else (a reset change of the driver's own port reset, a link
///   state change) asks nothing.
pub fn port_action(portsc: u32, tracked: bool) -> PortAction {
    let connected = portsc & (op::PORTSC_CCS | op::PORTSC_CAS) != 0;
    let connect_change = portsc & op::PORTSC_CSC != 0;
    let disabled_under = tracked && portsc & op::PORTSC_PEC != 0 && portsc & op::PORTSC_PED == 0;
    PortAction {
        detach: tracked && (connect_change || disabled_under),
        debounce: connected && (connect_change || disabled_under),
    }
}

/// The reset that takes a port with a device to Enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortReset {
    /// PR: a USB2 port's bus reset, or a USB3 port's Hot Reset.
    Hot,
    /// WPR: a USB3 port whose link is in SS.Inactive or Compliance Mode,
    /// or that reports Cold Attach Status, leaves that state only with a
    /// Warm Reset (xHCI 4.19.1.2, 5.4.8; Linux
    /// `hub_port_warm_reset_required`, FreeBSD `UPS_PORT_LS_SS_INA`).
    Warm,
}

/// Which reset the port of `portsc` needs. Those link states and CAS
/// exist only on USB3 ports, so a USB2 port (where WPR is reserved) always
/// takes PR. The link state is not read while a reset is in progress:
/// it is not reliable then (Linux `handle_port_status`).
pub fn port_reset(portsc: u32) -> PortReset {
    if portsc & op::PORTSC_RESET != 0 {
        return PortReset::Hot;
    }
    let pls = op::port_link_state(portsc);
    if portsc & op::PORTSC_CAS != 0 || pls == op::PLS_INACTIVE || pls == op::PLS_COMPLIANCE {
        PortReset::Warm
    } else {
        PortReset::Hot
    }
}

/// A port waiting to be enumerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPort {
    pub port: u8,
    /// Not before this time (ms).
    pub due: u64,
    /// Enumeration attempts already failed on this connection.
    pub attempts: u8,
}

/// The queue is full: the port was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Ports waiting for their connect debounce or for another enumeration
/// attempt, at most `N`. The debounce is event driven: every connect
/// change on a port starts its interval again (USB 2.0 7.1.7.3: the
/// interval restarts whenever a disconnect is detected; Linux and Haiku
/// poll for 100 ms of stable status instead, which comes to the same
/// because every change raises an event here), so a port is due only after
/// `debounce_ms` without a change.
#[derive(Debug, Clone, Copy)]
pub struct PendingPorts<const N: usize> {
    entries: [Option<PendingPort>; N],
}

impl<const N: usize> Default for PendingPorts<N> {
    fn default() -> Self {
        Self { entries: [None; N] }
    }
}

impl<const N: usize> PendingPorts<N> {
    /// A connection change on `port` at `now`: its debounce starts again,
    /// due at `now + debounce_ms`, with no attempts counted (a new
    /// connection is a new device).
    pub fn changed(&mut self, port: u8, now: u64, debounce_ms: u64) -> Result<(), Full> {
        self.put(PendingPort {
            port,
            due: now + debounce_ms,
            attempts: 0,
        })
    }

    /// `p` failed an attempt: queued again, due at `at`, with that attempt
    /// counted, unless its port changed meanwhile (`requeue`).
    pub fn retry(&mut self, p: PendingPort, at: u64) -> Result<(), Full> {
        self.requeue(PendingPort {
            due: at,
            attempts: p.attempts.saturating_add(1),
            ..p
        })
    }

    /// `p` could not be handled yet (its port waits for something else):
    /// queued again, due at `at`, with its attempts as they were, unless
    /// its port changed meanwhile (`requeue`).
    pub fn defer(&mut self, p: PendingPort, at: u64) -> Result<(), Full> {
        self.requeue(PendingPort { due: at, ..p })
    }

    /// A port taken out and put back. If a connect change queued it again
    /// in between, that entry stays: a new connection starts with its own
    /// debounce and no attempts counted.
    fn requeue(&mut self, p: PendingPort) -> Result<(), Full> {
        if self.entries.iter().flatten().any(|e| e.port == p.port) {
            return Ok(());
        }
        self.put(p)
    }

    fn put(&mut self, p: PendingPort) -> Result<(), Full> {
        if let Some(e) = self.entries.iter_mut().flatten().find(|e| e.port == p.port) {
            *e = p;
            return Ok(());
        }
        let free = self.entries.iter_mut().find(|e| e.is_none()).ok_or(Full)?;
        *free = Some(p);
        Ok(())
    }

    /// The earliest port due at `now`, taken out of the queue.
    pub fn take_due(&mut self, now: u64) -> Option<PendingPort> {
        let slot = self
            .entries
            .iter_mut()
            .filter(|e| e.is_some_and(|p| p.due <= now))
            .min_by_key(|e| e.map_or(u64::MAX, |p| p.due))?;
        slot.take()
    }

    /// When the next port is due, if any.
    pub fn next_due(&self) -> Option<u64> {
        self.entries.iter().flatten().map(|p| p.due).min()
    }

    /// `port` no longer waits (its connection is being handled another
    /// way, e.g. by the boot enumeration).
    pub fn forget(&mut self, port: u8) {
        for e in self.entries.iter_mut() {
            if e.is_some_and(|p| p.port == port) {
                *e = None;
            }
        }
    }
}

/// Memory handed back by released devices, reused before anything new is
/// allocated from a linear arena that never shrinks: at most `N` entries.
/// The caller puts an entry here only once the controller owns none of it
/// (after the Disable Slot of its slot completed releasing it, xHCI 4.6.4).
#[derive(Debug, Clone, Copy)]
pub struct Spares<T: Copy, const N: usize> {
    entries: [Option<T>; N],
}

impl<T: Copy, const N: usize> Default for Spares<T, N> {
    fn default() -> Self {
        Self { entries: [None; N] }
    }
}

impl<T: Copy, const N: usize> Spares<T, N> {
    /// Keeps `item` for reuse; gives it back when the pool is full.
    pub fn put(&mut self, item: T) -> Result<(), T> {
        match self.entries.iter_mut().find(|e| e.is_none()) {
            Some(e) => {
                *e = Some(item);
                Ok(())
            }
            None => Err(item),
        }
    }

    /// An entry to reuse, if any.
    pub fn take(&mut self) -> Option<T> {
        self.entries.iter_mut().find_map(|e| e.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use op::{
        PORTSC_CAS, PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_POWER, PORTSC_PRC,
        PORTSC_RESET,
    };
    use std::vec::Vec;

    const fn pls(state: u8) -> u32 {
        (state as u32) << 5
    }

    fn action(detach: bool, debounce: bool) -> PortAction {
        PortAction { detach, debounce }
    }

    #[test]
    fn a_new_connection_on_a_free_port_is_debounced() {
        let sc = PORTSC_POWER | PORTSC_CCS | PORTSC_CSC;
        assert_eq!(port_action(sc, false), action(false, true));
    }

    #[test]
    fn an_unplug_detaches_and_a_replug_detaches_then_debounces() {
        // Unplugged: CSC with CCS clear.
        assert_eq!(
            port_action(PORTSC_POWER | PORTSC_CSC, true),
            action(true, false)
        );
        // Unplugged and plugged again before the event was handled: CSC
        // with CCS set; the old device is gone all the same.
        assert_eq!(
            port_action(PORTSC_POWER | PORTSC_CSC | PORTSC_CCS | PORTSC_PED, true),
            action(true, true)
        );
        // A disconnect on a port nobody used asks nothing.
        assert_eq!(
            port_action(PORTSC_POWER | PORTSC_CSC, false),
            action(false, false)
        );
    }

    #[test]
    fn a_port_disabled_under_a_device_enumerates_it_again() {
        let sc = PORTSC_POWER | PORTSC_CCS | PORTSC_PEC;
        assert_eq!(port_action(sc, true), action(true, true));
        // Without a device of ours there, an enable change means nothing.
        assert_eq!(port_action(sc, false), action(false, false));
        // An enable change that left the port enabled is no loss.
        assert_eq!(port_action(sc | PORTSC_PED, true), action(false, false));
    }

    #[test]
    fn the_drivers_own_port_reset_asks_nothing() {
        // The reset of an enumeration: PRC, the port enabled, no CSC.
        let sc = PORTSC_POWER | PORTSC_CCS | PORTSC_PED | PORTSC_PRC;
        assert_eq!(port_action(sc, true), action(false, false));
        assert_eq!(port_action(sc, false), action(false, false));
        // A stale event whose change bits were already cleared.
        assert_eq!(
            port_action(PORTSC_POWER | PORTSC_CCS | PORTSC_PED, false),
            action(false, false)
        );
    }

    #[test]
    fn cold_attach_counts_as_a_connection() {
        let sc = PORTSC_POWER | PORTSC_CAS | PORTSC_CSC | pls(op::PLS_INACTIVE);
        assert_eq!(port_action(sc, false), action(false, true));
    }

    #[test]
    fn usb3_links_stuck_in_inactive_or_compliance_take_a_warm_reset() {
        let base = PORTSC_POWER | PORTSC_CCS;
        assert_eq!(port_reset(base | pls(op::PLS_INACTIVE)), PortReset::Warm);
        assert_eq!(port_reset(base | pls(op::PLS_COMPLIANCE)), PortReset::Warm);
        assert_eq!(port_reset(PORTSC_POWER | PORTSC_CAS), PortReset::Warm);
        // Polling (7), RxDetect (5), U0: the plain reset.
        for state in [0, 5, 7] {
            assert_eq!(port_reset(base | pls(state)), PortReset::Hot, "{state}");
        }
        // The link state is not read while a reset is in progress.
        assert_eq!(
            port_reset(base | PORTSC_RESET | pls(op::PLS_INACTIVE)),
            PortReset::Hot
        );
    }

    /// The ports queued, in no particular order.
    fn ports<const N: usize>(q: &PendingPorts<N>) -> Vec<u8> {
        let mut v: Vec<u8> = q.entries.iter().flatten().map(|p| p.port).collect();
        v.sort();
        v
    }

    #[test]
    fn a_change_restarts_the_debounce_and_forgets_attempts() {
        let mut q = PendingPorts::<2>::default();
        q.changed(3, 1000, 100).unwrap();
        assert_eq!(q.next_due(), Some(1100));
        assert!(q.take_due(1099).is_none(), "not stable for 100 ms yet");
        // Another change at 1050 (a bounce): due 100 ms after it.
        q.changed(3, 1050, 100).unwrap();
        assert_eq!(q.next_due(), Some(1150));
        assert!(q.take_due(1100).is_none());
        let p = q.take_due(1150).unwrap();
        assert_eq!((p.port, p.attempts), (3, 0));
        assert!(ports(&q).is_empty());
        // A failed attempt comes back counted; deferring keeps the count.
        q.retry(p, 1400).unwrap();
        let p = q.take_due(1400).unwrap();
        assert_eq!(p.attempts, 1);
        q.defer(p, 1500).unwrap();
        let p = q.take_due(1500).unwrap();
        assert_eq!((p.due, p.attempts), (1500, 1));
        // A change while the port is queued for a retry: a new connection.
        q.retry(p, 1600).unwrap();
        q.changed(3, 1550, 100).unwrap();
        let p = q.take_due(1650).unwrap();
        assert_eq!((p.due, p.attempts), (1650, 0));
    }

    #[test]
    fn a_new_connection_queued_during_an_attempt_is_not_overwritten() {
        let mut q = PendingPorts::<2>::default();
        q.changed(4, 0, 100).unwrap();
        let p = q.take_due(100).unwrap();
        // While the attempt ran, the device was replaced: queued afresh.
        q.changed(4, 150, 100).unwrap();
        // The failed attempt's retry and a deferral leave that entry alone.
        q.retry(PendingPort { attempts: 2, ..p }, 900).unwrap();
        q.defer(p, 900).unwrap();
        let p = q.take_due(250).unwrap();
        assert_eq!((p.port, p.due, p.attempts), (4, 250, 0));
        assert_eq!(q.next_due(), None);
    }

    #[test]
    fn the_earliest_port_goes_first_and_a_full_queue_says_so() {
        let mut q = PendingPorts::<2>::default();
        q.changed(5, 2000, 100).unwrap();
        q.changed(1, 1000, 100).unwrap();
        assert_eq!(q.changed(2, 1000, 100), Err(Full));
        // A port already queued still fits.
        q.changed(5, 1500, 100).unwrap();
        assert_eq!(ports(&q), [1, 5]);
        assert_eq!(q.take_due(3000).map(|p| p.port), Some(1));
        assert_eq!(q.take_due(3000).map(|p| p.port), Some(5));
        assert_eq!(q.take_due(3000), None);
        assert_eq!(q.next_due(), None);
        // A forgotten port frees its entry.
        q.changed(7, 0, 100).unwrap();
        q.changed(8, 0, 100).unwrap();
        q.forget(7);
        assert_eq!(ports(&q), [8]);
        q.changed(9, 0, 100).unwrap();
        assert_eq!(ports(&q), [8, 9]);
    }

    #[test]
    fn the_policy_spaces_retries_and_stops_after_its_attempts() {
        let p = Policy::DEFAULT;
        assert!(p.debounce_ms >= 100, "USB 2.0 7.1.7.3");
        assert!(p.reset_recovery_ms >= 10, "USB 2.0 7.1.7.5");
        assert_eq!(p.retry_at(1000, 1), Some(1200));
        assert_eq!(p.retry_at(1000, 2), Some(1400));
        assert_eq!(p.retry_at(1000, 3), None);
        let once = Policy { attempts: 1, ..p };
        assert_eq!(once.retry_at(1000, 1), None);
    }

    #[test]
    fn an_enumeration_holds_only_while_the_same_device_is_there() {
        let here = PORTSC_POWER | PORTSC_CCS | PORTSC_PED;
        assert!(still_attached(here, false));
        // Unplugged while it ran, the change already handled (CSC cleared).
        assert!(!still_attached(PORTSC_POWER, true));
        assert!(!still_attached(here, true), "replugged: a new device");
        // Unplugged, its event not handled yet.
        assert!(!still_attached(PORTSC_POWER | PORTSC_CSC, false));
        // Replugged, its event not handled yet.
        assert!(!still_attached(here | PORTSC_CSC, false));
    }

    #[test]
    fn spares_are_reused_and_a_full_pool_hands_the_item_back() {
        let mut s = Spares::<u32, 2>::default();
        assert_eq!(s.take(), None);
        s.put(1).unwrap();
        s.put(2).unwrap();
        assert_eq!(s.put(3), Err(3));
        let a = s.take().unwrap();
        let b = s.take().unwrap();
        assert_eq!(a + b, 3);
        assert_eq!(s.take(), None);
    }
}
