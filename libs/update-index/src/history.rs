// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The install history (`updates.log`): one line per successful install,
//! the oldest lines dropped to stay within a size. Failures go to the
//! kernel event ring instead (ADR 0028).

use alloc::vec::Vec;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
