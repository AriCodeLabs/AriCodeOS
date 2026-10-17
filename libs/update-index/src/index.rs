// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The signed index: format, parser, canonical writer and verification.

use crate::{FORMAT, MAX_APPS, MAX_INDEX};
use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::{ed25519, unhex};
use core::fmt::Write;

/// Longest file name in an index entry.
pub const MAX_FILE: usize = 64;
/// Longest app name (the package name rule of `aricode-pkg`).
pub const MAX_APP_NAME: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexError {
    /// Larger than `MAX_INDEX`.
    TooLarge,
    /// `index.sig` is not 64 bytes of hex.
    BadSignatureEncoding,
    /// The signature does not verify with the project key.
    BadSignature,
    /// Signed, but not valid UTF-8 text.
    NotText,
    /// Malformed line (1-based number).
    Line(usize),
    /// `format` missing or not [`FORMAT`].
    UnsupportedFormat,
    /// `serial` missing.
    MissingSerial,
    /// A second `system`, `format` or `serial`, or an app listed twice.
    Duplicate(usize),
    /// More than `MAX_APPS` apps.
    TooManyApps,
    /// Older than the index already stored for this source.
    Stale { stored: u64, offered: u64 },
}

/// The system image a source offers (slot layout: header + container).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemEntry<'a> {
    pub version: u32,
    pub file: &'a str,
    pub size: u32,
    pub sha256: [u8; 32],
}

/// An app package a source offers (`aricode-pkg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppEntry<'a> {
    pub name: &'a str,
    pub version: u32,
    pub file: &'a str,
    pub size: u32,
    pub sha256: [u8; 32],
}

/// A parsed index. Only [`verify`] should produce one from downloaded bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index<'a> {
    pub serial: u64,
    pub system: Option<SystemEntry<'a>>,
    pub apps: Vec<AppEntry<'a>>,
}

/// A plain file name inside the source directory: no `/`, no leading `.`,
/// only `[A-Za-z0-9._-]`, so it can never leave the source's directory.
pub fn valid_file(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The package name rule (`aricode-pkg`): `[A-Za-z0-9_-]`, 1 to 32 bytes.
pub fn valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_APP_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// `version file size sha256` after the key (and the app name).
fn entry_fields<'a>(
    fields: &mut core::str::SplitWhitespace<'a>,
) -> Option<(u32, &'a str, u32, [u8; 32])> {
    let version = fields.next()?.parse().ok()?;
    let file = fields.next().filter(|f| valid_file(f))?;
    let size: u32 = fields.next()?.parse().ok().filter(|&s| s > 0)?;
    let mut sha = [0u8; 32];
    unhex(fields.next()?, &mut sha)?;
    if fields.next().is_some() {
        return None;
    }
    Some((version, file, size, sha))
}

impl<'a> Index<'a> {
    /// Parses index text. Strict: unknown keys, extra fields and repeated
    /// keys are errors, so two parsers can never disagree on what was
    /// signed.
    pub fn parse(text: &'a str) -> Result<Self, IndexError> {
        if text.len() > MAX_INDEX {
            return Err(IndexError::TooLarge);
        }
        let (mut format, mut serial, mut system) = (None, None, None);
        let mut apps: Vec<AppEntry<'a>> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(IndexError::Line(n))?;
            let value = value.trim();
            match key.trim() {
                "format" => {
                    if format.is_some() {
                        return Err(IndexError::Duplicate(n));
                    }
                    format = Some(value.parse::<u32>().map_err(|_| IndexError::Line(n))?);
                }
                "serial" => {
                    if serial.is_some() {
                        return Err(IndexError::Duplicate(n));
                    }
                    serial = Some(value.parse::<u64>().map_err(|_| IndexError::Line(n))?);
                }
                "system" => {
                    if system.is_some() {
                        return Err(IndexError::Duplicate(n));
                    }
                    let (version, file, size, sha256) =
                        entry_fields(&mut value.split_whitespace()).ok_or(IndexError::Line(n))?;
                    system = Some(SystemEntry {
                        version,
                        file,
                        size,
                        sha256,
                    });
                }
                "app" => {
                    let mut fields = value.split_whitespace();
                    let name = fields
                        .next()
                        .filter(|f| valid_app_name(f))
                        .ok_or(IndexError::Line(n))?;
                    let (version, file, size, sha256) =
                        entry_fields(&mut fields).ok_or(IndexError::Line(n))?;
                    if apps.iter().any(|a| a.name == name) {
                        return Err(IndexError::Duplicate(n));
                    }
                    if apps.len() == MAX_APPS {
                        return Err(IndexError::TooManyApps);
                    }
                    apps.push(AppEntry {
                        name,
                        version,
                        file,
                        size,
                        sha256,
                    });
                }
                _ => return Err(IndexError::Line(n)),
            }
        }
        if format != Some(FORMAT) {
            return Err(IndexError::UnsupportedFormat);
        }
        Ok(Self {
            serial: serial.ok_or(IndexError::MissingSerial)?,
            system,
            apps,
        })
    }

    /// Canonical text (what xtask signs and publishes).
    pub fn write(&self) -> String {
        let mut out = String::new();
        let mut hex = [0u8; 64];
        let _ = writeln!(out, "# AriCodeOS update index (ADR 0029)");
        let _ = writeln!(out, "format = {FORMAT}");
        let _ = writeln!(out, "serial = {}", self.serial);
        if let Some(s) = &self.system {
            let _ = writeln!(
                out,
                "system = {} {} {} {}",
                s.version,
                s.file,
                s.size,
                aricode_crypto::hex(&s.sha256, &mut hex)
            );
        }
        for a in &self.apps {
            let _ = writeln!(
                out,
                "app = {} {} {} {} {}",
                a.name,
                a.version,
                a.file,
                a.size,
                aricode_crypto::hex(&a.sha256, &mut hex)
            );
        }
        out
    }

    /// The app entry with that name.
    pub fn app(&self, name: &str) -> Option<&AppEntry<'a>> {
        self.apps.iter().find(|a| a.name == name)
    }
}

