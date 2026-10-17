// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The install history (`updates.log`): one line per successful install,
//! the oldest lines dropped to stay within a size. Failures go to the
//! kernel event ring instead (ADR 0028).

use crate::files::{self, Files, ReadError};
use alloc::vec::Vec;

/// The log is kept within its limit by every append, so a stored file
/// larger than this many times the limit was not written by these
/// programs: it is refused rather than trimmed blindly.
const READ_FACTOR: usize = 4;

/// `existing` with `line` appended (a newline added) and whole lines
/// dropped from the front until it fits in `max` bytes.
pub fn append(existing: &[u8], line: &str, max: usize) -> Vec<u8> {
    let line = &line.as_bytes()[..line.len().min(max.saturating_sub(1))];
    let mut out = Vec::with_capacity(existing.len() + line.len() + 1);
    out.extend_from_slice(existing);
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(line);
    out.push(b'\n');
    while out.len() > max {
        match out.iter().position(|&b| b == b'\n') {
            Some(i) => {
                out.drain(..=i);
            }
            None => out.clear(),
        }
    }
    out
}

/// The log to store after appending `line` to the history in file `name`:
/// the existing lines (none when the file does not exist) plus the new
/// one, trimmed to `max` by [`append`]. `Err` when the existing history
/// could not be read: the caller then stores nothing, so a failed request
/// never replaces the whole history by one line (incident 0008).
pub fn appended<F: Files>(
    fs: &mut F,
    name: &str,
    line: &str,
    max: usize,
) -> Result<Vec<u8>, ReadError<F::Error>> {
    let existing = files::read_optional(fs, name, max.saturating_mul(READ_FACTOR))?;
    Ok(append(&existing.unwrap_or_default(), line, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    #[test]
    fn appends_and_drops_oldest_lines() {
        let log = append(b"", "one", 64);
        assert_eq!(log, b"one\n");
        let log = append(&log, "two", 64);
        assert_eq!(log, b"one\ntwo\n");
        // 8 bytes allowed: "one\n" goes, "two\nsix\n" stays.
        assert_eq!(append(&log, "six", 8), b"two\nsix\n");
        // A missing final newline is repaired.
        assert_eq!(append(b"cut", "next", 64), b"cut\nnext\n");
        // A line longer than the whole log is cut to fit.
        assert_eq!(append(b"old\n", "abcdefgh", 5), b"abcd\n");
    }

    /// A history whose requests fail on demand.
    struct Log {
        stored: Option<Vec<u8>>,
        fail: bool,
    }

    impl Files for Log {
        type Fd = ();
        type Error = &'static str;

        fn open(&mut self, _name: &str) -> Result<Option<()>, &'static str> {
            if self.fail {
                return Err("ReplyLost");
            }
            Ok(self.stored.as_ref().map(|_| ()))
        }

        fn read(&mut self, _fd: &(), offset: u32, out: &mut [u8]) -> Result<usize, &'static str> {
            let rest = &self.stored.as_deref().unwrap_or_default()[offset as usize..];
            let n = rest.len().min(out.len());
            out[..n].copy_from_slice(&rest[..n]);
            Ok(n)
        }

        fn close(&mut self, _fd: ()) {}

        fn list(
            &mut self,
            _index: u32,
            _name_out: &mut [u8; files::NAME_LEN],
        ) -> Result<Option<(usize, u64)>, &'static str> {
            Ok(None)
        }
    }

    /// Incident 0008: a missing history starts with the new line; a failed
    /// read gives nothing to store, so the old lines survive.
    #[test]
    fn appended_starts_empty_when_absent_and_refuses_a_failed_read() {
        let mut absent = Log {
            stored: None,
            fail: false,
        };
        assert_eq!(
            appended(&mut absent, "history.log", "one", 64),
            Ok(b"one\n".to_vec())
        );

        let mut present = Log {
            stored: Some(b"one\n".to_vec()),
            fail: false,
        };
        assert_eq!(
            appended(&mut present, "history.log", "two", 64),
            Ok(b"one\ntwo\n".to_vec())
        );

        present.fail = true;
        assert_eq!(
            appended(&mut present, "history.log", "three", 64),
            Err(ReadError::Failed("ReplyLost"))
        );
    }

    /// A stored log somewhat over the limit is trimmed by the append; one
    /// far over it was not written by these programs and is refused.
    #[test]
    fn appended_trims_an_overgrown_log_but_refuses_an_absurd_one() {
        let lines: String = (0..40).map(|i| alloc::format!("line {i}\n")).collect();
        let mut grown = Log {
            stored: Some(lines.as_bytes().to_vec()),
            fail: false,
        };
        // 40 lines of 7-8 bytes (about 300 bytes) against a 100-byte limit.
        let log = appended(&mut grown, "history.log", "new", 100).expect("trimmed");
        assert!(log.len() <= 100);
        assert!(log.ends_with(b"new\n"));

        let mut absurd = Log {
            stored: Some(vec![b'x'; 100 * READ_FACTOR + 1]),
            fail: false,
        };
        assert_eq!(
            appended(&mut absurd, "history.log", "new", 100),
            Err(ReadError::TooLarge)
        );
    }
}
