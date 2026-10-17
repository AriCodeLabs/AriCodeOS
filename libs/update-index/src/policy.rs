// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Version rules: comparison, the downgrade rule, the best offer among
//! several sources and rollback protection for indexes.

use crate::index::{AppEntry, Index, IndexError, SystemEntry};

/// What installing an offered version would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing of that name is installed.
    New,
    /// The same version is installed.
    Same,
    /// The offered version is newer.
    Upgrade,
    /// The offered version is older than the installed one.
    Downgrade,
}

/// Compares an installed version (if any) with an offered one.
pub fn compare(installed: Option<u32>, offered: u32) -> Change {
    match installed {
        None => Change::New,
        Some(v) if offered == v => Change::Same,
        Some(v) if offered > v => Change::Upgrade,
        Some(_) => Change::Downgrade,
    }
}

impl Change {
    /// Whether an install of this kind may proceed. A downgrade needs the
    /// owner's explicit `--allow-downgrade` (ADR 0029, decision 5): a
    /// replayed old index or package, validly signed, must not roll the
    /// system back to a version with known flaws.
    pub fn permitted(self, allow_downgrade: bool) -> bool {
        match self {
            Change::New | Change::Upgrade => true,
            Change::Same => false,
            Change::Downgrade => allow_downgrade,
        }
    }
}

/// The newest system image among the indexes: (index position, entry).
/// On equal versions the first source wins (list order is the owner's
/// preference).
pub fn best_system<'i, 'a>(indexes: &'i [Index<'a>]) -> Option<(usize, &'i SystemEntry<'a>)> {
    let mut best: Option<(usize, &SystemEntry)> = None;
    for (i, index) in indexes.iter().enumerate() {
        if let Some(s) = &index.system {
            if best.is_none_or(|(_, b)| s.version > b.version) {
                best = Some((i, s));
            }
        }
    }
    best
}

/// The newest package of app `name` among the indexes, same rule as
/// [`best_system`].
pub fn best_app<'i, 'a>(indexes: &'i [Index<'a>], name: &str) -> Option<(usize, &'i AppEntry<'a>)> {
    let mut best: Option<(usize, &AppEntry)> = None;
    for (i, index) in indexes.iter().enumerate() {
        if let Some(a) = index.app(name) {
            if best.is_none_or(|(_, b)| a.version > b.version) {
                best = Some((i, a));
            }
        }
    }
    best
}

/// Rollback protection: a source's new index must not be older than the
/// one already stored for it (an attacker replaying an old, validly signed
/// index could otherwise hide newer fixes). Equal serials are the same
/// publication fetched again.
pub fn check_fresh(stored: Option<u64>, offered: u64) -> Result<(), IndexError> {
    match stored {
        Some(s) if offered < s => Err(IndexError::Stale { stored: s, offered }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn version_comparison() {
        assert_eq!(compare(None, 1), Change::New);
        assert_eq!(compare(Some(2), 2), Change::Same);
        assert_eq!(compare(Some(2), 3), Change::Upgrade);
        assert_eq!(compare(Some(3), 2), Change::Downgrade);
        assert_eq!(compare(Some(0), u32::MAX), Change::Upgrade);
        assert_eq!(compare(Some(u32::MAX), 0), Change::Downgrade);
    }

    #[test]
    fn downgrade_rule() {
        assert!(Change::Upgrade.permitted(false));
        assert!(Change::New.permitted(false));
        assert!(!Change::Same.permitted(false));
        assert!(
            !Change::Same.permitted(true),
            "reinstalling is never needed"
        );
        assert!(
            !Change::Downgrade.permitted(false),
            "no downgrade by default"
        );
        assert!(Change::Downgrade.permitted(true));
    }

    fn index(serial: u64, system: Option<u32>, apps: &[(&'static str, u32)]) -> Index<'static> {
        Index {
            serial,
            system: system.map(|version| SystemEntry {
                version,
                file: "s.img",
                size: 1,
                sha256: [0; 32],
            }),
            apps: apps
                .iter()
                .map(|&(name, version)| AppEntry {
                    name,
                    version,
                    file: "a.aripkg",
                    size: 1,
                    sha256: [0; 32],
                })
                .collect(),
        }
    }

    #[test]
    fn best_offer_across_sources() {
        let list = vec![
            index(1, Some(2), &[("a", 1), ("b", 5)]),
            index(1, None, &[("a", 3)]),
            index(1, Some(4), &[("a", 3)]),
            index(1, Some(4), &[]),
        ];
        assert_eq!(
            best_system(&list).map(|(i, s)| (i, s.version)),
            Some((2, 4))
        );
        // Tie on version 3: the first source in the list wins.
        assert_eq!(
            best_app(&list, "a").map(|(i, a)| (i, a.version)),
            Some((1, 3))
        );
        assert_eq!(
            best_app(&list, "b").map(|(i, a)| (i, a.version)),
            Some((0, 5))
        );
        assert!(best_app(&list, "c").is_none());
        let empty: Vec<Index> = Vec::new();
        assert!(best_system(&empty).is_none());
        assert!(best_system(&list[1..2]).is_none());
    }

    #[test]
    fn rollback_protection() {
        assert_eq!(check_fresh(None, 1), Ok(()));
        assert_eq!(check_fresh(Some(5), 5), Ok(()));
        assert_eq!(check_fresh(Some(5), 6), Ok(()));
        assert_eq!(
            check_fresh(Some(5), 4),
            Err(IndexError::Stale {
                stored: 5,
                offered: 4
            })
        );
    }
}
