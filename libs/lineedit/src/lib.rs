// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Line editor for text consoles (the shell's `read_line`). It understands
//! the keys a terminal sends (characters in UTF-8, ADR 0076; backspace
//! `0x08`/`0x7F`,
//! Enter, Ctrl-A/E/U, and the escape sequences of the arrows, Home, End and
//! Delete) and redraws using only plain text and backspace, which both the
//! framebuffer console and a serial terminal understand: one cell per
//! character, so the cursor moves a character at a time. History is
//! provided by the caller through [`History`].

#![no_std]

use aricode_input::term::{Key, KeyDecoder};

/// Previous lines, most recent first: `back(1)` is the last one entered.
pub trait History {
    fn back(&self, n: usize) -> Option<&[u8]>;
}

/// What a key produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Keep reading.
    Pending,
    /// Enter: the line is complete (`Editor::line`).
    Submit,
}

pub struct Editor<const N: usize> {
    buf: [u8; N],
    len: usize,
    pos: usize,
    keys: KeyDecoder,
    /// Position in the history: 0 = the line being edited, n = `n` back.
    back: usize,
}

impl<const N: usize> Default for Editor<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Editor<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            pos: 0,
            keys: KeyDecoder::new(),
            back: 0,
        }
    }

    /// Starts a new empty line.
    pub fn reset(&mut self) {
        self.len = 0;
        self.pos = 0;
        self.keys = KeyDecoder::new();
        self.back = 0;
    }

    pub fn line(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn cursor(&self) -> usize {
        self.pos
    }

    /// Processes one input byte, writing the screen updates to `out`.
    pub fn feed(&mut self, b: u8, history: &dyn History, out: &mut dyn FnMut(&[u8])) -> Event {
        let mut event = Event::Pending;
        for key in self.keys.feed(b).into_iter().flatten() {
            match key {
                Key::Enter => {
                    self.end(out);
                    out(b"\n");
                    event = Event::Submit;
                }
                Key::Backspace => {
                    if self.pos > 0 {
                        self.left(out);
                        self.delete(out);
                    }
                }
                Key::Up => self.history(1, history, out),
                Key::Down => self.history(-1, history, out),
                Key::Right => self.right(out),
                Key::Left => self.left(out),
                Key::Home | Key::Ctrl(b'a') => self.home(out),
                Key::End | Key::Ctrl(b'e') => self.end(out),
                Key::Delete => self.delete(out),
                Key::Ctrl(b'u') => self.replace(b"", out), // clear the line
                Key::Char(c) => self.insert(c, out),
                Key::Tab | Key::Escape | Key::Ctrl(_) => {}
            }
        }
        event
    }

    /// Where the character before `pos` starts (UTF-8: back over at most
    /// three continuation bytes).
    fn before(&self, pos: usize) -> usize {
        let mut p = pos.saturating_sub(1);
        while p > 0 && pos - p < 4 && self.buf[p] & 0xC0 == 0x80 {
            p -= 1;
        }
        p
    }

    /// Where the character at `pos` ends.
    fn after(&self, pos: usize) -> usize {
        let mut p = (pos + 1).min(self.len);
        while p < self.len && p - pos < 4 && self.buf[p] & 0xC0 == 0x80 {
            p += 1;
        }
        p
    }

    /// Cells the bytes `from..to` take on the screen: one per character.
    fn cells(&self, from: usize, to: usize) -> usize {
        self.buf[from..to]
            .iter()
            .filter(|&&b| b & 0xC0 != 0x80)
            .count()
    }

    fn left(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos > 0 {
            self.pos = self.before(self.pos);
            out(b"\x08");
        }
    }

    fn right(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos < self.len {
            let next = self.after(self.pos);
            out(&self.buf[self.pos..next]);
            self.pos = next;
        }
    }

    fn home(&mut self, out: &mut dyn FnMut(&[u8])) {
        while self.pos > 0 {
            self.left(out);
        }
    }

    fn end(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos < self.len {
            out(&self.buf[self.pos..self.len]);
            self.pos = self.len;
        }
    }

    /// Redraws from the cursor to the end of the line, erasing `extra`
    /// trailing cells, and moves the cursor back to where it was.
    fn redraw_tail(&self, extra: usize, out: &mut dyn FnMut(&[u8])) {
        out(&self.buf[self.pos..self.len]);
        for _ in 0..extra {
            out(b" ");
        }
        for _ in 0..(self.cells(self.pos, self.len) + extra) {
            out(b"\x08");
        }
    }

    /// Inserts `ch` at the cursor; a character that does not fit whole is
    /// refused.
    fn insert(&mut self, ch: char, out: &mut dyn FnMut(&[u8])) {
        let n = ch.len_utf8();
        if self.len + n > N {
            return;
        }
        self.buf.copy_within(self.pos..self.len, self.pos + n);
        ch.encode_utf8(&mut self.buf[self.pos..self.pos + n]);
        self.len += n;
        out(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        self.redraw_tail(0, out);
    }

    /// Deletes the character under the cursor.
    fn delete(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos < self.len {
            let next = self.after(self.pos);
            self.buf.copy_within(next..self.len, self.pos);
            self.len -= next - self.pos;
            self.redraw_tail(1, out);
        }
    }

    /// Replaces the whole line with `text` and leaves the cursor at its end.
    fn replace(&mut self, text: &[u8], out: &mut dyn FnMut(&[u8])) {
        self.home(out);
        let old = self.cells(0, self.len);
        let mut n = text.len().min(N);
        // Not inside a character.
        while n > 0 && n < text.len() && text[n] & 0xC0 == 0x80 {
            n -= 1;
        }
        self.buf[..n].copy_from_slice(&text[..n]);
        self.len = n;
        out(&self.buf[..n]);
        let new = self.cells(0, n);
        for _ in new..old {
            out(b" ");
        }
        for _ in new..old {
            out(b"\x08");
        }
        self.pos = n;
    }

    fn history(&mut self, step: isize, history: &dyn History, out: &mut dyn FnMut(&[u8])) {
        let target = self.back as isize + step;
        if target < 0 {
            return;
        }
        let target = target as usize;
        let text: &[u8] = if target == 0 {
            b""
        } else {
            match history.back(target) {
                Some(t) => t,
                None => return,
            }
        };
        let mut copy = [0u8; N];
        let n = text.len().min(N);
        copy[..n].copy_from_slice(&text[..n]);
        self.back = target;
        self.replace(&copy[..n], out);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    struct Hist(Vec<&'static [u8]>);
    impl History for Hist {
        fn back(&self, n: usize) -> Option<&[u8]> {
            n.checked_sub(1).and_then(|i| self.0.get(i).copied())
        }
    }

    /// Feeds `keys` and returns (line, cursor, what a terminal would show).
    fn run(keys: &[u8], hist: &Hist) -> (Vec<u8>, usize, Vec<u8>) {
        let mut e: Editor<64> = Editor::new();
        let mut screen = Screen::default();
        for &k in keys {
            if e.feed(k, hist, &mut |b| screen.write(b)) == Event::Submit {
                break;
            }
        }
        (e.line().to_vec(), e.cursor(), screen.text())
    }

    /// One-row terminal: text overwrites a cell per character (UTF-8),
    /// backspace moves left.
    #[derive(Default)]
    struct Screen {
        cells: Vec<char>,
        col: usize,
        pending: Vec<u8>,
    }
    impl Screen {
        fn write(&mut self, b: &[u8]) {
            self.pending.extend_from_slice(b);
            let Ok(text) = core::str::from_utf8(&self.pending) else {
                return; // a character cut between two writes
            };
            let text: std::string::String = text.into();
            self.pending.clear();
            for c in text.chars() {
                match c {
                    '\x08' => self.col = self.col.saturating_sub(1),
                    '\n' => {}
                    _ => {
                        if self.col == self.cells.len() {
                            self.cells.push(c);
                        } else {
                            self.cells[self.col] = c;
                        }
                        self.col += 1;
                    }
                }
            }
        }
        fn text(&self) -> Vec<u8> {
            let mut t: std::string::String = self.cells.iter().collect();
            while t.ends_with(' ') {
                t.pop();
            }
            t.into_bytes()
        }
    }

    const LEFT: &[u8] = b"\x1b[D";
    const RIGHT: &[u8] = b"\x1b[C";

    fn keys(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn a_lone_escape_types_nothing_and_the_next_key_counts() {
        let h = Hist(Vec::new());
        // Since the shared decoder (audit of ADR 0074 phase C, L6) the key
        // after a lone ESC is typed, as terminals do for Alt+key.
        let (line, pos, _) = run(b"a\x1bbc\x1b[1;5D\x1b[Zd", &h);
        assert_eq!((line.as_slice(), pos), (&b"abdc"[..], 3));
    }

    #[test]
    fn typing_and_backspace() {
        let h = Hist(Vec::new());
        let (line, pos, screen) = run(b"helxx\x08\x08lo", &h);
        assert_eq!(
            (line.as_slice(), pos, screen.as_slice()),
            (&b"hello"[..], 5, &b"hello"[..])
        );
    }

    #[test]
    fn move_left_and_insert_in_the_middle() {
        let h = Hist(Vec::new());
        // "hllo", left x3, insert 'e' -> "hello" with the cursor after 'e'.
        let k = keys(&[b"hllo", LEFT, LEFT, LEFT, b"e"]);
        let (line, pos, screen) = run(&k, &h);
        assert_eq!(
            (line.as_slice(), pos, screen.as_slice()),
            (&b"hello"[..], 2, &b"hello"[..])
        );
    }

    #[test]
    fn backspace_and_delete_in_the_middle() {
        let h = Hist(Vec::new());
        // "helxlo": left x2, backspace removes 'x'.
        let k = keys(&[b"helxlo", LEFT, LEFT, b"\x08"]);
        let (line, pos, screen) = run(&k, &h);
        assert_eq!(
            (line.as_slice(), pos, screen.as_slice()),
            (&b"hello"[..], 3, &b"hello"[..])
        );
        // "hexllo": home, right x2, delete removes 'x'.
        let k = keys(&[b"hexllo", b"\x1b[H", RIGHT, RIGHT, b"\x1b[3~"]);
        let (line, pos, screen) = run(&k, &h);
        assert_eq!(
            (line.as_slice(), pos, screen.as_slice()),
            (&b"hello"[..], 2, &b"hello"[..])
        );
    }

    #[test]
    fn home_end_and_ctrl_keys() {
        let h = Hist(Vec::new());
        let k = keys(&[b"world", b"\x01", b"hello ", b"\x05", b"!"]);
        let (line, _, screen) = run(&k, &h);
        assert_eq!(
            (line.as_slice(), screen.as_slice()),
            (&b"hello world!"[..], &b"hello world!"[..])
        );
        let k = keys(&[b"abc", b"\x1b[1~", b"x", b"\x1b[4~", b"y"]);
        assert_eq!(run(&k, &h).0, b"xabcy");
        let k = keys(&[b"wrong", b"\x15", b"ok"]);
        let (line, _, screen) = run(&k, &h);
        assert_eq!(
            (line.as_slice(), screen.as_slice()),
            (&b"ok"[..], &b"ok"[..])
        );
    }

    #[test]
    fn history_then_edit() {
        let h = Hist(std::vec![&b"cat notas.txt"[..], &b"ls"[..]]);
        // Up twice -> "ls", down -> "cat notas.txt", edit the end.
        let k = keys(&[b"\x1b[A\x1b[A\x1b[B", b"\x08\x08\x08md"]);
        let (line, _, screen) = run(&k, &h);
        assert_eq!(
            (line.as_slice(), screen.as_slice()),
            (&b"cat notas.md"[..], &b"cat notas.md"[..])
        );
        // Down past the newest returns to an empty line; up past the oldest stays.
        let k = keys(&[b"\x1b[A\x1b[B"]);
        assert_eq!(run(&k, &h).0, b"");
        let k = keys(&[b"\x1b[A\x1b[A\x1b[A"]);
        assert_eq!(run(&k, &h).0, b"ls");
    }

    #[test]
    fn edges_do_nothing() {
        let h = Hist(Vec::new());
        let k = keys(&[LEFT, b"\x08", b"\x1b[3~", b"ab", RIGHT, RIGHT, b"\x1b[3~"]);
        let (line, pos, _) = run(&k, &h);
        assert_eq!((line.as_slice(), pos), (&b"ab"[..], 2));
        // Unknown sequences and control bytes are ignored; Enter submits.
        let mut e: Editor<4> = Editor::new();
        let mut sink = |_: &[u8]| {};
        for &k in b"\x1b[5~\x07abcdef" {
            e.feed(k, &h, &mut sink);
        }
        assert_eq!(e.line(), b"abcd", "full line ignores extra input");
        assert_eq!(e.feed(b'\r', &h, &mut sink), Event::Submit);
    }

    #[test]
    fn characters_of_several_bytes_are_one_cell() {
        let h = Hist(std::vec!["añil".as_bytes()]);
        // ñ and € are typed, moved over and deleted whole.
        let k = keys(&["añb€".as_bytes(), LEFT, LEFT, b"\x7f", RIGHT, b"x"]);
        let (line, pos, screen) = run(&k, &h);
        assert_eq!(line, "abx€".as_bytes());
        assert_eq!(pos, 3);
        assert_eq!(screen, "abx€".as_bytes());
        // Home, Delete at a two-byte character, End.
        let k = keys(&["ñandú".as_bytes(), b"\x1b[H\x1b[3~\x1b[F!"]);
        let (line, pos, screen) = run(&k, &h);
        assert_eq!((line.as_slice(), pos), ("andú!".as_bytes(), 6));
        assert_eq!(screen, "andú!".as_bytes());
        // A line from the history with ñ replaces a longer one cell for cell.
        let k = keys(&["ñandúes".as_bytes(), b"\x1b[A"]);
        let (line, _, screen) = run(&k, &h);
        assert_eq!(line, "añil".as_bytes());
        assert_eq!(screen, "añil".as_bytes());
        // A full line refuses a character that does not fit whole.
        let mut e: Editor<4> = Editor::new();
        let mut sink = |_: &[u8]| {};
        for &k in "abc€".as_bytes() {
            e.feed(k, &h, &mut sink);
        }
        assert_eq!(e.line(), b"abc");
    }
}
