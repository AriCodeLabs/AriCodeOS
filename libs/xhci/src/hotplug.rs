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

use crate::{op, speed};

/// The window a port reset gets (every try of it), before the device's
/// recovery time, which comes on top: a USB3 warm reset takes 80 to 120 ms
/// plus link training, and Linux allows a reset 800 ms in all.
pub const RESET_WINDOW_MS: u64 = 500;
/// The longest reset recovery `Policy::parse` accepts (on top of the
/// reset window, so it never eats into the reset).
pub const MAX_RESET_RECOVERY_MS: u64 = 200;

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
    /// Port resets of one attempt when a USB2 connection changes during the
    /// reset (Linux `PORT_RESET_TRIES` 5, `hub_port_wait_reset` -EAGAIN).
    pub reset_tries: u8,
    /// How many enumerations in a row may end with the device changing or
    /// bouncing at every reset before its port is left alone until it is
    /// empty (`PortBounces`); at least 2, so one replug during an
    /// enumeration never loses the port.
    pub bounce_limit: u8,
}

impl Policy {
    pub const DEFAULT: Self = Self {
        debounce_ms: 100,
        reset_recovery_ms: 50,
        attempts: 3,
        retry_ms: 200,
        reset_tries: 5,
        bounce_limit: 3,
    };

    /// The policy `text` asks for, from the defaults: a comma-separated
    /// list of `debounce:MS`, `recovery:MS`, `attempts:N`, `retry:MS`,
    /// `resets:N`, `bounces:N` (the value of the boot key `usb.hotplug`).
    /// Each value is bounded where the specifications or a working driver
    /// put it: the debounce at least 100 ms (USB 2.0 7.1.7.3) and at most
    /// 2000 (Linux gives up then), the reset recovery 10 ms (TRSTRCY) to
    /// `MAX_RESET_RECOVERY_MS`, 1 to 10 attempts and resets, 2 to 10
    /// bounces, a retry pause up to 5000 ms. Anything else is refused with
    /// the reason.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let mut p = Self::DEFAULT;
        for item in text.split(',').filter(|i| !i.is_empty()) {
            let (name, value) = item.split_once(':').ok_or("an item is not NAME:VALUE")?;
            let value: u64 = value.parse().map_err(|_| "a value is not a number")?;
            let within = |lo: u64, hi: u64, why: &'static str| {
                (lo..=hi).contains(&value).then_some(value).ok_or(why)
            };
            match name {
                "debounce" => p.debounce_ms = within(100, 2000, "debounce: 100 to 2000 ms")?,
                "recovery" => {
                    p.reset_recovery_ms =
                        within(10, MAX_RESET_RECOVERY_MS, "recovery: 10 to 200 ms")?
                }
                "attempts" => p.attempts = within(1, 10, "attempts: 1 to 10")? as u8,
                "retry" => p.retry_ms = within(0, 5000, "retry: 0 to 5000 ms")?,
                "resets" => p.reset_tries = within(1, 10, "resets: 1 to 10")? as u8,
                "bounces" => p.bounce_limit = within(2, 10, "bounces: 2 to 10")? as u8,
                _ => {
                    return Err(
                        "unknown item (debounce, recovery, attempts, retry, resets, bounces)",
                    )
                }
            }
        }
        Ok(p)
    }

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

/// What a port's status says after a reset the driver issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterReset {
    /// The reset is in progress (PR set), the port is not enabled yet, or
    /// the connection is not back yet (some chips take a moment after the
    /// reset, Linux `hub_port_wait_reset`): wait, up to the reset's time.
    Waiting,
    /// Reset done, the port enabled and still connected: go on.
    Ready,
    /// Reset again: a USB2 connection changed during the reset while a
    /// device is still connected (Linux retries it, -EAGAIN), or the link
    /// still needs a Warm Reset (`port_reset` picks it next).
    Again,
}

