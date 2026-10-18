// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Bounded fault reporting (ADR 0028, incident 0012). A service that
//! refuses requests records every kind of refusal in the kernel event ring,
//! but at most once per [`WINDOW_MS`] per kind: the ring holds 64 entries
//! and a client in a loop would otherwise push every other event out of
//! it. What is not recorded is counted and travels with the next record of
//! the same kind, so no refusal disappears from the tally.

/// Length of the window during which a kind is recorded once.
pub const WINDOW_MS: u64 = 1000;

/// Per-kind meter over `N` kinds (the caller maps its kinds to indices).
pub struct Meter<const N: usize> {
    /// End of the current window of each kind (0: no window open).
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
        if *end != 0 && now_ms < *end {
            self.pending[kind] = self.pending[kind].saturating_add(1);
            return 0;
        }
        *end = now_ms.saturating_add(WINDOW_MS).max(1);
        let n = self.pending[kind].saturating_add(1);
        self.pending[kind] = 0;
        n
    }

    /// Refusals of `kind` counted but not yet recorded.
    pub fn held(&self, kind: usize) -> u32 {
        self.pending.get(kind).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_refusal_is_recorded_at_once() {
        let mut m = Meter::<3>::new();
        assert_eq!(m.report(0, 0), 1);
        assert_eq!(m.report(1, 5000), 1);
        assert_eq!(m.held(0), 0);
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
        assert_eq!(m.held(0), 999);
        // The next one after the window carries the whole count.
        assert_eq!(m.report(0, 100 + WINDOW_MS), 1000);
        assert_eq!(m.held(0), 0);
        // And opens a new window.
        assert_eq!(m.report(0, 100 + WINDOW_MS + 1), 0);
    }

    #[test]
    fn kinds_are_independent() {
        let mut m = Meter::<2>::new();
        assert_eq!(m.report(0, 10), 1);
        assert_eq!(m.report(0, 20), 0);
        assert_eq!(m.report(1, 20), 1);
        assert_eq!(m.report(1, 30), 0);
        assert_eq!(m.held(0), 1);
        assert_eq!(m.held(1), 1);
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
        assert_eq!(m.held(0), 2);
    }

    #[test]
    fn unknown_kind_is_always_reported() {
        let mut m = Meter::<1>::new();
        assert_eq!(m.report(7, 0), 1);
        assert_eq!(m.report(7, 0), 1);
        assert_eq!(m.held(7), 0);
    }

    #[test]
    fn count_saturates_instead_of_wrapping() {
        let mut m = Meter::<1>::new();
        m.pending[0] = u32::MAX - 1;
        m.window_end[0] = 10;
        assert_eq!(m.report(0, 5), 0);
        assert_eq!(m.held(0), u32::MAX);
        assert_eq!(m.report(0, 5), 0);
        assert_eq!(m.held(0), u32::MAX);
        assert_eq!(m.report(0, 10), u32::MAX);
    }
}
