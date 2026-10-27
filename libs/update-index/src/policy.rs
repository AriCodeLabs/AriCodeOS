// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Version rules: comparison, the downgrade rule, the best offer among
//! several sources and rollback protection for indexes.

use crate::index::{AppEntry, Index, IndexError, SystemEntry};
use crate::lists::DamagedFloor;
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

/// A system image as the install rules see it: its channel, version (the
/// build number for `Dev`) and, for a development build, its commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemId<'a> {
    pub channel: Channel,
    pub version: u32,
    pub commit: Option<&'a str>,
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
    /// Same channel and number but another commit: a development build
    /// number published twice (two checkouts, a lost counter). Never taken
    /// for the installed build, never installed over it silently.
    Conflict,
}

/// Compares the installed system (if known) with an offered one.
pub fn compare_system(installed: Option<SystemId>, offered: SystemId) -> SystemChange {
    match installed {
        None => SystemChange::Unknown,
        Some(i) if i.channel != offered.channel => SystemChange::Switch {
            from: i.channel,
            to: offered.channel,
        },
        Some(i) if i.version == offered.version && i.commit != offered.commit => {
            SystemChange::Conflict
        }
        Some(i) => SystemChange::Within(compare(Some(i.version), offered.version)),
    }
}

/// Why a system install is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Older than the installed one, same channel (anti-replay).
    Downgrade,
    /// A stable release older than the newest stable release this machine
    /// ran (the stable floor, audit of ADR 0045 M1): a downgrade, even when
    /// it arrives as a channel switch.
    BelowStableFloor(u32),
    /// A stable release while the stable floor is damaged (ADR 0060): the
    /// newest stable release this machine ran is unknown, so any stable
    /// release could be below it.
    FloorDamaged,
    /// The installed version is unknown (ADR 0029, audit 1 H1).
    Unknown,
    /// Another channel, and the owner did not name it (`--channel`), or the
    /// command never changes the channel (`update apply`).
    SwitchNotExplicit,
    /// [`SystemChange::Conflict`].
    Conflict,
}

/// The outcome of [`decide_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemDecision {
    Install,
    /// The same build of the same channel is installed.
    AlreadyInstalled,
    Refused(Refusal),
}

/// What the owner allowed for one install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemRules {
    /// `--allow-downgrade`.
    pub allow_downgrade: bool,
    /// `--channel` named the offered image's channel (`upgrade`); `false`
    /// for `update apply`, which never changes the channel.
    pub explicit_channel: bool,
    /// The newest stable release this machine ran, as far as known.
    pub stable_floor: StableFloor,
}

/// The stable floor as one run knows it (audit of ADR 0045 M1, ADR 0060).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StableFloor {
    /// None recorded: a machine from before the floor, or one that ran
    /// development builds only.
    #[default]
    Unset,
    /// The newest stable release this machine ran.
    At(u32),
    /// The record is damaged: that release is unknown.
    Damaged,
}

