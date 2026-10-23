// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The owner's source list (`updater/sources`): one normalized URL per
//! line, in preference order. A development source (ADR 0045) is the same
//! line after `dev `:
//!
//! ```text
//! http://192.168.1.10:8080/
//! dev http://192.168.1.10:8081/
//! ```
//!
//! A line without a prefix is a stable source, as every source was before
//! ADR 0045; there is no `stable ` prefix (one spelling per line).

use crate::{MAX_SOURCES, MAX_SOURCE_LEN};
use alloc::string::String;
use alloc::vec::Vec;
use aricode_ab::Channel;

/// Prefix of a development source's line.
const DEV_PREFIX: &str = "dev ";

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
/// scheme and host are lower-cased, the default port (80 for http, 443 for
/// https) is dropped, other ports must be 1..=65535 without leading zeros,
/// and the URL always ends in `/`: one spelling per source. Query strings,
/// fragments, credentials, `..` segments, `//` in the path and any other
/// scheme are refused.
pub fn normalize_source(input: &str) -> Result<String, SourceError> {
    let input = input.trim();
    let lower = input.to_ascii_lowercase();
    let (scheme, rest) = if lower.starts_with("http://") {
        ("http://", &input[7..])
    } else if lower.starts_with("https://") {
        ("https://", &input[8..])
    } else if input.contains("://") {
        return Err(SourceError::UnsupportedScheme);
    } else {
        ("http://", input)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => {
            let digits = !port.is_empty()
                && !port.starts_with('0')
                && port.bytes().all(|b| b.is_ascii_digit());
            if !digits || port.parse::<u16>().is_err() {
                return Err(SourceError::Invalid);
            }
            (host, Some(port))
        }
        None => (authority, None),
    };
    let default_port = if scheme == "http://" { "80" } else { "443" };
    let port = port.filter(|p| *p != default_port);
    if !valid_host(host)
        || !valid_path(path)
        || path.contains("//")
        || path.split('/').any(|seg| seg == ".." || seg == ".")
    {
        return Err(SourceError::Invalid);
    }
    let mut url = String::with_capacity(scheme.len() + rest.len() + 1);
    url.push_str(scheme);
    url.push_str(&host.to_ascii_lowercase());
    if let Some(p) = port {
        url.push(':');
        url.push_str(p);
    }
    url.push_str(if path.is_empty() { "/" } else { path });
    if !url.ends_with('/') {
        url.push('/');
    }
    if url.len() > MAX_SOURCE_LEN {
        return Err(SourceError::TooLong);
    }
    Ok(url)
}

/// Outcome of [`Sources::migrate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migration {
    /// A sources file exists, or there is no `update_url`.
    NotNeeded,
    /// `update_url` became the first source; save it.
    Migrated(Sources),
    /// `update_url` is not a valid source: refused.
    InvalidLegacy(SourceError),
}

/// One registered source: its channel and normalized URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub channel: Channel,
    pub url: String,
}

/// The registered sources, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    list: Vec<Source>,
}

