// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Bounded fault reporting (ADR 0028, incident 0012). A service that
//! refuses requests records every kind of refusal in the kernel event ring,
//! but at most once per [`WINDOW_MS`] per kind: the ring holds 64 entries
//! and a client in a loop would otherwise push every other event out of
//! it. What is not recorded is counted and recorded when the window ends
//! ([`Meter::drain_due`]), so no refusal disappears from the tally and a
//! burst followed by silence still shows its size.

/// Length of the window during which a kind is recorded once.
pub const WINDOW_MS: u64 = 1000;

/// Length of a kernel event name (`abi::sysinfo::EVENT_NAME_LEN`).
pub const LABEL_LEN: usize = 24;

/// Largest count a label shows; `" x999"` is five bytes, so a kind name of
/// up to `LABEL_LEN - 5` bytes always fits whole.
pub const LABEL_MAX_COUNT: u32 = 999;

/// Per-kind meter over `N` kinds (the caller maps its kinds to indices).
pub struct Meter<const N: usize> {
    /// End of the current window of each kind.
    window_end: [u64; N],
    /// Refusals of each kind not yet recorded.
    pending: [u32; N],
}

impl<const N: usize> Default for Meter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Meter<N> {
    pub const fn new() -> Self {
        Self {
            window_end: [0; N],
            pending: [0; N],
        }
    }

    /// Accounts one refusal of `kind` at `now_ms`. Returns the number of
    /// refusals to record now (this one plus those held back since the
    /// previous record), or 0 when the kind was recorded less than
    /// [`WINDOW_MS`] ago and this one only joins the count. An index beyond
    /// `N` is counted nowhere and always reported.
    pub fn report(&mut self, kind: usize, now_ms: u64) -> u32 {
        let Some(end) = self.window_end.get_mut(kind) else {
            return 1;
        };
        if now_ms < *end {
            self.pending[kind] = self.pending[kind].saturating_add(1);
            return 0;
        }
        *end = now_ms.saturating_add(WINDOW_MS);
        let n = self.pending[kind].saturating_add(1);
        self.pending[kind] = 0;
        n
    }

    /// One kind whose window has ended with refusals held back: returns
    /// `(kind, count)`, records them as done and opens a new window for the
    /// kind, so a burst that stops is still reported once its second is
    /// over and never more than once per second. `None` when nothing is
    /// due. Call it in a loop from the periodic work.
    pub fn drain_due(&mut self, now_ms: u64) -> Option<(usize, u32)> {
        let kind = (0..N).find(|&k| self.pending[k] > 0 && now_ms >= self.window_end[k])?;
        let count = self.pending[kind];
        self.pending[kind] = 0;
        self.window_end[kind] = now_ms.saturating_add(WINDOW_MS);
        Some((kind, count))
    }
}