/// `portsc` read after a reset the driver issued on a port that is a
/// USB3 one (`usb3`, `PortProtocols`) or a USB2 one. A connect change on a
/// USB3 port is not a reason to reset again: a USB3 connection may bounce
/// across warm resets while the device connects fine (Linux ignores it
/// likewise); the caller clears it with the other change bits.
pub fn after_reset(portsc: u32, usb3: bool) -> AfterReset {
    if portsc & op::PORTSC_RESET != 0 || portsc & (op::PORTSC_CCS | op::PORTSC_CAS) == 0 {
        return AfterReset::Waiting;
    }
    if port_reset(portsc) == PortReset::Warm {
        return AfterReset::Again;
    }
    if portsc & op::PORTSC_CSC != 0 && !usb3 {
        return AfterReset::Again;
    }
    if portsc & op::PORTSC_PED == 0 {
        return AfterReset::Waiting;
    }
    AfterReset::Ready
}

/// The USB major revision of each root port, from the xHCI Supported
/// Protocol capabilities (xHCI 7.2): which ports are USB2 and which USB3
/// is a property of the port, not of the speed a device negotiated (Linux
/// keeps a root hub per protocol for the same reason).
#[derive(Debug, Clone, Copy)]
pub struct PortProtocols {
    major: [u8; 256],
}

impl Default for PortProtocols {
    fn default() -> Self {
        Self { major: [0; 256] }
    }
}

/// Extended capability ID of a Supported Protocol capability (xHCI 7.2).
pub const CAP_SUPPORTED_PROTOCOL: u8 = 2;

impl PortProtocols {
    /// A Supported Protocol capability: its first dword (`dword0`: ID,
    /// next pointer, minor and major revision) and its third (`dword2`:
    /// Compatible Port Offset and Count). Ports past `max_ports` or a
    /// capability of another ID are ignored (device-reported values are
    /// bounded, ADR 0051). Returns whether it was taken.
    pub fn add(&mut self, dword0: u32, dword2: u32, max_ports: u8) -> bool {
        if dword0 & 0xFF != CAP_SUPPORTED_PROTOCOL as u32 {
            return false;
        }
        let major = (dword0 >> 24) as u8;
        let first = (dword2 & 0xFF) as usize;
        let count = ((dword2 >> 8) & 0xFF) as usize;
        if first == 0 || count == 0 {
            return false;
        }
        let last = (first + count - 1).min(max_ports as usize);
        for port in first..=last {
            self.major[port] = major;
        }
        first <= last
    }

    /// The major revision (2 or 3) of `port`, if a capability named it.
    pub fn major(&self, port: u8) -> Option<u8> {
        Some(self.major[port as usize]).filter(|&m| m != 0)
    }

    /// Whether `port` is a USB3 port: by its capability, or, on a
    /// controller without one (none seen so far), by the speed its device
    /// negotiated (`portsc`), which only a USB3 port reports as SuperSpeed.
    pub fn usb3(&self, port: u8, portsc: u32) -> bool {
        match self.major(port) {
            Some(m) => m >= 3,
            None => op::port_speed(portsc) >= speed::SUPER,
        }
    }
}

/// How many times in a row a device left or changed while it was being
/// enumerated, per root port. A port that reaches the limit is left alone
/// until it is seen empty (`clear`), so a device that keeps bouncing does
/// not keep the idle loop enumerating it.
#[derive(Debug, Clone, Copy)]
pub struct PortBounces {
    counts: [u8; 256],
}

impl Default for PortBounces {
    fn default() -> Self {
        Self { counts: [0; 256] }
    }
}

impl PortBounces {
    /// One more change during an enumeration on `port`: true while the
    /// port may still be enumerated (fewer than `limit` in a row).
    pub fn note(&mut self, port: u8, limit: u8) -> bool {
        let c = &mut self.counts[port as usize];
        *c = c.saturating_add(1);
        *c < limit
    }

    /// Whether `port` reached `limit`.
    pub fn exhausted(&self, port: u8, limit: u8) -> bool {
        self.counts[port as usize] >= limit
    }