impl Sources {
    /// Parses the sources file. Lines that do not normalize to themselves
    /// (edited by hand, corrupted) are skipped and counted, so a damaged
    /// file never adds a source the owner did not type. A line is `URL`
    /// (stable) or `dev URL` (development channel, ADR 0045).
    pub fn parse(text: &str) -> (Self, usize) {
        let mut sources = Self::default();
        let mut rejected = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (channel, spelled) = match line.strip_prefix(DEV_PREFIX) {
                Some(url) => (Channel::Dev, url),
                None => (Channel::Stable, line),
            };
            match normalize_source(spelled) {
                Ok(url)
                    if url == spelled
                        && sources.list.len() < MAX_SOURCES
                        && !sources.contains(&url) =>
                {
                    sources.list.push(Source { channel, url })
                }
                _ => rejected += 1,
            }
        }
        (sources, rejected)
    }

    /// The file content.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for s in &self.list {
            if s.channel == Channel::Dev {
                out.push_str(DEV_PREFIX);
            }
            out.push_str(&s.url);
            out.push('\n');
        }
        out
    }

    /// Migration of ADR 0027: when there is no sources file yet, a valid
    /// `update_url` becomes the first source. An invalid one is reported
    /// ([`Migration::InvalidLegacy`]), never dropped silently.
    pub fn migrate(sources_file: Option<&str>, legacy_url: Option<&str>) -> Migration {
        let (None, Some(legacy)) = (sources_file, legacy_url) else {
            return Migration::NotNeeded;
        };
        match normalize_source(legacy.trim()) {
            Ok(url) => Migration::Migrated(Self {
                list: alloc::vec![Source {
                    channel: Channel::Stable,
                    url,
                }],
            }),
            Err(e) => Migration::InvalidLegacy(e),
        }
    }

    /// Registers a stable source; returns its normalized URL.
    pub fn add(&mut self, input: &str) -> Result<&str, SourceError> {
        self.add_to(input, Channel::Stable)
    }

    /// Registers a source of `channel`; returns its normalized URL. A URL
    /// belongs to one channel only: registering it again, in either
    /// channel, is a duplicate.
    pub fn add_to(&mut self, input: &str, channel: Channel) -> Result<&str, SourceError> {
        let url = normalize_source(input)?;
        if self.contains(&url) {
            return Err(SourceError::Duplicate);
        }
        if self.list.len() == MAX_SOURCES {
            return Err(SourceError::Full);
        }
        self.list.push(Source { channel, url });
        Ok(self.list.last().map(|s| s.url.as_str()).unwrap_or(""))
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
                .and_then(|url| self.list.iter().position(|s| s.url == url)),
        };
        position
            .map(|i| self.list.remove(i).url)
            .ok_or(SourceError::NotFound)
    }

    /// Every source's URL, in order (positions are the numbers `update
    /// sources` shows and the slots of the stored indexes).
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.list.iter().map(|s| s.url.as_str())
    }

    /// Every source with its channel, in order.
    pub fn entries(&self) -> impl Iterator<Item = &Source> {
        self.list.iter()
    }

    /// The channel of the source at `position` (0-based).
    pub fn channel(&self, position: usize) -> Option<Channel> {
        self.list.get(position).map(|s| s.channel)
    }

    /// Whether any source of `channel` is registered.
    pub fn has_channel(&self, channel: Channel) -> bool {
        self.list.iter().any(|s| s.channel == channel)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn contains(&self, url: &str) -> bool {
        self.list.iter().any(|s| s.url == url)
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
            (
                "HTTP://Updates.Example.ORG/OS",
                "http://updates.example.org/OS/",
            ),
            ("10.0.2.2:80/update", "http://10.0.2.2/update/"),
            ("https://h:443/", "https://h/"),
            ("https://h:80/", "https://h:80/"),
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
            ("host/a/../b", SourceError::Invalid),
            ("host/..", SourceError::Invalid),
            ("host/./x", SourceError::Invalid),
            ("host//x", SourceError::Invalid),
            ("host/x//", SourceError::Invalid),
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
    fn development_sources_are_marked_and_kept_apart() {
        // ADR 0045: a development source is opt-in, one line `dev URL`.
        let mut s = Sources::default();
        s.add("10.0.2.2:8080").unwrap();
        assert_eq!(
            s.add_to("10.0.2.2:8081/dev", Channel::Dev),
            Ok("http://10.0.2.2:8081/dev/")
        );
        assert_eq!(
            s.add_to("http://10.0.2.2:8080/", Channel::Dev),
            Err(SourceError::Duplicate),
            "one URL, one channel"
        );
        let text = s.to_text();
        assert_eq!(
            text,
            "http://10.0.2.2:8080/\ndev http://10.0.2.2:8081/dev/\n"
        );
        assert_eq!(Sources::parse(&text), (s.clone(), 0));
        assert_eq!(s.channel(0), Some(Channel::Stable));
        assert_eq!(s.channel(1), Some(Channel::Dev));
        assert_eq!(s.channel(2), None);
        assert!(s.has_channel(Channel::Dev));
        let urls: Vec<&str> = s.iter().collect();
        assert_eq!(urls, ["http://10.0.2.2:8080/", "http://10.0.2.2:8081/dev/"]);
        // Damaged development lines are refused like any other.
        let damaged = "dev\ndev ftp://x/\ndev http://not-normalized\nstable http://a/\ndev  http://a/\nDEV http://a/\n";
        assert_eq!(Sources::parse(damaged), (Sources::default(), 6));
        // Without a development line there is no development source: the
        // channel is closed by default.
        let (stable_only, _) = Sources::parse("http://a/\n");
        assert!(!stable_only.has_channel(Channel::Dev));
        assert_eq!(s.remove("2").as_deref(), Ok("http://10.0.2.2:8081/dev/"));
        assert!(!s.has_channel(Channel::Dev));
    }

    #[test]
    fn migration_from_single_channel_url() {
        let Migration::Migrated(migrated) =
            Sources::migrate(None, Some("http://10.0.2.2:17010/update/\n"))
        else {
            panic!("not migrated");
        };
        assert_eq!(
            migrated.iter().collect::<Vec<_>>(),
            ["http://10.0.2.2:17010/update/"]
        );
        // A sources file exists (even an empty one: the owner removed them all).
        assert_eq!(
            Sources::migrate(Some(""), Some("http://x/")),
            Migration::NotNeeded
        );
        assert_eq!(Sources::migrate(None, None), Migration::NotNeeded);
        assert_eq!(
            Sources::migrate(None, Some("ftp://x/")),
            Migration::InvalidLegacy(SourceError::UnsupportedScheme)
        );
    }
}
