// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The owner's source list (`updater/sources`): one normalized URL per
//! line, in preference order.

use crate::{MAX_SOURCES, MAX_SOURCE_LEN};
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// Not `http[s]://host[:port][/path]` or `host[:port][/path]`.
    Invalid,
    /// A scheme other than `http` or `https`.
    UnsupportedScheme,
    /// Longer than `MAX_SOURCE_LEN` once normalized.
    TooLong,
    /// Already registered.
    Duplicate,
    /// `MAX_SOURCES` already registered.
    Full,
    /// `update remove` named no registered source.
    NotFound,
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('.')
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

fn valid_path(path: &str) -> bool {
    path.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~' | b'%' | b'+')
    })
}

/// Normalizes what the owner typed into a source URL: `http://` is assumed
/// when no scheme is given (`10.0.0.5:8080`, `updates.example.org/os`),
/// the port must be 1..=65535 and the URL always ends in `/`. Query strings,
/// fragments, credentials and any other scheme are refused.
pub fn normalize_source(input: &str) -> Result<String, SourceError> {
    let input = input.trim();
    let (scheme, rest) = if let Some(r) = input.strip_prefix("http://") {
        ("http://", r)
    } else if let Some(r) = input.strip_prefix("https://") {
        ("https://", r)
    } else if input.contains("://") {
        return Err(SourceError::UnsupportedScheme);
    } else {
        ("http://", input)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = match authority.split_once(':') {
        Some((host, port)) => {
            // Digits only, no leading zero (one spelling per port), 1..=65535.
            let digits = !port.is_empty()
                && !port.starts_with('0')
                && port.bytes().all(|b| b.is_ascii_digit());
            if !digits || port.parse::<u16>().is_err() {
                return Err(SourceError::Invalid);
            }
            host
        }
        None => authority,
    };
    if !valid_host(host) || !valid_path(path) {
        return Err(SourceError::Invalid);
    }
    let mut url = String::with_capacity(scheme.len() + rest.len() + 1);
    url.push_str(scheme);
    url.push_str(authority);
    url.push_str(if path.is_empty() { "/" } else { path });
    if !url.ends_with('/') {
        url.push('/');
    }
    if url.len() > MAX_SOURCE_LEN {
        return Err(SourceError::TooLong);
    }
    Ok(url)
}

/// The registered sources, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    list: Vec<String>,
}