    /// `port` was seen empty, or its device was enumerated: counted afresh.
    pub fn clear(&mut self, port: u8) {
        self.counts[port as usize] = 0;
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
    fn a_reset_is_done_only_when_the_port_is_enabled_on_the_same_connection() {
        let hs = 3u32 << 10;
        let ss = 4u32 << 10;
        let base = PORTSC_POWER | PORTSC_CCS;
        assert_eq!(
            after_reset(base | PORTSC_RESET | hs, false),
            AfterReset::Waiting
        );
        assert_eq!(
            after_reset(base | hs, false),
            AfterReset::Waiting,
            "not enabled yet"
        );
        assert_eq!(
            after_reset(PORTSC_POWER | hs, false),
            AfterReset::Waiting,
            "connection not back yet"
        );
        assert_eq!(
            after_reset(base | PORTSC_PED | hs, false),
            AfterReset::Ready
        );
        // USB2: a connect change during the reset, still connected: again.
        assert_eq!(
            after_reset(base | PORTSC_PED | PORTSC_CSC | hs, false),
            AfterReset::Again
        );
        // USB3: the connection may bounce across a warm reset: ignored.
        assert_eq!(
            after_reset(base | PORTSC_PED | PORTSC_CSC | ss, true),
            AfterReset::Ready
        );
        // A USB2 port where a device negotiated a USB2 speed: the port's
        // protocol decides, not the speed.
        assert_eq!(
            after_reset(base | PORTSC_PED | PORTSC_CSC | hs, true),
            AfterReset::Ready,
            "a USB3 port with a bounce"
        );
        // A USB3 link still stuck where only a Warm Reset helps.
        assert_eq!(
            after_reset(base | pls(op::PLS_INACTIVE) | ss, true),
            AfterReset::Again
        );
    }

    #[test]
    fn port_protocols_come_from_the_supported_protocol_capabilities() {
        let mut p = PortProtocols::default();
        // USB 3.0 on ports 1-4, USB 2.0 on ports 5-12 (QEMU's layout).
        assert!(p.add(0x0300_0002, 4 << 8 | 1, 12));
        assert!(p.add(0x0200_0002, 8 << 8 | 5, 12));
        assert_eq!(p.major(1), Some(3));
        assert_eq!(p.major(4), Some(3));
        assert_eq!(p.major(5), Some(2));
        assert_eq!(p.major(12), Some(2));
        assert_eq!(p.major(13), None);
        let ss = 4u32 << 10;
        assert!(p.usb3(2, 0));
        assert!(!p.usb3(6, ss), "the port's protocol, not the speed");
        // No capability for a port: the speed is the fallback.
        assert!(p.usb3(200, ss) && !p.usb3(200, 3 << 10));
        // Another capability, an empty range, ports past max_ports.
        assert!(!p.add(0x0300_0001, 4 << 8 | 1, 12));
        assert!(!p.add(0x0300_0002, 1, 12));
        let mut q = PortProtocols::default();
        assert!(q.add(0x0300_0002, 200 << 8 | 10, 12));
        assert_eq!(q.major(12), Some(3));
        assert_eq!(q.major(13), None);
        assert!(!q.add(0x0300_0002, 1 << 8 | 13, 12));
    }

    #[test]
    fn a_port_whose_device_keeps_changing_is_left_alone_until_empty() {
        let mut b = PortBounces::default();
        assert!(b.note(5, 3));
        assert!(b.note(5, 3));
        assert!(!b.exhausted(5, 3));
        assert!(!b.note(5, 3), "the third one in a row reaches the limit");
        assert!(b.exhausted(5, 3));
        assert!(!b.exhausted(6, 3), "per port");
        b.clear(5);
        assert!(!b.exhausted(5, 3));
        assert!(b.note(255, 3));
    }

    #[test]
    fn the_policy_is_tuned_within_its_bounds() {
        let p = Policy::parse("debounce:150,recovery:20,attempts:4,retry:0,resets:2,bounces:5")
            .unwrap();
        assert_eq!(
            p,
            Policy {
                debounce_ms: 150,
                reset_recovery_ms: 20,
                attempts: 4,
                retry_ms: 0,
                reset_tries: 2,
                bounce_limit: 5
            }
        );
        assert_eq!(Policy::parse("").unwrap(), Policy::DEFAULT);
        assert_eq!(Policy::parse("attempts:2").unwrap().debounce_ms, 100);
        for bad in [
            "debounce:99",
            "debounce:2001",
            "recovery:9",
            "recovery:201",
            "bounces:1",
            "bounces:11",
            "attempts:0",
            "attempts:11",
            "resets:0",
            "retry:5001",
            "debounce",
            "debounce:x",
            "speed:3",
        ] {
            assert!(Policy::parse(bad).is_err(), "{bad}");
        }
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
