// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! What a machine boots into (`session`, ADR 0074): the text console, or
//! the desktop of the `desktop` profile.
//!
//! ```text
//! session = desktop
//! ```
//!
//! The shell writes it (`session desktop|shell`, and following or leaving
//! the `desktop` profile); init reads it at boot. The desktop starts only
//! when the machine also follows the `desktop` profile and has its
//! `launcher` installed: the file alone never boots into a desktop that is
//! not there.

use crate::files::{self, Files};
use crate::profiles::Profiles;

/// The file, at the top of the main namespace.
pub const FILE: &str = "session";
/// The profile whose desktop `session = desktop` starts.
pub const DESKTOP_PROFILE: &str = "desktop";
/// The application the desktop session starts; it holds `launch`.
pub const LAUNCHER: &str = "launcher";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// The text console (the default, and without the file).
    Shell,
    Desktop,
}

impl Session {
    /// Parses the file: one `session = shell|desktop` line, with blank
    /// lines and `#` comments around it. `None` for anything else, which
    /// init treats as the text console.
    pub fn parse(text: &str) -> Option<Self> {
        let mut found = None;
        for line in text
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
        {
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=')?;
            if key.trim() != "session" || found.is_some() {
                return None;
            }
            found = Some(Self::from_name(value.trim())?);
        }
        found
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "shell" => Some(Self::Shell),
            "desktop" => Some(Self::Desktop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Desktop => "desktop",
        }
    }

    pub fn to_text(self) -> alloc::string::String {
        alloc::format!("session = {}\n", self.name())
    }
}

/// Whether `profiles` (the text of `updater/profiles`) follows the
/// `desktop` profile. A damaged line follows nothing, as for the updater.
pub fn follows_desktop(profiles: &str) -> bool {
    Profiles::parse(profiles).0.contains(DESKTOP_PROFILE)
}

/// Whether `session = desktop` would start a desktop on the next boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// The `desktop` profile is not followed.
    NotFollowed,
    /// It is followed, but its launcher is not installed yet.
    NotInstalled,
    /// Whether the profile is followed cannot be read.
    ProfilesUnreadable,
    /// Whether the launcher is installed cannot be read.
    LauncherUnreadable,
}

impl Readiness {
    /// What a line about the desktop session ends with: nothing when
    /// ready, otherwise what keeps it from starting.
    pub fn note(self) -> &'static str {
        match self {
            Readiness::Ready => "",
            Readiness::NotFollowed => {
                " (it will not start: the desktop profile is not followed; update profile add desktop)"
            }
            Readiness::NotInstalled => {
                " (it will not start until the profile is installed: update, then upgrade)"
            }
            Readiness::ProfilesUnreadable => {
                " (whether the desktop profile is followed cannot be read)"
            }
            Readiness::LauncherUnreadable => " (whether the launcher is installed cannot be read)",
        }
    }
}

/// Whether the desktop would start: the profile followed
/// (`updater/profiles`) and its launcher installed.
pub fn desktop_readiness<F: Files>(fs: &mut F) -> Readiness {
    let profiles = files::read_optional(fs, &crate::main_path(crate::PROFILES_FILE), 1024);
    let followed = match &profiles {
        Err(_) => return Readiness::ProfilesUnreadable,
        Ok(text) => text
            .as_deref()
            .and_then(|t| core::str::from_utf8(t).ok())
            .is_some_and(follows_desktop),
    };
    if !followed {
        return Readiness::NotFollowed;
    }
    let Some(path) = crate::installed_path(LAUNCHER) else {
        return Readiness::NotInstalled;
    };
    match fs.open(&path) {
        Ok(Some(fd)) => {
            fs.close(fd);
            Readiness::Ready
        }
        Ok(None) => Readiness::NotInstalled,
        Err(_) => Readiness::LauncherUnreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files by name, or every request failing.
    struct Mem {
        files: alloc::vec::Vec<(&'static str, &'static [u8])>,
        fail: bool,
    }

    impl Files for Mem {
        type Fd = usize;
        type Error = ();
        fn open(&mut self, name: &str) -> Result<Option<usize>, ()> {
            if self.fail {
                return Err(());
            }
            Ok(self.files.iter().position(|(n, _)| *n == name))
        }
        fn read(&mut self, fd: &usize, offset: u32, out: &mut [u8]) -> Result<usize, ()> {
            let data = &self.files[*fd].1[(offset as usize).min(self.files[*fd].1.len())..];
            let n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
            Ok(n)
        }
        fn close(&mut self, _: usize) {}
        fn list(
            &mut self,
            _: u32,
            _: &mut [u8; files::NAME_LEN],
        ) -> Result<Option<(usize, u64)>, ()> {
            Ok(None)
        }
    }

    #[test]
    fn readiness_needs_the_profile_and_the_launcher() {
        let profiles = "updater/profiles";
        let mut m = Mem {
            files: alloc::vec![],
            fail: false,
        };
        assert_eq!(desktop_readiness(&mut m), Readiness::NotFollowed);
        m.files.push((profiles, b"desktop launcher\n"));
        assert_eq!(desktop_readiness(&mut m), Readiness::NotInstalled);
        m.files.push(("apps/launcher.aripkg", b"pkg"));
        assert_eq!(desktop_readiness(&mut m), Readiness::Ready);
        assert_eq!(Readiness::Ready.note(), "");
        m.fail = true;
        assert_eq!(desktop_readiness(&mut m), Readiness::ProfilesUnreadable);
    }

    #[test]
    fn one_line_round_trips_and_anything_else_is_refused() {
        for s in [Session::Shell, Session::Desktop] {
            assert_eq!(Session::parse(&s.to_text()), Some(s));
        }
        assert_eq!(
            Session::parse("# chosen by the owner\n\n session = desktop # boot\n"),
            Some(Session::Desktop)
        );
        for bad in [
            "",
            "session = kiosk",
            "session desktop",
            "mode = desktop",
            "session = desktop\nsession = shell",
            "session = desktop\nextra",
        ] {
            assert_eq!(Session::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_desktop_profile_is_followed_only_on_a_sound_line() {
        assert!(follows_desktop("robot\ndesktop launcher terminal\n"));
        assert!(!follows_desktop("robot\n"));
        assert!(!follows_desktop("desktop bad/app\n"));
        assert!(!follows_desktop(""));
    }
}