/// `"<kind> x<count>"` as an event name: `count` is shown up to
/// [`LABEL_MAX_COUNT`], and a kind longer than the room left is cut so the
/// count always appears.
pub fn label<'a>(kind: &str, count: u32, buf: &'a mut [u8; LABEL_LEN]) -> &'a str {
    let mut digits = [0u8; 3];
    let mut n = count.min(LABEL_MAX_COUNT);
    let mut len = 0;
    loop {
        digits[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let room = LABEL_LEN - 2 - len;
    let mut cut = kind.len().min(room);
    while !kind.is_char_boundary(cut) {
        cut -= 1;
    }
    buf[..cut].copy_from_slice(&kind.as_bytes()[..cut]);
    buf[cut..cut + 2].copy_from_slice(b" x");
    for (i, d) in digits[..len].iter().rev().enumerate() {
        buf[cut + 2 + i] = *d;
    }
    // Only ASCII and a prefix cut on a char boundary were written.
    core::str::from_utf8(&buf[..cut + 2 + len]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_refusal_is_recorded_at_once() {
        let mut m = Meter::<3>::new();
        assert_eq!(m.report(0, 0), 1);
        assert_eq!(m.report(1, 5000), 1);
        assert_eq!(m.drain_due(5000), None);
    }

    /// A flood within the window yields a single record; the ring (64
    /// entries) cannot be filled by one misbehaving client.
    #[test]
    fn flood_within_a_window_records_once_and_counts_the_rest() {
        let mut m = Meter::<2>::new();
        let mut recorded = 0;
        for i in 0..1000u64 {
            if m.report(0, 100 + i) > 0 {
                recorded += 1;
            }
        }
        assert_eq!(recorded, 1);
        // The next one after the window carries the whole count.
        assert_eq!(m.report(0, 100 + WINDOW_MS), 1000);
        // And opens a new window whose count starts again from zero.
        assert_eq!(m.report(0, 100 + WINDOW_MS + 1), 0);
        assert_eq!(m.report(0, 100 + 2 * WINDOW_MS), 2);
    }

    /// A burst followed by silence is reported when its window ends, not
    /// only by the next refusal.
    #[test]
    fn held_refusals_are_drained_when_the_window_ends() {
        let mut m = Meter::<2>::new();
        assert_eq!(m.report(0, 10), 1);
        for t in 11..20 {
            assert_eq!(m.report(0, t), 0);
        }
        assert_eq!(m.drain_due(10 + WINDOW_MS - 1), None);
        assert_eq!(m.drain_due(10 + WINDOW_MS), Some((0, 9)));
        assert_eq!(m.drain_due(10 + WINDOW_MS), None);
        // The drain opened a new window: the next refusal is held again
        // (still at most one record per kind per second) and drained later.
        assert_eq!(m.report(0, 10 + WINDOW_MS + 1), 0);
        assert_eq!(m.drain_due(10 + 2 * WINDOW_MS - 1), None);
        assert_eq!(m.drain_due(10 + 2 * WINDOW_MS), Some((0, 1)));
    }

    #[test]
    fn drain_returns_every_due_kind_once() {
        let mut m = Meter::<3>::new();
        for k in 0..3 {
            assert_eq!(m.report(k, 0), 1);
            assert_eq!(m.report(k, 1), 0);
            assert_eq!(m.report(k, 2), 0);
        }
        let mut due = [
            m.drain_due(WINDOW_MS),
            m.drain_due(WINDOW_MS),
            m.drain_due(WINDOW_MS),
        ];
        due.sort();
        assert_eq!(due, [Some((0, 2)), Some((1, 2)), Some((2, 2))]);
        assert_eq!(m.drain_due(WINDOW_MS), None);
    }

    #[test]
    fn kinds_are_independent() {
        let mut m = Meter::<2>::new();
        assert_eq!(m.report(0, 10), 1);
        assert_eq!(m.report(0, 20), 0);
        assert_eq!(m.report(1, 20), 1);
        assert_eq!(m.report(1, 30), 0);
        assert_eq!(m.drain_due(20 + WINDOW_MS), Some((0, 1)));
        assert_eq!(m.drain_due(20 + WINDOW_MS), Some((1, 1)));
    }

    #[test]
    fn window_boundary_is_inclusive_of_its_end() {
        let mut m = Meter::<1>::new();
        assert_eq!(m.report(0, 0), 1);
        assert_eq!(m.report(0, WINDOW_MS - 1), 0);
        assert_eq!(m.report(0, WINDOW_MS), 2);
    }

    /// A clock stuck at 0 (no `SYSTEM` access) still records the first one
    /// and still bounds the rest.
    #[test]
    fn zero_clock_still_bounds() {
        let mut m = Meter::<1>::new();
        assert_eq!(m.report(0, 0), 1);
        assert_eq!(m.report(0, 0), 0);
        assert_eq!(m.report(0, 0), 0);
        assert_eq!(m.drain_due(0), None);
        assert_eq!(m.drain_due(WINDOW_MS), Some((0, 2)));
    }

    #[test]
    fn unknown_kind_is_always_reported() {
        let mut m = Meter::<1>::new();
        assert_eq!(m.report(7, 0), 1);
        assert_eq!(m.report(7, 0), 1);
        assert_eq!(m.drain_due(WINDOW_MS), None);
    }

    #[test]
    fn count_saturates_instead_of_wrapping() {
        let mut m = Meter::<1>::new();
        m.pending[0] = u32::MAX - 1;
        m.window_end[0] = 10;
        assert_eq!(m.report(0, 5), 0);
        assert_eq!(m.report(0, 5), 0);
        assert_eq!(m.report(0, 10), u32::MAX);
    }

    #[test]
    fn label_appends_the_count() {
        let mut b = [0u8; LABEL_LEN];
        assert_eq!(
            label("net tcp table full", 1, &mut b),
            "net tcp table full x1"
        );
        assert_eq!(label("net tx busy", 42, &mut b), "net tx busy x42");
        assert_eq!(label("net tx busy", 1000, &mut b), "net tx busy x999");
    }

    /// The longest kind netd uses (19 bytes) plus " x999" fills the name
    /// exactly; anything longer is cut, never the count.
    #[test]
    fn label_fits_the_event_name() {
        let mut b = [0u8; LABEL_LEN];
        let longest = "net ping table full";
        assert_eq!(longest.len() + 5, LABEL_LEN);
        assert_eq!(label(longest, 999, &mut b), "net ping table full x999");
        assert_eq!(
            label("net a kind far too long for a name", 7, &mut b),
            "net a kind far too lo x7"
        );
        let cut = label("ñññññññññññññññññññññññ", 10, &mut b);
        assert!(cut.len() <= LABEL_LEN && cut.ends_with(" x10"), "{cut}");
    }
}