/// The install rule for a system image (ADR 0029 decision 5, ADR 0045
/// decision 2): within a channel, no downgrade without `--allow-downgrade`
/// and never a reinstall; across channels, only when the owner named the
/// channel, whatever the numbers say, since a build number and a release
/// version are not comparable; never a stable release below the stable
/// floor without `--allow-downgrade`, switch or not, nor any stable
/// release while the floor is damaged (ADR 0060); an unknown installed
/// version only with `--allow-downgrade`, and then, without a named
/// channel, only a stable image; a build number seen with another commit
/// never.
pub fn decide_system(
    change: SystemChange,
    offered: SystemId,
    rules: SystemRules,
) -> SystemDecision {
    use SystemDecision::{AlreadyInstalled, Install, Refused};
    match change {
        SystemChange::Within(Change::Same) => return AlreadyInstalled,
        SystemChange::Conflict => return Refused(Refusal::Conflict),
        // Older than the installed one: that is the reason to give, the
        // floor would only repeat it.
        SystemChange::Within(c) if !c.permitted(rules.allow_downgrade) => {
            return Refused(Refusal::Downgrade)
        }
        _ => {}
    }
    if offered.channel == Channel::Stable && !rules.allow_downgrade {
        match rules.stable_floor {
            StableFloor::At(floor) if offered.version < floor => {
                return Refused(Refusal::BelowStableFloor(floor))
            }
            StableFloor::Damaged => return Refused(Refusal::FloorDamaged),
            _ => {}
        }
    }
    match change {
        SystemChange::Within(c) if c.permitted(rules.allow_downgrade) => Install,
        SystemChange::Within(_) => Refused(Refusal::Downgrade),
        SystemChange::Unknown if !rules.allow_downgrade => Refused(Refusal::Unknown),
        SystemChange::Unknown if rules.explicit_channel || offered.channel == Channel::Stable => {
            Install
        }
        SystemChange::Unknown => Refused(Refusal::SwitchNotExplicit),
        SystemChange::Switch { .. } if rules.explicit_channel => Install,
        SystemChange::Switch { .. } => Refused(Refusal::SwitchNotExplicit),
        SystemChange::Conflict => Refused(Refusal::Conflict),
    }
}

/// The new stable floor once the active system is `installed`: the newest
/// stable release this machine ran never goes down (a rollback or a switch
/// to the development channel keeps it).
pub fn raise_floor(floor: Option<u32>, installed: Option<SystemId>) -> Option<u32> {
    match installed {
        Some(i) if i.channel == Channel::Stable => {
            Some(floor.map_or(i.version, |f| f.max(i.version)))
        }
        _ => floor,
    }
}

/// The floor of one run (ADR 0060), from the `[floor]` section as read
/// (`stored`) and the active system (`installed`), and the version to save
/// when it changes. A sound floor is raised to the active stable release
/// ([`raise_floor`]). A damaged one is never replaced here, since whatever
/// replaced it could be lower than what it held: it stays damaged, which
/// refuses every stable release ([`decide_system`]) until an install the
/// owner forced rewrites it ([`floor_rewrite`]).
pub fn floor_for_run(
    stored: Result<Option<u32>, DamagedFloor>,
    installed: Option<SystemId>,
) -> (StableFloor, Option<u32>) {
    match stored {
        Ok(floor) => {
            let raised = raise_floor(floor, installed);
            (
                raised.map_or(StableFloor::Unset, StableFloor::At),
                raised.filter(|_| raised != floor),
            )
        }
        Err(DamagedFloor) => (StableFloor::Damaged, None),
    }
}

