// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The end of an installer `STAGED` run (ADR 0029, decision 5; incident
//! 0008, audit items M1 and A1): whether the `[staged]` list is consumed
//! and which code the run exits with, decided from what happened to each
//! entry.
//!
//! A *refusal* is the policy saying no to a package that was read fine
//! (tampered, unverified, a downgrade, the same version, an entry naming
//! another file): the list is consumed, so a later run cannot install the
//! same files again. A *failure* is a request that did not complete (a
//! read the file service did not answer, an open or a write that failed,
//! an installed copy that could not be read): nothing is known about the
//! package, the list is kept for inspection in `updater/state` (the next
//! `upgrade` stages again) and the run exits nonzero. A failure is never
//! counted as a refusal, whatever else happened in the run.

/// What happened to one staged entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    /// Refused by policy; `code` is the fault code recorded, or 0 when the
    /// refusal is not a fault (the same version already installed).
    Refused(u64),
    /// A request failed; `code` is the fault code recorded (never 0).
    Failed(u64),
}

/// The decision at the end of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedEnd {
    pub installed: usize,
    pub skipped: usize,
    /// The exit code: the first failure's code when anything failed;
    /// otherwise `prior` (a refusal recorded before the entries were
    /// handled, 0 if none); otherwise the first refusal's nonzero code; 0
    /// when everything was installed or merely already installed. The
    /// first failure wins so the code names what went wrong first, not
    /// what happened last.
    pub code: u64,
    /// Whether `[staged]` is to be emptied. False as soon as one entry
    /// failed: a failure must not consume what it did not handle.
    pub consume: bool,
}

/// Decides the end of a `STAGED` run from `outcomes` (one per entry, in
/// order) and `prior`, a refusal code recorded before the entries (0 if
/// none).
pub fn settle_staged(prior: u64, outcomes: &[Outcome]) -> StagedEnd {
    let installed = outcomes
        .iter()
        .filter(|o| **o == Outcome::Installed)
        .count();
    let first_failure = outcomes.iter().find_map(|o| match o {
        Outcome::Failed(code) => Some(*code),
        _ => None,
    });
    let first_refusal = outcomes.iter().find_map(|o| match o {
        Outcome::Refused(code) if *code != 0 => Some(*code),
        _ => None,
    });
    let code = match first_failure {
        Some(code) => code,
        None if prior != 0 => prior,
        None => first_refusal.unwrap_or(0),
    };
    StagedEnd {
        installed,
        skipped: outcomes.len() - installed,
        code,
        consume: first_failure.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{self, tests::Mock};
    use crate::lists;
    use alloc::string::String;

    const STATE: &str =
        "[installed]\nhello_ext 1\n[plan]\napp hello_ext 2\n[staged]\npkg-1.aripkg hello_ext 2 00\n";

    /// What the installer does with the decision: the state read once per
    /// run, the list emptied when consumed, left as it is when kept.
    fn settled_state(fs: &mut Mock, end: &StagedEnd) -> String {
        let state = files::read_text(fs, "updater/state", 16 * 1024)
            .expect("readable")
            .unwrap_or_default();
        if end.consume {
            lists::with_section(&state, lists::STAGED, "")
        } else {
            state
        }
    }

    #[test]
    fn everything_installed_consumes_with_code_zero() {
        let end = settle_staged(0, &[Outcome::Installed, Outcome::Installed]);
        assert_eq!(
            end,
            StagedEnd {
                installed: 2,
                skipped: 0,
                code: 0,
                consume: true
            }
        );
        let mut fs = Mock::with("updater/state", STATE.as_bytes());
        let after = settled_state(&mut fs, &end);
        assert_eq!(lists::section(&after, lists::STAGED).as_deref(), Some(""));
        assert_eq!(
            lists::section(&after, lists::PLAN).as_deref(),
            Some("app hello_ext 2\n")
        );
    }

    #[test]
    fn refusals_consume_and_report_the_first_fault_code() {
        // Already installed (no fault), then tampered (10), then a downgrade (8).
        let end = settle_staged(
            0,
            &[
                Outcome::Refused(0),
                Outcome::Refused(10),
                Outcome::Refused(8),
                Outcome::Installed,
            ],
        );
        assert_eq!(end.installed, 1);
        assert_eq!(end.skipped, 3);
        assert_eq!(end.code, 10);
        assert!(end.consume);
        // Only "already installed": no fault, exit 0, still consumed.
        let quiet = settle_staged(0, &[Outcome::Refused(0)]);
        assert_eq!(quiet.code, 0);
        assert!(quiet.consume);
        // A refusal recorded before the entries (malformed lines) wins
        // over later refusals: it was first.
        assert_eq!(settle_staged(10, &[Outcome::Refused(7)]).code, 10);
    }

    /// Incident 0008, audit M1: a transient read failure used to be
    /// "skipped", the list emptied and the run exited 0.
    #[test]
    fn a_failure_keeps_the_list_and_exits_nonzero() {
        let end = settle_staged(0, &[Outcome::Installed, Outcome::Failed(14)]);
        assert_eq!(end.installed, 1);
        assert_eq!(end.skipped, 1);
        assert_eq!(end.code, 14);
        assert!(!end.consume);
        let mut fs = Mock::with("updater/state", STATE.as_bytes());
        let after = settled_state(&mut fs, &end);
        assert_eq!(
            after, STATE,
            "a failure must leave the staged list as it was"
        );
    }

    #[test]
    fn a_failure_wins_over_refusals_whatever_the_order() {
        // Refusal first, failure later: the failure decides.
        let end = settle_staged(
            10,
            &[
                Outcome::Refused(7),
                Outcome::Failed(16),
                Outcome::Refused(8),
            ],
        );
        assert_eq!(end.code, 16);
        assert!(!end.consume);
        // Two failures: the first one names the exit code.
        let end = settle_staged(0, &[Outcome::Failed(16), Outcome::Failed(14)]);
        assert_eq!(end.code, 16);
        assert!(!end.consume);
        // Nothing staged at all: nothing to do, consumed (an empty list stays empty).
        let end = settle_staged(0, &[]);
        assert_eq!(
            end,
            StagedEnd {
                installed: 0,
                skipped: 0,
                code: 0,
                consume: true
            }
        );
    }
}