impl Sources {
    /// Parses the sources file. Lines that do not normalize to themselves
    /// (edited by hand, corrupted) are skipped and counted, so a damaged
    /// file never adds a source the owner did not type.
    pub fn parse(text: &str) -> (Self, usize) {
        let mut sources = Self::default();
        let mut rejected = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match normalize_source(line) {
                Ok(url)
                    if url == line
                        && sources.list.len() < MAX_SOURCES
                        && !sources.list.contains(&url) =>
                {
                    sources.list.push(url)
                }
                _ => rejected += 1,
            }
        }
        (sources, rejected)
    }

    /// The file content.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for url in &self.list {
            out.push_str(url);
            out.push('\n');
        }
        out
    }

    /// The source list after migrating ADR 0027: when there is no sources
    /// file yet, a valid `update_url` becomes the first source. `None`
    /// when nothing has to be migrated.
    pub fn migrate(sources_file: Option<&str>, legacy_url: Option<&str>) -> Option<Self> {
        if sources_file.is_some() {
            return None;
        }
        let url = normalize_source(legacy_url?.trim()).ok()?;
        Some(Self {
            list: alloc::vec![url],
        })
    }

    /// Registers a source; returns its normalized URL.
    pub fn add(&mut self, input: &str) -> Result<&str, SourceError> {
        let url = normalize_source(input)?;
        if self.list.contains(&url) {
            return Err(SourceError::Duplicate);
        }
        if self.list.len() == MAX_SOURCES {
            return Err(SourceError::Full);
        }
        self.list.push(url);
        Ok(self.list.last().map(String::as_str).unwrap_or(""))
    }

    /// Removes a source given by its number in `update sources` (1-based)
    /// or by its URL (any spelling that normalizes to it).
    pub fn remove(&mut self, spec: &str) -> Result<String, SourceError> {
        let spec = spec.trim();
        let position = match spec.parse::<usize>() {
            Ok(n) if (1..=self.list.len()).contains(&n) => Some(n - 1),
            Ok(_) => None,
            Err(_) => normalize_source(spec)
                .ok()
                .and_then(|url| self.list.iter().position(|s| *s == url)),
        };
        position
            .map(|i| self.list.remove(i))
            .ok_or(SourceError::NotFound)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.list.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn contains(&self, url: &str) -> bool {
        self.list.iter().any(|s| s == url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization() {
        for (input, want) in [
            ("10.0.2.2:17010", "http://10.0.2.2:17010/"),
            ("10.0.2.2:17010/update/", "http://10.0.2.2:17010/update/"),
            ("10.0.2.2:17010/update", "http://10.0.2.2:17010/update/"),
            ("  192.168.1.10  ", "http://192.168.1.10/"),
            ("http://host/path", "http://host/path/"),
            (
                "https://updates.example.org/os/",
                "https://updates.example.org/os/",
            ),
            ("updates.example.org", "http://updates.example.org/"),
        ] {
            assert_eq!(normalize_source(input).as_deref(), Ok(want), "{input}");
        }
        for (input, err) in [
            ("", SourceError::Invalid),
            ("ftp://host/", SourceError::UnsupportedScheme),
            ("file:///etc", SourceError::UnsupportedScheme),
            ("http://", SourceError::Invalid),
            ("host:0", SourceError::Invalid),
            ("host:65536", SourceError::Invalid),
            ("host:080", SourceError::Invalid),
            ("host:", SourceError::Invalid),
            ("host:x", SourceError::Invalid),
            ("user@host", SourceError::Invalid),
            ("host/path?q=1", SourceError::Invalid),
            ("host/a b", SourceError::Invalid),
            ("-host", SourceError::Invalid),
            ("http://host/#frag", SourceError::Invalid),
        ] {
            assert_eq!(normalize_source(input), Err(err), "{input}");
        }
        let long = alloc::format!("host/{}", "a".repeat(MAX_SOURCE_LEN));
        assert_eq!(normalize_source(&long), Err(SourceError::TooLong));
    }

    #[test]
    fn add_remove_list() {
        let mut s = Sources::default();
        assert_eq!(
            s.add("10.0.2.2:17010/update/"),
            Ok("http://10.0.2.2:17010/update/")
        );
        assert_eq!(
            s.add("http://10.0.2.2:17010/update"),
            Err(SourceError::Duplicate),
            "another spelling of the same source"
        );
        assert_eq!(s.add("ftp://x/"), Err(SourceError::UnsupportedScheme));
        for i in 2..=MAX_SOURCES {
            s.add(&alloc::format!("host{i}")).unwrap();
        }
        assert_eq!(s.add("one-too-many"), Err(SourceError::Full));
        assert_eq!(s.len(), MAX_SOURCES);
        assert!(s.contains("http://host2/"));
        assert_eq!(s.remove("2").as_deref(), Ok("http://host2/"));
        assert_eq!(
            s.remove("http://10.0.2.2:17010/update/").as_deref(),
            Ok("http://10.0.2.2:17010/update/")
        );
        assert_eq!(
            s.remove("10.0.2.2:17010/update"),
            Err(SourceError::NotFound)
        );
        assert_eq!(s.remove("0"), Err(SourceError::NotFound));
        assert_eq!(s.remove("9"), Err(SourceError::NotFound));
        let urls: Vec<&str> = s.iter().collect();
        assert_eq!(urls, ["http://host3/", "http://host4/"]);
    }

    #[test]
    fn file_round_trip_skips_damaged_lines() {
        let mut s = Sources::default();
        s.add("a.example").unwrap();
        s.add("https://b.example/x/").unwrap();
        let text = s.to_text();
        assert_eq!(Sources::parse(&text), (s.clone(), 0));
        let damaged = alloc::format!(
            "# comment\n{text}ftp://evil/\nhttp://not-normalized\nhttp://a.example/\n"
        );
        // Bad scheme, a spelling that is not the normalized form and a
        // duplicate: three rejected lines, the good ones kept.
        assert_eq!(Sources::parse(&damaged), (s, 3));
    }

    #[test]
    fn migration_from_single_channel_url() {
        let migrated = Sources::migrate(None, Some("http://10.0.2.2:17010/update/\n")).unwrap();
        assert_eq!(
            migrated.iter().collect::<Vec<_>>(),
            ["http://10.0.2.2:17010/update/"]
        );
        // A sources file exists (even an empty one: the owner removed them all).
        assert!(Sources::migrate(Some(""), Some("http://x/")).is_none());
        assert!(Sources::migrate(None, None).is_none());
        assert!(Sources::migrate(None, Some("ftp://x/")).is_none());
    }
}
