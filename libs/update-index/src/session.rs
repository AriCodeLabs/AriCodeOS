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

#[cfg(test)]
mod tests {
    use super::*;

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
