// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Reading optional files and listing directories without mistaking a
//! failure for absence (incident 0008).
//!
//! The installer used to find a file's size in the directory listing and
//! answer "no such file" both when the file was absent and when the listing
//! itself failed (a `ReplyLost` or a timeout from the file service). The
//! exchange file `updater/state` then read as empty, and the next atomic
//! replace faithfully stored that emptiness: the plan and the staged list
//! were erased and no fault was raised.
//!
//! Here the three outcomes are kept apart by the types: `Ok(None)` is only
//! ever produced when the file service explicitly answered *not found*;
//! every other failure is an [`ReadError`]. A caller that does a
//! read-modify-write starts from an empty text on `Ok(None)` and refuses
//! the write on `Err`. The I/O goes through [`Files`], which the programs
//! implement over their file client and the tests over a mock, so the
//! decision is checked on the host.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Debug;

/// Longest file name AriFS stores (bytes).
pub const NAME_LEN: usize = 32;

/// The file operations the decisions below need. `open` must return
/// `Ok(None)` only when the service answered that the file does not exist;
/// a lost reply, a timeout or any other error status is `Err`.
pub trait Files {
    type Fd;
    type Error: Debug;

    fn open(&mut self, name: &str) -> Result<Option<Self::Fd>, Self::Error>;
    /// Bytes stored at `offset` into `out`; `Ok(0)` at the end of the file.
    fn read(&mut self, fd: &Self::Fd, offset: u32, out: &mut [u8]) -> Result<usize, Self::Error>;
    fn close(&mut self, fd: Self::Fd);
    /// Directory entry `index` as (name length, size), the name written to
    /// `name_out`; `Ok(None)` past the last entry.
    fn list(
        &mut self,
        index: u32,
        name_out: &mut [u8; NAME_LEN],
    ) -> Result<Option<(usize, u64)>, Self::Error>;
}

/// Why a file could not be read. Neither variant says anything about
/// whether the file exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError<E> {
    /// A request failed: the service did not answer or answered with an
    /// error other than *not found*.
    Failed(E),
    /// The file is longer than the caller's limit.
    TooLarge,
}

/// The whole content of `name`, at most `max` bytes. `Ok(None)` only when
/// the file definitely does not exist.
pub fn read_optional<F: Files>(
    fs: &mut F,
    name: &str,
    max: usize,
) -> Result<Option<Vec<u8>>, ReadError<F::Error>> {
    let Some(fd) = fs.open(name).map_err(ReadError::Failed)? else {
        return Ok(None);
    };
    let mut data = Vec::new();
    let mut chunk = [0u8; 1024];
    let result = loop {
        match fs.read(&fd, data.len() as u32, &mut chunk) {
            Ok(0) => break Ok(Some(data)),
            Ok(n) if data.len() + n <= max => data.extend_from_slice(&chunk[..n]),
            Ok(_) => break Err(ReadError::TooLarge),
            Err(e) => break Err(ReadError::Failed(e)),
        }
    };
    fs.close(fd);
    result
}

/// [`read_optional`] as text (invalid UTF-8 is replaced, never refused:
/// the parsers of [`crate::lists`] reject what they do not understand).
pub fn read_text<F: Files>(
    fs: &mut F,
    name: &str,
    max: usize,
) -> Result<Option<String>, ReadError<F::Error>> {
    read_optional(fs, name, max).map(|d| d.map(|d| String::from_utf8_lossy(&d).into_owned()))
}

