// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! When `sshd` rereads `authorized_keys` (incident 0001 follow-up). While
//! the file service answers, the file is reread after every connection, so
//! a key added or removed from the console counts for the next client.
//! While it does not answer, every reread would hold the next client's
//! banner for the file client's whole wait; so the last keys read stay in
//! use and the reread is tried at most once per [`RETRY_MS`]. The first
//! failure and the recovery are reported once each, not every attempt.

/// Minimum time between rereads while the file service is failing.
pub const RETRY_MS: u64 = 60_000;

/// A change worth one log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// First failed reread: from now on the last keys stay in use.
    Failing,
    /// The file service answered again after failing.
    Recovered,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KeyRefresh {
    /// Time of the last attempt while failing; `None` while answering.
    failed_at: Option<u64>,
}

impl KeyRefresh {
    pub const fn new() -> Self {
        Self { failed_at: None }
    }

    /// Whether a reread should be attempted at `now` (ms).
    pub fn due(&self, now: u64) -> bool {
        self.failed_at
            .is_none_or(|t| now.saturating_sub(t) >= RETRY_MS)
    }

    /// Records a reread attempted at `now`: `answered` when the file
    /// service gave an answer (the content, or "no such file"). Returns
    /// the change to report, if any.
    pub fn record(&mut self, now: u64, answered: bool) -> Option<Change> {
        let was_failing = self.failed_at.is_some();
        self.failed_at = (!answered).then_some(now);
        match (was_failing, answered) {
            (false, false) => Some(Change::Failing),
            (true, true) => Some(Change::Recovered),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rereads_every_time_while_the_service_answers() {
        let mut r = KeyRefresh::new();
        for now in [0, 10, 20] {
            assert!(r.due(now));
            assert_eq!(r.record(now, true), None);
        }
    }

    #[test]
    fn while_failing_retries_at_most_once_a_minute_and_reports_once() {
        let mut r = KeyRefresh::new();
        assert!(r.due(1_000));
        assert_eq!(r.record(1_000, false), Some(Change::Failing));
        // Connections in the next minute do not touch the file service.
        assert!(!r.due(1_001));
        assert!(!r.due(1_000 + RETRY_MS - 1));
        assert!(r.due(1_000 + RETRY_MS));
        // A retry that fails again is not reported and restarts the minute.
        assert_eq!(r.record(1_000 + RETRY_MS, false), None);
        assert!(!r.due(1_000 + RETRY_MS + 1));
        assert!(r.due(1_000 + 2 * RETRY_MS));
    }

    #[test]
    fn recovery_is_reported_once_and_restores_rereads() {
        let mut r = KeyRefresh::new();
        r.record(0, false);
        assert_eq!(r.record(RETRY_MS, true), Some(Change::Recovered));
        assert!(r.due(RETRY_MS + 1));
        assert_eq!(r.record(RETRY_MS + 1, true), None);
    }
}
