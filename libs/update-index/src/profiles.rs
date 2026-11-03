// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The profiles this machine follows (`updater/profiles`, ADR 0074): one
//! profile name per line, edited by the owner with `update profile add` and
//! `update profile remove`.
//!
//! ```text
//! desktop
//! ```
//!
//! What a profile installs is never written here: it is what the signed
//! indexes say, read again on every run, so a source can add an app to a
//! profile and every machine following it installs that app on its next
//! `upgrade`.

use crate::index::{valid_app_name, Index};
use crate::MAX_PROFILES;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// Not a valid profile name (the app name rule).
    Invalid,
    /// Already followed.
    Duplicate,
    /// `MAX_PROFILES` already followed.
    Full,
    /// `update profile remove` named a profile not followed.
    NotFound,
}

/// The followed profiles, in the order they were added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    names: Vec<String>,
}

impl Profiles {
    /// Parses the file; returns the profiles and how many lines were
    /// refused (a bad name, a repeated one or one past `MAX_PROFILES`),
    /// which are never acted upon. Blank lines are formatting.
    pub fn parse(text: &str) -> (Self, usize) {
        let mut profiles = Self::default();
        let mut rejected = 0;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if profiles.add(line).is_err() {
                rejected += 1;
            }
        }
        (profiles, rejected)
    }

    pub fn add(&mut self, name: &str) -> Result<(), ProfileError> {
        if !valid_app_name(name) {
            return Err(ProfileError::Invalid);
        }
        if self.contains(name) {
            return Err(ProfileError::Duplicate);
        }
        if self.names.len() == MAX_PROFILES {
            return Err(ProfileError::Full);
        }
        self.names.push(String::from(name));
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), ProfileError> {
        let at = self
            .names
            .iter()
            .position(|n| n == name)
            .ok_or(ProfileError::NotFound)?;
        self.names.remove(at);
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for name in &self.names {
            out.push_str(name);
            out.push('\n');
        }
        out
    }
}

/// The apps profile `name` installs: from the first index (in source
/// preference order) that offers it. `None` when no index does.
pub fn profile_apps<'a>(indexes: &[Index<'a>], name: &str) -> Option<Vec<&'a str>> {
    indexes
        .iter()
        .find_map(|index| index.profile(name))
        .map(|p| p.apps.clone())
}

/// The apps that leaving profile `removed` makes unneeded: those it
/// installs that no profile still followed installs. An app another
/// profile also lists stays. `None` when no index offers `removed`, so
/// nothing is known to delete.
pub fn apps_to_remove<'a>(
    indexes: &[Index<'a>],
    removed: &str,
    remaining: &Profiles,
) -> Option<Vec<&'a str>> {
    let mut apps = profile_apps(indexes, removed)?;
    let kept: Vec<&str> = remaining
        .names()
        .iter()
        .filter(|n| n.as_str() != removed)
        .filter_map(|n| profile_apps(indexes, n))
        .flatten()
        .collect();
    apps.retain(|a| !kept.contains(a));
    Some(apps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{AppEntry, ProfileEntry};
    use alloc::vec;
    use aricode_ab::Channel;

    #[test]
    fn edit_and_parse_round_trip() {
        let mut p = Profiles::default();
        assert_eq!(p.add("desktop"), Ok(()));
        assert_eq!(p.add("desktop"), Err(ProfileError::Duplicate));
        assert_eq!(p.add("a/b"), Err(ProfileError::Invalid));
        assert_eq!(p.add(""), Err(ProfileError::Invalid));
        for name in ["r1", "r2", "r3"] {
            assert_eq!(p.add(name), Ok(()));
        }
        assert_eq!(p.add("r4"), Err(ProfileError::Full));
        assert_eq!(Profiles::parse(&p.to_text()), (p.clone(), 0));
        assert_eq!(p.remove("r2"), Ok(()));
        assert_eq!(p.remove("r2"), Err(ProfileError::NotFound));
        assert_eq!(p.names(), ["desktop", "r1", "r3"]);
        // Damaged lines are counted and dropped; blank lines are not damage.
        let (parsed, rejected) = Profiles::parse("desktop\n\nbad name\ndesktop\r\nrobot\n");
        assert_eq!(parsed.names(), ["desktop", "robot"]);
        assert_eq!(rejected, 2);
    }

    fn index(profiles: Vec<ProfileEntry<'static>>) -> Index<'static> {
        let apps = ["a", "b", "c"]
            .iter()
            .map(|&name| AppEntry {
                name,
                version: 1,
                file: "x.aripkg",
                size: 1,
                sha256: [0; 32],
            })
            .collect();
        Index {
            serial: 1,
            channel: Channel::Stable,
            commit: None,
            system: None,
            apps,
            profiles,
        }
    }

    #[test]
    fn removal_keeps_what_another_profile_installs() {
        let indexes = vec![
            index(vec![ProfileEntry {
                name: "desktop",
                apps: vec!["a", "b"],
            }]),
            index(vec![
                ProfileEntry {
                    name: "desktop",
                    apps: vec!["c"],
                },
                ProfileEntry {
                    name: "robot",
                    apps: vec!["b"],
                },
            ]),
        ];
        // The first source offering a profile defines it.
        assert_eq!(profile_apps(&indexes, "desktop"), Some(vec!["a", "b"]));
        assert_eq!(profile_apps(&indexes, "kiosk"), None);
        let (mut followed, _) = Profiles::parse("desktop\nrobot\n");
        followed.remove("desktop").unwrap();
        assert_eq!(
            apps_to_remove(&indexes, "desktop", &followed),
            Some(vec!["a"])
        );
        assert_eq!(
            apps_to_remove(&indexes, "desktop", &Profiles::default()),
            Some(vec!["a", "b"])
        );
        assert_eq!(apps_to_remove(&indexes, "kiosk", &followed), None);
    }
}
