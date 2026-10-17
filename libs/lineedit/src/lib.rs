// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Line editor for text consoles (the shell's `read_line`). It understands
//! the keys a terminal sends (printable ASCII, backspace `0x08`/`0x7F`,
//! Enter, Ctrl-A/E/U, and the escape sequences of the arrows, Home, End and
//! Delete) and redraws using only plain text and backspace, which both the
//! framebuffer console and a serial terminal understand. History is
//! provided by the caller through [`History`].

#![no_std]

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Esc {
    None,
    /// After `ESC`.
    Start,
    /// After `ESC [` (or `ESC O`), collecting a numeric parameter.
    Csi(u8),
}

pub struct Editor<const N: usize> {
    buf: [u8; N],
    len: usize,
    pos: usize,
    esc: Esc,
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
            esc: Esc::None,
            back: 0,
        }
    }

    /// Starts a new empty line.
    pub fn reset(&mut self) {
        self.len = 0;
        self.pos = 0;
        self.esc = Esc::None;
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
        match self.esc {
            Esc::Start => {
                self.esc = if b == b'[' || b == b'O' {
                    Esc::Csi(0)
                } else {
                    Esc::None
                };
                return Event::Pending;
            }
            Esc::Csi(n) => {
                match b {
                    b'0'..=b'9' => {
                        self.esc = Esc::Csi(n.saturating_mul(10).saturating_add(b - b'0'));
                        return Event::Pending;
                    }
                    b';' => return Event::Pending,
                    _ => {}
                }
                self.esc = Esc::None;
                match (b, n) {
                    (b'A', _) => self.history(1, history, out),
                    (b'B', _) => self.history(-1, history, out),
                    (b'C', _) => self.right(out),
                    (b'D', _) => self.left(out),
                    (b'H', _) | (b'~', 1) | (b'~', 7) => self.home(out),
                    (b'F', _) | (b'~', 4) | (b'~', 8) => self.end(out),
                    (b'~', 3) => self.delete(out),
                    _ => {}
                }
                return Event::Pending;
            }
            Esc::None => {}
        }
        match b {
            0x1B => self.esc = Esc::Start,
            b'\r' | b'\n' => {
                self.end(out);
                out(b"\n");
                return Event::Submit;
            }
            0x08 | 0x7F => {
                if self.pos > 0 {
                    self.left(out);
                    self.delete(out);
                }
            }
            0x01 => self.home(out),         // Ctrl-A
            0x05 => self.end(out),          // Ctrl-E
            0x15 => self.replace(b"", out), // Ctrl-U: clear the line
            0x20..=0x7E => self.insert(b, out),
            _ => {}
        }
        Event::Pending
    }

    fn left(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos > 0 {
            self.pos -= 1;
            out(b"\x08");
        }
    }

    fn right(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos < self.len {
            out(&self.buf[self.pos..self.pos + 1]);
            self.pos += 1;
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
        for _ in 0..(self.len - self.pos + extra) {
            out(b"\x08");
        }
    }

    fn insert(&mut self, b: u8, out: &mut dyn FnMut(&[u8])) {
        if self.len == N {
            return;
        }
        self.buf.copy_within(self.pos..self.len, self.pos + 1);
        self.buf[self.pos] = b;
        self.len += 1;
        out(&[b]);
        self.pos += 1;
        self.redraw_tail(0, out);
    }

    /// Deletes the character under the cursor.
    fn delete(&mut self, out: &mut dyn FnMut(&[u8])) {
        if self.pos < self.len {
            self.buf.copy_within(self.pos + 1..self.len, self.pos);
            self.len -= 1;
            self.redraw_tail(1, out);
        }
    }

    /// Replaces the whole line with `text` and leaves the cursor at its end.
    fn replace(&mut self, text: &[u8], out: &mut dyn FnMut(&[u8])) {
        self.home(out);
        let old = self.len;
        let n = text.len().min(N);
        self.buf[..n].copy_from_slice(&text[..n]);
        self.len = n;
        out(&self.buf[..n]);
        for _ in n..old {
            out(b" ");
        }
        for _ in n..old {
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

    /// One-row terminal: text overwrites, backspace moves left.
    #[derive(Default)]
    struct Screen {
        cells: Vec<u8>,
        col: usize,
    }
    impl Screen {
        fn write(&mut self, b: &[u8]) {
            for &c in b {
                match c {
                    0x08 => self.col = self.col.saturating_sub(1),
                    b'\n' => {}
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
            let mut t = self.cells.clone();
            while t.last() == Some(&b' ') {
                t.pop();
            }
            t
        }
    }

    const LEFT: &[u8] = b"\x1b[D";
    const RIGHT: &[u8] = b"\x1b[C";

    fn keys(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
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
}