/// Decodes `index.sig`: 128 hex digits, surrounding whitespace allowed
/// (the format `cargo xtask sign` writes).
pub fn decode_signature(text: &[u8]) -> Result<[u8; 64], IndexError> {
    let text = core::str::from_utf8(text).map_err(|_| IndexError::BadSignatureEncoding)?;
    let mut sig = [0u8; 64];
    unhex(text.trim(), &mut sig).ok_or(IndexError::BadSignatureEncoding)?;
    Ok(sig)
}

/// Verifies `index` against `signature` (the `index.sig` file) with the
/// project key and only then parses it (ADR 0029, decision 3: nothing in an
/// index is read before its signature is checked).
pub fn verify<'a>(
    index: &'a [u8],
    signature: &[u8],
    pubkey: &[u8; 32],
) -> Result<Index<'a>, IndexError> {
    if index.len() > MAX_INDEX {
        return Err(IndexError::TooLarge);
    }
    let sig = decode_signature(signature)?;
    if !ed25519::verify(pubkey, index, &sig) {
        return Err(IndexError::BadSignature);
    }
    let text = core::str::from_utf8(index).map_err(|_| IndexError::NotText)?;
    Index::parse(text)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::vec;

    const SEED: [u8; 32] = [3u8; 32];

    fn sample() -> Index<'static> {
        Index {
            serial: 1_760_659_200,
            system: Some(SystemEntry {
                version: 3,
                file: "aricodeos-3.img",
                size: 2_412_544,
                sha256: [0xAB; 32],
            }),
            apps: vec![AppEntry {
                name: "hello_ext",
                version: 2,
                file: "hello_ext-2.aripkg",
                size: 10_240,
                sha256: [0x11; 32],
            }],
        }
    }

    fn signed(text: &str) -> std::string::String {
        let sig = ed25519::sign(&SEED, text.as_bytes());
        let mut hex = [0u8; 128];
        std::format!("{}\n", aricode_crypto::hex(&sig, &mut hex))
    }

    #[test]
    fn write_parse_round_trip() {
        let index = sample();
        let text = index.write();
        assert!(text.starts_with("# AriCodeOS update index"));
        assert_eq!(Index::parse(&text), Ok(index));
        let empty = Index {
            serial: 7,
            system: None,
            apps: Vec::new(),
        };
        assert_eq!(Index::parse(&empty.write()), Ok(empty));
    }

    #[test]
    fn verify_accepts_signed_index() {
        let text = sample().write();
        let pk = ed25519::public_key(&SEED);
        let index = verify(text.as_bytes(), signed(&text).as_bytes(), &pk).unwrap();
        assert_eq!(index.system.unwrap().version, 3);
        assert_eq!(index.app("hello_ext").unwrap().version, 2);
        assert!(index.app("other").is_none());
    }

    #[test]
    fn verify_refuses_tampering_and_other_keys() {
        let text = sample().write();
        let sig = signed(&text);
        let pk = ed25519::public_key(&SEED);
        // One byte of the index changed after signing (the version).
        let tampered = text.replace("system = 3", "system = 4");
        assert_eq!(
            verify(tampered.as_bytes(), sig.as_bytes(), &pk),
            Err(IndexError::BadSignature)
        );
        // An extra line appended after signing.
        let mut longer = text.clone();
        longer.push_str("app = evil 9 evil.aripkg 1 ");
        longer.push_str(&"00".repeat(32));
        assert_eq!(
            verify(longer.as_bytes(), sig.as_bytes(), &pk),
            Err(IndexError::BadSignature)
        );
        // Signed with another key.
        let other = ed25519::public_key(&[4u8; 32]);
        assert_eq!(
            verify(text.as_bytes(), sig.as_bytes(), &other),
            Err(IndexError::BadSignature)
        );
        // Malformed signature files.
        assert_eq!(
            verify(text.as_bytes(), b"not hex", &pk),
            Err(IndexError::BadSignatureEncoding)
        );
        assert_eq!(
            verify(text.as_bytes(), &sig.as_bytes()[..100], &pk),
            Err(IndexError::BadSignatureEncoding)
        );
        // Oversized index: refused before any signature work.
        let big = vec![b'#'; crate::MAX_INDEX + 1];
        assert_eq!(verify(&big, sig.as_bytes(), &pk), Err(IndexError::TooLarge));
    }

    #[test]
    fn signed_but_malformed_is_still_refused() {
        // A valid signature does not make a bad index acceptable.
        let pk = ed25519::public_key(&SEED);
        let text = "format = 1\nserial = 1\nsurprise = yes\n";
        assert_eq!(
            verify(text.as_bytes(), signed(text).as_bytes(), &pk),
            Err(IndexError::Line(3))
        );
    }

    #[test]
    fn parser_is_strict() {
        let sha = "00".repeat(32);
        let ok = std::format!("format = 1\nserial = 5\nsystem = 2 a.img 10 {sha}\n");
        assert!(Index::parse(&ok).is_ok());
        let cases: [(std::string::String, IndexError); 12] = [
            ("serial = 5\n".into(), IndexError::UnsupportedFormat),
            ("format = 2\nserial = 5\n".into(), IndexError::UnsupportedFormat),
            ("format = 1\n".into(), IndexError::MissingSerial),
            ("format = 1\nformat = 1\nserial = 1\n".into(), IndexError::Duplicate(2)),
            (
                std::format!("{ok}system = 3 b.img 10 {sha}\n"),
                IndexError::Duplicate(4),
            ),
            (
                std::format!("format = 1\nserial = 1\nsystem = 2 ../x 10 {sha}\n"),
                IndexError::Line(3),
            ),
            (
                std::format!("format = 1\nserial = 1\nsystem = 2 .hidden 10 {sha}\n"),
                IndexError::Line(3),
            ),
            (
                std::format!("format = 1\nserial = 1\nsystem = 2 a.img 0 {sha}\n"),
                IndexError::Line(3),
            ),
            (
                "format = 1\nserial = 1\nsystem = 2 a.img 10 abcd\n".into(),
                IndexError::Line(3),
            ),
            (
                std::format!("format = 1\nserial = 1\nsystem = 2 a.img 10 {sha} extra\n"),
                IndexError::Line(3),
            ),
            (
                std::format!(
                    "format = 1\nserial = 1\napp = x 1 x.aripkg 1 {sha}\napp = x 2 y.aripkg 1 {sha}\n"
                ),
                IndexError::Duplicate(4),
            ),
            (
                std::format!("format = 1\nserial = 1\napp = bad/name 1 x.aripkg 1 {sha}\n"),
                IndexError::Line(3),
            ),
        ];
        for (text, err) in cases {
            assert_eq!(Index::parse(&text), Err(err), "{text}");
        }
        let mut many = std::string::String::from("format = 1\nserial = 1\n");
        for i in 0..=crate::MAX_APPS {
            many.push_str(&std::format!("app = a{i} 1 a{i}.aripkg 1 {sha}\n"));
        }
        assert_eq!(Index::parse(&many), Err(IndexError::TooManyApps));
    }

    #[test]
    fn file_and_name_rules() {
        for good in ["aricodeos-3.img", "hello_ext-2.aripkg", "a"] {
            assert!(valid_file(good), "{good}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "a b", ".x", "a\\b", "a?b"] {
            assert!(!valid_file(bad), "{bad}");
        }
        assert!(!valid_file(&"x".repeat(MAX_FILE + 1)));
        assert!(valid_app_name("hello_ext"));
        assert!(!valid_app_name("hello.ext"));
        assert!(!valid_app_name(&"x".repeat(MAX_APP_NAME + 1)));
    }
}
