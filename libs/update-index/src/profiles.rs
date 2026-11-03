// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The profiles this machine follows (`updater/profiles`, ADR 0074): one
//! line per profile, its name and then the apps `upgrade` installed for it.
//!
//! ```text
//! desktop editor terminal
//! robot
//! ```
//!
//! The owner adds and removes profiles (`update profile add|remove`); the
//! updater appends an app when it stages it for a profile. What a profile
//! installs next is never read from here: it is what the signed indexes
//! say, read again on every run, so a source can add an app to a profile
//! and every machine following it installs that app on its next `upgrade`.
//! The apps recorded here are what leaving the profile may delete: never
//! an app the owner installed by other means.

use crate::index::{valid_app_name, Index};
use crate::{MAX_APPS, MAX_PROFILES};
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

/// A followed profile and the apps installed for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub installed: Vec<String>,
}

/// The followed profiles, in the order they were added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    list: Vec<Profile>,
}

impl Profiles {
    /// Parses the file; returns the profiles and how many lines were
    /// refused (a bad or repeated name, a bad or repeated app, more than
    /// `MAX_APPS` apps, a line past `MAX_PROFILES`), which are never acted
    /// upon. Blank lines are formatting.
    pub fn parse(text: &str) -> (Self, usize) {
        let mut profiles = Self::default();
        let mut rejected = 0;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut fields = line.split_whitespace();
            let name = fields.next().unwrap_or("");
            let apps: Vec<&str> = fields.collect();
            let apps_valid = apps.len() <= MAX_APPS
                && apps
                    .iter()
                    .enumerate()
                    .all(|(i, a)| valid_app_name(a) && !apps[..i].contains(a));
            if !apps_valid || profiles.add(name).is_err() {
                rejected += 1;
                continue;
            }
            for app in apps {
                profiles.record(name, app);
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
        if self.list.len() == MAX_PROFILES {
            return Err(ProfileError::Full);
        }
        self.list.push(Profile {
            name: String::from(name),
            installed: Vec::new(),
        });
        Ok(())
    }

    /// Stops following `name`; returns it with the apps installed for it.
    pub fn remove(&mut self, name: &str) -> Result<Profile, ProfileError> {
        let at = self
            .list
            .iter()
            .position(|p| p.name == name)
            .ok_or(ProfileError::NotFound)?;
        Ok(self.list.remove(at))
    }

    /// Records that `app` was installed for `profile` (once; nothing when
    /// the profile is not followed or already holds `MAX_APPS`).
    pub fn record(&mut self, profile: &str, app: &str) {
        if let Some(p) = self.list.iter_mut().find(|p| p.name == profile) {
            if !p.installed.iter().any(|a| a == app) && p.installed.len() < MAX_APPS {
                p.installed.push(String::from(app));
            }
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.list.iter().any(|p| p.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.list.iter().map(|p| p.name.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for p in &self.list {
            out.push_str(&p.name);
            for app in &p.installed {
                out.push(' ');
                out.push_str(app);
            }
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

/// What leaving a profile does to the apps installed for it (`removed`):
/// an app a followed profile still names moves to that profile's record,
/// and the rest is returned to be deleted. `remaining` must already lack
/// the removed profile. Every followed profile must be offered by
/// `indexes` (the verified indexes of the current sources), or nothing is
/// decided: `Err` names the first one no index offers, since an app it
/// would keep cannot be told apart from one to delete.
pub fn plan_removal(
    indexes: &[Index],
    removed: &Profile,
    remaining: &mut Profiles,
) -> Result<Vec<String>, String> {
    let mut definitions: Vec<(String, Vec<&str>)> = Vec::new();
    for name in remaining.names() {
        let apps = profile_apps(indexes, name).ok_or_else(|| String::from(name))?;
        definitions.push((String::from(name), apps));
    }
    let mut delete = Vec::new();
    for app in &removed.installed {
        match definitions
            .iter()
            .find(|(_, apps)| apps.contains(&app.as_str()))
        {
            Some((keeper, _)) => remaining.record(keeper, app),
            None => delete.push(app.clone()),
        }
    }
    Ok(delete)
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
        p.record("desktop", "editor");
        p.record("desktop", "editor");
        p.record("kiosk", "browser");
        assert_eq!(p.to_text(), "desktop editor\nr1\nr2\nr3\n");
        assert_eq!(Profiles::parse(&p.to_text()), (p.clone(), 0));
        let r2 = p.remove("r2").unwrap();
        assert_eq!((r2.name.as_str(), r2.installed.len()), ("r2", 0));
        assert_eq!(p.remove("r2"), Err(ProfileError::NotFound));
        assert_eq!(p.names().collect::<Vec<_>>(), ["desktop", "r1", "r3"]);
        // Damaged lines are counted and dropped whole; blank lines are not
        // damage.
        let (parsed, rejected) =
            Profiles::parse("desktop a b\n\nbad/name\ndesktop\r\nrobot c c\nkiosk d/e\nrobot b\n");
        assert_eq!(parsed.to_text(), "desktop a b\nrobot b\n");
        assert_eq!(rejected, 4);
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
    fn removal_deletes_only_what_the_profile_installed_and_nobody_needs() {
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
        // desktop installed a and b; c (named by the second source only)
        // was installed by the owner and is not desktop's to delete.
        let (mut followed, _) = Profiles::parse("desktop a b\nrobot\n");
        let removed = followed.remove("desktop").unwrap();
        assert_eq!(
            plan_removal(&indexes, &removed, &mut followed),
            Ok(vec![String::from("a")])
        );
        // robot names b, so b is now robot's.
        assert_eq!(followed.to_text(), "robot b\n");
        // Alone, everything desktop installed goes.
        let (mut alone, _) = Profiles::parse("desktop a b\n");
        let removed = alone.remove("desktop").unwrap();
        assert_eq!(
            plan_removal(&[], &removed, &mut alone),
            Ok(vec![String::from("a"), String::from("b")])
        );
        // A followed profile no index offers: nothing is decided.
        let (mut unknown, _) = Profiles::parse("desktop a b\nkiosk\n");
        let removed = unknown.remove("desktop").unwrap();
        assert_eq!(
            plan_removal(&indexes, &removed, &mut unknown),
            Err(String::from("kiosk"))
        );
    }
}
