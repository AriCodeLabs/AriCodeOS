// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Version rules: comparison, the downgrade rule, the best offer among
//! several sources and rollback protection for indexes.

use crate::index::{AppEntry, Index, IndexError, SystemEntry};
use aricode_ab::Channel;

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

/// What installing an offered system image would do, channels included
/// (ADR 0045).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemChange {
    /// Same channel: the ADR 0029 rules ([`Change`]).
    Within(Change),
    /// The installed version cannot be read or verified.
    Unknown,
    /// The offered image belongs to the other channel.
    Switch { from: Channel, to: Channel },
}

/// Compares the installed system (channel and version, if known) with an
/// offered one.
pub fn compare_system(installed: Option<(Channel, u32)>, offered: (Channel, u32)) -> SystemChange {
    match installed {
        None => SystemChange::Unknown,
        Some((channel, version)) if channel == offered.0 => {
            SystemChange::Within(compare(Some(version), offered.1))
        }
        Some((from, _)) => SystemChange::Switch {
            from,
            to: offered.0,
        },
    }
}

/// Why a system install is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Older than the installed one, same channel (anti-replay).
    Downgrade,
    /// The installed version is unknown (ADR 0029, audit 1 H1).
    Unknown,
    /// Another channel, and the owner did not name it (`--channel`).
    SwitchNotExplicit,
}

/// The outcome of [`decide_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemDecision {
    Install,
    /// The same version of the same channel is installed.
    AlreadyInstalled,
    Refused(Refusal),
}

/// The install rule for a system image (ADR 0029 decision 5, ADR 0045
/// decision 2): within a channel, no downgrade without `--allow-downgrade`
/// and never a reinstall; across channels, only when the owner named the
/// channel (`upgrade --channel stable|dev`), whatever the numbers say,
/// since a build number and a release version are not comparable; an
/// unknown installed version only with `--allow-downgrade`.
pub fn decide_system(
    change: SystemChange,
    allow_downgrade: bool,
    explicit_channel: bool,
) -> SystemDecision {
    match change {
        SystemChange::Within(Change::Same) => SystemDecision::AlreadyInstalled,
        SystemChange::Within(c) if c.permitted(allow_downgrade) => SystemDecision::Install,
        SystemChange::Within(_) => SystemDecision::Refused(Refusal::Downgrade),
        SystemChange::Unknown if allow_downgrade => SystemDecision::Install,
        SystemChange::Unknown => SystemDecision::Refused(Refusal::Unknown),
        SystemChange::Switch { .. } if explicit_channel => SystemDecision::Install,
        SystemChange::Switch { .. } => SystemDecision::Refused(Refusal::SwitchNotExplicit),
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

    #[test]
    fn channel_switches_need_the_owner() {
        use Channel::{Dev, Stable};
        // Same channel: the ADR 0029 rules, anti-replay unchanged.
        assert_eq!(
            compare_system(Some((Dev, 7)), (Dev, 8)),
            SystemChange::Within(Change::Upgrade)
        );
        assert_eq!(
            compare_system(Some((Dev, 7)), (Dev, 6)),
            SystemChange::Within(Change::Downgrade)
        );
        assert_eq!(
            compare_system(Some((Stable, 12)), (Stable, 12)),
            SystemChange::Within(Change::Same)
        );
        assert_eq!(compare_system(None, (Dev, 1)), SystemChange::Unknown);
        let back = compare_system(Some((Dev, 7)), (Stable, 2));
        assert_eq!(
            back,
            SystemChange::Switch {
                from: Dev,
                to: Stable
            }
        );
        let to_dev = compare_system(Some((Stable, 12)), (Dev, 1));
        assert_eq!(
            to_dev,
            SystemChange::Switch {
                from: Stable,
                to: Dev
            }
        );
        // Within a channel, `--channel` changes nothing.
        for explicit in [false, true] {
            let older = SystemChange::Within(Change::Downgrade);
            assert_eq!(
                decide_system(older, false, explicit),
                SystemDecision::Refused(Refusal::Downgrade)
            );
            assert_eq!(
                decide_system(older, true, explicit),
                SystemDecision::Install
            );
            let newer = SystemChange::Within(Change::Upgrade);
            assert_eq!(
                decide_system(newer, false, explicit),
                SystemDecision::Install
            );
            let same = SystemChange::Within(Change::Same);
            assert_eq!(
                decide_system(same, true, explicit),
                SystemDecision::AlreadyInstalled
            );
            assert_eq!(
                decide_system(SystemChange::Unknown, false, explicit),
                SystemDecision::Refused(Refusal::Unknown)
            );
        }
        // Across channels: only when named, even when the stable release is
        // numerically lower than the development build, and never through
        // --allow-downgrade alone.
        for switch in [back, to_dev] {
            assert_eq!(decide_system(switch, false, true), SystemDecision::Install);
            assert_eq!(
                decide_system(switch, false, false),
                SystemDecision::Refused(Refusal::SwitchNotExplicit)
            );
            assert_eq!(
                decide_system(switch, true, false),
                SystemDecision::Refused(Refusal::SwitchNotExplicit)
            );
        }
    }

    fn index(serial: u64, system: Option<u32>, apps: &[(&'static str, u32)]) -> Index<'static> {
        Index {
            serial,
            channel: Channel::Stable,
            commit: None,
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