/// The names in the directory for which `keep` holds, in directory order.
/// A listing that fails part-way is an error, never a shorter list: a
/// caller that concluded "not installed" or "nothing pending" from it
/// would act on a directory it never saw.
pub fn list_names<F: Files>(
    fs: &mut F,
    mut keep: impl FnMut(&str) -> bool,
) -> Result<Vec<String>, F::Error> {
    let mut names = Vec::new();
    let mut entry = [0u8; NAME_LEN];
    let mut i = 0;
    while let Some((n, _)) = fs.list(i, &mut entry)? {
        i += 1;
        if let Ok(name) = core::str::from_utf8(&entry[..n]) {
            if keep(name) {
                names.push(String::from(name));
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lists;
    use alloc::vec;

    /// A directory whose requests fail on demand: `fail_open` and
    /// `fail_list` fail every such request; `fail_read_at` fails the read
    /// that starts at that offset. Every opened descriptor must be closed.
    /// Shared with the other modules' tests.
    #[derive(Default)]
    pub(crate) struct Mock {
        pub(crate) files: Vec<(&'static str, Vec<u8>)>,
        pub(crate) fail_open: bool,
        pub(crate) fail_read_at: Option<u32>,
        pub(crate) fail_list: bool,
        pub(crate) open_fds: usize,
    }

    impl Mock {
        pub(crate) fn with(name: &'static str, data: &[u8]) -> Self {
            Self {
                files: vec![(name, data.to_vec())],
                ..Self::default()
            }
        }
    }

    impl Files for Mock {
        type Fd = usize;
        type Error = &'static str;

        fn open(&mut self, name: &str) -> Result<Option<usize>, &'static str> {
            if self.fail_open {
                return Err("ReplyLost");
            }
            let i = self.files.iter().position(|(n, _)| *n == name);
            if i.is_some() {
                self.open_fds += 1;
            }
            Ok(i)
        }

        fn read(&mut self, fd: &usize, offset: u32, out: &mut [u8]) -> Result<usize, &'static str> {
            if self.fail_read_at == Some(offset) {
                return Err("Timeout");
            }
            let rest = &self.files[*fd].1[offset as usize..];
            let n = rest.len().min(out.len());
            out[..n].copy_from_slice(&rest[..n]);
            Ok(n)
        }

        fn close(&mut self, _fd: usize) {
            self.open_fds -= 1;
        }

        fn list(
            &mut self,
            index: u32,
            name_out: &mut [u8; NAME_LEN],
        ) -> Result<Option<(usize, u64)>, &'static str> {
            if self.fail_list && index == 1 {
                return Err("ReplyLost");
            }
            let Some((name, data)) = self.files.get(index as usize) else {
                return Ok(None);
            };
            name_out[..name.len()].copy_from_slice(name.as_bytes());
            Ok(Some((name.len(), data.len() as u64)))
        }
    }

    const STATE: &str = "[installed]\nhello_ext 1\n[plan]\napp hello_ext 2\n[staged]\n";

    #[test]
    fn absent_file_is_none_and_nothing_else_is() {
        let mut fs = Mock::with("other", b"x");
        assert_eq!(read_optional(&mut fs, "updater/state", 16), Ok(None));
        assert_eq!(read_text(&mut fs, "updater/state", 16), Ok(None));
        assert_eq!(fs.open_fds, 0);
    }

    #[test]
    fn present_file_is_read_whole_across_chunks() {
        let big: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let mut fs = Mock::with("updater/state", &big);
        assert_eq!(read_optional(&mut fs, "updater/state", 4096), Ok(Some(big)));
        assert_eq!(fs.open_fds, 0);
    }

    /// Incident 0008: the service did not answer. Nothing is known about
    /// the file, so the answer is an error, not "absent".
    #[test]
    fn failed_open_is_an_error_not_absence() {
        let mut fs = Mock::with("updater/state", STATE.as_bytes());
        fs.fail_open = true;
        assert_eq!(
            read_text(&mut fs, "updater/state", 16 * 1024),
            Err(ReadError::Failed("ReplyLost"))
        );
    }

    #[test]
    fn failed_read_is_an_error_and_closes_the_file() {
        let big: Vec<u8> = vec![7u8; 2500];
        let mut fs = Mock::with("updater/state", &big);
        fs.fail_read_at = Some(2048);
        assert_eq!(
            read_optional(&mut fs, "updater/state", 4096),
            Err(ReadError::Failed("Timeout"))
        );
        assert_eq!(fs.open_fds, 0);
    }

    #[test]
    fn oversized_file_is_refused_and_closed() {
        let mut fs = Mock::with("updater/state", &[0u8; 1025]);
        assert_eq!(
            read_optional(&mut fs, "updater/state", 1024),
            Err(ReadError::TooLarge)
        );
        assert_eq!(fs.open_fds, 0);
        // Exactly the limit is fine.
        assert_eq!(
            read_optional(&mut fs, "updater/state", 1025).map(|d| d.map(|d| d.len())),
            Ok(Some(1025))
        );
    }

    /// The read-modify-write the installer does on `updater/state`: with
    /// a successful read the other sections survive; a missing file starts
    /// from an empty text; a failed read gives nothing to start from, so
    /// the caller must not write.
    #[test]
    fn state_rewrite_keeps_unrelated_sections_or_is_refused() {
        let mut fs = Mock::with("updater/state", STATE.as_bytes());
        let start = read_text(&mut fs, "updater/state", 16 * 1024)
            .expect("readable")
            .unwrap_or_default();
        let written = lists::with_section(&start, lists::INSTALLED, "hello_ext 1\n");
        assert_eq!(
            lists::section(&written, lists::PLAN).as_deref(),
            Some("app hello_ext 2\n")
        );
        assert_eq!(lists::section(&written, lists::STAGED).as_deref(), Some(""));

        let mut empty = Mock::default();
        let start = read_text(&mut empty, "updater/state", 16 * 1024)
            .expect("readable")
            .unwrap_or_default();
        assert_eq!(start, "");

        fs.fail_open = true;
        assert!(read_text(&mut fs, "updater/state", 16 * 1024).is_err());
    }

    #[test]
    fn listing_filters_in_directory_order() {
        let mut fs = Mock {
            files: vec![
                ("apps/a.aripkg", vec![]),
                ("pkgs/b.aripkg", vec![]),
                ("apps/c.aripkg", vec![]),
            ],
            ..Mock::default()
        };
        let apps = list_names(&mut fs, |n| n.starts_with("apps/")).expect("listed");
        assert_eq!(apps, vec!["apps/a.aripkg", "apps/c.aripkg"]);
        assert_eq!(
            list_names(&mut Mock::default(), |_| true).expect("listed"),
            Vec::<String>::new()
        );
    }

    /// A listing that fails after the first entry is not a directory with
    /// one file in it.
    #[test]
    fn failed_listing_is_an_error_not_a_shorter_list() {
        let mut fs = Mock {
            files: vec![("apps/a.aripkg", vec![]), ("apps/c.aripkg", vec![])],
            fail_list: true,
            ..Mock::default()
        };
        assert_eq!(list_names(&mut fs, |_| true), Err("ReplyLost"));
    }
}