/// What to write over a damaged floor once a stable image was installed
/// with `--allow-downgrade` (ADR 0060), the consent the floor asks for
/// anyway: the active stable release, the newest one this machine is still
/// known to have run. Only after the image verified and went into its slot
/// (audit of incident 0045, L4): a refused or invalid image changes
/// nothing. With a development build or an unknown system active there is
/// nothing to write.
pub fn floor_rewrite(
    stored: Result<Option<u32>, DamagedFloor>,
    installed: Option<SystemId>,
    offered: SystemId,
    allow_downgrade: bool,
) -> Option<u32> {
    let forced = stored.is_err() && allow_downgrade && offered.channel == Channel::Stable;
    raise_floor(None, installed).filter(|_| forced)
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

    fn id(channel: Channel, version: u32, commit: Option<&'static str>) -> SystemId<'static> {
        SystemId {
            channel,
            version,
            commit,
        }
    }

    const UPGRADE: SystemRules = SystemRules {
        allow_downgrade: false,
        explicit_channel: false,
        stable_floor: StableFloor::Unset,
    };

    #[test]
    fn channel_switches_need_the_owner() {
        use Channel::{Dev, Stable};
        let dev7 = id(Dev, 7, Some("0123abc"));
        let stable12 = id(Stable, 12, None);
        // Same channel: the ADR 0029 rules, anti-replay unchanged.
        assert_eq!(
            compare_system(Some(dev7), id(Dev, 8, Some("4567def"))),
            SystemChange::Within(Change::Upgrade)
        );
        assert_eq!(
            compare_system(Some(dev7), id(Dev, 6, Some("4567def"))),
            SystemChange::Within(Change::Downgrade)
        );
        assert_eq!(
            compare_system(Some(stable12), stable12),
            SystemChange::Within(Change::Same)
        );
        assert_eq!(
            compare_system(Some(dev7), dev7),
            SystemChange::Within(Change::Same)
        );
        assert_eq!(compare_system(None, dev7), SystemChange::Unknown);
        let back = compare_system(Some(dev7), id(Stable, 2, None));
        assert_eq!(
            back,
            SystemChange::Switch {
                from: Dev,
                to: Stable
            }
        );
        let to_dev = compare_system(Some(stable12), id(Dev, 1, Some("0123abc")));
        assert_eq!(
            to_dev,
            SystemChange::Switch {
                from: Stable,
                to: Dev
            }
        );
        for explicit in [false, true] {
            let rules = |allow_downgrade| SystemRules {
                allow_downgrade,
                explicit_channel: explicit,
                stable_floor: StableFloor::Unset,
            };
            let offered = id(Dev, 6, Some("0123abc"));
            let older = SystemChange::Within(Change::Downgrade);
            assert_eq!(
                decide_system(older, offered, rules(false)),
                SystemDecision::Refused(Refusal::Downgrade)
            );
            assert_eq!(
                decide_system(older, offered, rules(true)),
                SystemDecision::Install
            );
            let newer = SystemChange::Within(Change::Upgrade);
            assert_eq!(
                decide_system(newer, offered, rules(false)),
                SystemDecision::Install
            );
            let same = SystemChange::Within(Change::Same);
            assert_eq!(
                decide_system(same, offered, rules(true)),
                SystemDecision::AlreadyInstalled
            );
            assert_eq!(
                decide_system(SystemChange::Unknown, stable12, rules(false)),
                SystemDecision::Refused(Refusal::Unknown)
            );
        }
        // Across channels: only when named, even when the stable release is
        // numerically lower than the development build, and never through
        // --allow-downgrade alone.
        for (switch, offered) in [
            (back, id(Stable, 2, None)),
            (to_dev, id(Dev, 1, Some("0123abc"))),
        ] {
            let named = SystemRules {
                explicit_channel: true,
                ..UPGRADE
            };
            assert_eq!(
                decide_system(switch, offered, named),
                SystemDecision::Install
            );
            assert_eq!(
                decide_system(switch, offered, UPGRADE),
                SystemDecision::Refused(Refusal::SwitchNotExplicit)
            );
            let forced = SystemRules {
                allow_downgrade: true,
                ..UPGRADE
            };
            assert_eq!(
                decide_system(switch, offered, forced),
                SystemDecision::Refused(Refusal::SwitchNotExplicit)
            );
        }
    }

    #[test]
    fn a_switch_back_never_goes_below_the_stable_floor() {
        // Audit of ADR 0045, M1: a machine that ran stable 12, then
        // development builds, is offered an old validly signed stable 5.
        use Channel::{Dev, Stable};
        let floor = raise_floor(None, Some(id(Stable, 12, None)));
        assert_eq!(floor, Some(12));
        let floor = raise_floor(floor, Some(id(Dev, 7, Some("0123abc"))));
        assert_eq!(floor, Some(12), "a development build keeps the floor");
        let floor = raise_floor(floor, Some(id(Stable, 9, None)));
        assert_eq!(floor, Some(12), "a rollback keeps the floor");
        let named = SystemRules {
            explicit_channel: true,
            stable_floor: floor.map_or(StableFloor::Unset, StableFloor::At),
            ..UPGRADE
        };
        let installed = id(Dev, 7, Some("0123abc"));
        for (version, want) in [
            (5, SystemDecision::Refused(Refusal::BelowStableFloor(12))),
            (12, SystemDecision::Install),
            (13, SystemDecision::Install),
        ] {
            let offered = id(Stable, version, None);
            let change = compare_system(Some(installed), offered);
            assert_eq!(
                decide_system(change, offered, named),
                want,
                "stable {version}"
            );
        }
        // --allow-downgrade is the owner's way below it, named a downgrade.
        let forced = SystemRules {
            allow_downgrade: true,
            ..named
        };
        let old = id(Stable, 5, None);
        assert_eq!(
            decide_system(compare_system(Some(installed), old), old, forced),
            SystemDecision::Install
        );
        // Within the stable channel too: after a rollback to 9, stable 10 is
        // still below the floor.
        let rolled_back = id(Stable, 9, None);
        let ten = id(Stable, 10, None);
        assert_eq!(
            decide_system(compare_system(Some(rolled_back), ten), ten, named),
            SystemDecision::Refused(Refusal::BelowStableFloor(12))
        );
        // Older than the installed release as well: named a plain downgrade.
        let twelve = id(Stable, 12, None);
        assert_eq!(
            decide_system(compare_system(Some(twelve), ten), ten, named),
            SystemDecision::Refused(Refusal::Downgrade)
        );
        // The floor says nothing about development builds.
        let dev = id(Dev, 1, Some("0123abc"));
        assert_eq!(
            decide_system(compare_system(Some(id(Stable, 12, None)), dev), dev, named),
            SystemDecision::Install
        );
    }

    #[test]
    fn a_build_number_with_another_commit_is_refused() {
        // Audit of ADR 0045, M2: two checkouts published build 5.
        let installed = id(Channel::Dev, 5, Some("0123abc"));
        let other = id(Channel::Dev, 5, Some("4567def"));
        let change = compare_system(Some(installed), other);
        assert_eq!(change, SystemChange::Conflict);
        for allow_downgrade in [false, true] {
            let rules = SystemRules {
                allow_downgrade,
                explicit_channel: true,
                stable_floor: StableFloor::Unset,
            };
            assert_eq!(
                decide_system(change, other, rules),
                SystemDecision::Refused(Refusal::Conflict)
            );
        }
    }

    #[test]
    fn apply_never_changes_the_channel() {
        // Audit of ADR 0045, L4: `update apply` passes no channel; with an
        // unknown installed version and --allow-downgrade it takes a
        // stable image only.
        let apply = SystemRules {
            allow_downgrade: true,
            explicit_channel: false,
            stable_floor: StableFloor::Unset,
        };
        let dev = id(Channel::Dev, 5, Some("0123abc"));
        assert_eq!(
            decide_system(SystemChange::Unknown, dev, apply),
            SystemDecision::Refused(Refusal::SwitchNotExplicit)
        );
        let stable = id(Channel::Stable, 3, None);
        assert_eq!(
            decide_system(SystemChange::Unknown, stable, apply),
            SystemDecision::Install
        );
        let switch = compare_system(Some(id(Channel::Stable, 2, None)), dev);
        assert_eq!(
            decide_system(switch, dev, apply),
            SystemDecision::Refused(Refusal::SwitchNotExplicit)
        );
    }

    #[test]
    fn apply_never_goes_below_the_stable_floor() {
        // `update apply` keeps the floor (no open downgrade path): stable 2
        // in UPDATE, stable 1 running, stable 3 once ran.
        let apply = SystemRules {
            allow_downgrade: false,
            explicit_channel: false,
            stable_floor: StableFloor::At(3),
        };
        let running = id(Channel::Stable, 1, None);
        let two = id(Channel::Stable, 2, None);
        let change = compare_system(Some(running), two);
        assert_eq!(change, SystemChange::Within(Change::Upgrade));
        assert_eq!(
            decide_system(change, two, apply),
            SystemDecision::Refused(Refusal::BelowStableFloor(3))
        );
        let forced = SystemRules {
            allow_downgrade: true,
            ..apply
        };
        assert_eq!(decide_system(change, two, forced), SystemDecision::Install);
        // Older than the running release too: the plain downgrade reason.
        let older = compare_system(
            Some(id(Channel::Stable, 2, None)),
            id(Channel::Stable, 1, None),
        );
        assert_eq!(
            decide_system(older, id(Channel::Stable, 1, None), apply),
            SystemDecision::Refused(Refusal::Downgrade)
        );
        // At or above the floor: installed as before.
        let four = id(Channel::Stable, 4, None);
        assert_eq!(
            decide_system(compare_system(Some(running), four), four, apply),
            SystemDecision::Install
        );
    }

    #[test]
    fn a_damaged_floor_refuses_every_stable_release() {
        // Incident 0045: the machine ran stable 3, runs stable 1 (rolled
        // back with --allow-downgrade) and its `[floor]` is damaged. The
        // floor was dropped and saved again as 1, the running release, so
        // stable 2 went in as an upgrade.
        use crate::lists;
        use Channel::{Dev, Stable};
        let body = "stable 3x\n";
        assert_eq!(lists::parse_floor(body), Err(DamagedFloor));
        let running = id(Stable, 1, None);
        let (floor, save) = floor_for_run(lists::parse_floor(body), Some(running));
        assert_eq!(
            (floor, save),
            (StableFloor::Damaged, None),
            "never replaced"
        );
        let rules = SystemRules {
            stable_floor: floor,
            ..UPGRADE
        };
        for version in [2, 4, 400] {
            let offered = id(Stable, version, None);
            assert_eq!(
                decide_system(compare_system(Some(running), offered), offered, rules),
                SystemDecision::Refused(Refusal::FloorDamaged),
                "stable {version}"
            );
        }
        // Older than the running release: the plain downgrade, as before.
        let two = id(Stable, 2, None);
        assert_eq!(
            decide_system(compare_system(Some(id(Stable, 3, None)), two), two, rules),
            SystemDecision::Refused(Refusal::Downgrade)
        );
        // The floor says nothing about development builds.
        let dev = id(Dev, 8, Some("0123abc"));
        let named = SystemRules {
            explicit_channel: true,
            ..rules
        };
        assert_eq!(
            decide_system(
                compare_system(Some(id(Dev, 7, Some("4567def"))), dev),
                dev,
                named
            ),
            SystemDecision::Install
        );
        // --allow-downgrade installs; the floor is rewritten as the running
        // stable release only by the install that went in.
        let forced = SystemRules {
            allow_downgrade: true,
            ..rules
        };
        assert_eq!(
            decide_system(compare_system(Some(running), two), two, forced),
            SystemDecision::Install
        );
        let damaged = lists::parse_floor(body);
        assert_eq!(floor_rewrite(damaged, Some(running), two, true), Some(1));
        // ... not without --allow-downgrade, not for a development build,
        // not over a sound floor, and not with a development build or an
        // unknown system active.
        let dev7 = id(Dev, 7, Some("0123abc"));
        assert_eq!(floor_rewrite(damaged, Some(running), two, false), None);
        assert_eq!(floor_rewrite(damaged, Some(running), dev, true), None);
        assert_eq!(floor_rewrite(Ok(Some(3)), Some(running), two, true), None);
        for installed in [Some(dev7), None] {
            assert_eq!(floor_rewrite(damaged, installed, two, true), None);
        }
        // A sound floor is raised and saved only when it changes.
        assert_eq!(
            floor_for_run(Ok(Some(3)), Some(running)),
            (StableFloor::At(3), None)
        );
        assert_eq!(
            floor_for_run(Ok(Some(3)), Some(id(Stable, 5, None))),
            (StableFloor::At(5), Some(5))
        );
        assert_eq!(
            floor_for_run(Ok(None), Some(dev7)),
            (StableFloor::Unset, None)
        );
    }

    /// Audit of incident 0045, L4: `update apply --allow-downgrade` saved
    /// the rewritten floor before UPDATE was even read, so an invalid or
    /// development image still lowered it. Reading the floor never saves
    /// over a damaged one, with or without the flag.
    #[test]
    fn reading_a_damaged_floor_never_saves_it() {
        let running = id(Channel::Stable, 1, None);
        assert_eq!(
            floor_for_run(Err(DamagedFloor), Some(running)),
            (StableFloor::Damaged, None)
        );
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
