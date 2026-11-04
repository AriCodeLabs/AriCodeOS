// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Widgets the desktop applications share (ADR 0074 phase C): keys decoded
//! from the bytes a window receives, buttons, lists and a one-line text
//! field. They draw on a [`Canvas`] and answer hit tests; the application
//! keeps its own state and repaints. No `alloc`.

use crate::{rgb, Canvas, Color, Rect};
use aricode_fb_console::font::GLYPH_WIDTH;

/// Height of one text line of the console font.
pub const LINE_H: u32 = 16;
/// Height of a list row or a button.
pub const ROW_H: u32 = 24;
/// Inner margin of widgets and windows.
pub const PAD: i32 = 8;

pub const BG: Color = rgb(0xf1, 0xf5, 0xf9);
pub const FG: Color = rgb(0x0f, 0x17, 0x2a);
pub const MUTED: Color = rgb(0x64, 0x74, 0x8b);
pub const ACCENT: Color = rgb(0x3b, 0x82, 0xf6);
pub const ACCENT_FG: Color = rgb(0xff, 0xff, 0xff);
pub const FIELD_BG: Color = rgb(0xff, 0xff, 0xff);
pub const EDGE: Color = rgb(0xcb, 0xd5, 0xe1);
pub const ERROR: Color = rgb(0xb9, 0x1c, 0x1c);

/// Width in pixels of `chars` characters of the console font.
pub const fn text_width(chars: usize) -> u32 {
    (chars * GLYPH_WIDTH) as u32
}

/// Keys back from the bytes a window receives (`Event::Key`): the
/// compositor sends the editing keys as a terminal's escape sequences.
pub use aricode_input::term::{Key, KeyDecoder};

/// A push button.
#[derive(Debug, Clone, Copy)]
pub struct Button<'a> {
    pub rect: Rect,
    pub label: &'a str,
}

impl<'a> Button<'a> {
    pub const fn new(rect: Rect, label: &'a str) -> Self {
        Self { rect, label }
    }

    /// A button `label` wide (plus margins) at `(x, y)`.
    pub const fn at(x: i32, y: i32, label: &'a str) -> Self {
        let w = text_width(label.len()) + 2 * PAD as u32;
        Self::new(Rect::new(x, y, w, ROW_H), label)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.rect.contains(x, y)
    }

    /// Draws it; `primary` in the accent color.
    pub fn draw(&self, c: &mut Canvas, primary: bool) {
        let (bg, fg) = if primary {
            (ACCENT, ACCENT_FG)
        } else {
            (FIELD_BG, FG)
        };
        c.fill_rect(&self.rect, EDGE);
        let inner = inset(&self.rect, 1);
        c.fill_rect(&inner, bg);
        let tx = self.rect.x + (self.rect.w as i32 - text_width(self.label.len()) as i32) / 2;
        let ty = self.rect.y + (self.rect.h as i32 - LINE_H as i32) / 2;
        c.text(tx, ty, self.label, fg, None);
    }
}

/// `r` shrunk by `by` pixels on every side.
pub fn inset(r: &Rect, by: i32) -> Rect {
    let w = (r.w as i32 - 2 * by).max(0) as u32;
    let h = (r.h as i32 - 2 * by).max(0) as u32;
    Rect::new(r.x + by, r.y + by, w, h)
}

/// A vertical list of rows in `rect`, scrolled so the selected row shows.
#[derive(Debug, Clone, Copy)]
pub struct List {
    pub rect: Rect,
    /// Rows in the list.
    pub count: usize,
    pub selected: Option<usize>,
    /// First row shown.
    pub top: usize,
}

impl List {
    pub const fn new(rect: Rect) -> Self {
        Self {
            rect,
            count: 0,
            selected: None,
            top: 0,
        }
    }

    /// Rows that fit.
    pub fn visible(&self) -> usize {
        (self.rect.h / ROW_H) as usize
    }

    /// Sets the number of rows, keeping the selection inside.
    pub fn set_count(&mut self, count: usize) {
        self.count = count;
        self.selected = match self.selected {
            _ if count == 0 => None,
            Some(s) => Some(s.min(count - 1)),
            None => Some(0),
        };
        self.top = self.top.min(count.saturating_sub(1));
        self.follow();
    }

    /// The row at `(x, y)`, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let row = self.top + ((y - self.rect.y) as u32 / ROW_H) as usize;
        (row < self.count).then_some(row)
    }

    /// Moves the selection with the arrows, Home and End; whether the key
    /// was the list's.
    pub fn key(&mut self, key: Key) -> bool {
        if self.count == 0 {
            return false;
        }
        let s = self.selected.unwrap_or(0);
        let to = match key {
            Key::Up => s.saturating_sub(1),
            Key::Down => (s + 1).min(self.count - 1),
            Key::Home => 0,
            Key::End => self.count - 1,
            _ => return false,
        };
        self.select(to);
        true
    }

    pub fn select(&mut self, row: usize) {
        if row < self.count {
            self.selected = Some(row);
            self.follow();
        }
    }

    /// Scrolls so the selected row is shown.
    fn follow(&mut self) {
        let Some(s) = self.selected else { return };
        let visible = self.visible().max(1);
        if s < self.top {
            self.top = s;
        } else if s >= self.top + visible {
            self.top = s + 1 - visible;
        }
    }

    /// Draws the rows shown; `label(i)` is row `i`'s text.
    pub fn draw<'s>(&self, c: &mut Canvas, mut label: impl FnMut(usize) -> &'s str) {
        c.fill_rect(&self.rect, FIELD_BG);
        let mut area = c.clipped(&self.rect);
        for (k, row) in (self.top..self.count).take(self.visible() + 1).enumerate() {
            let y = self.rect.y + (k as u32 * ROW_H) as i32;
            let line = Rect::new(self.rect.x, y, self.rect.w, ROW_H);
            let fg = if self.selected == Some(row) {
                area.fill_rect(&line, ACCENT);
                ACCENT_FG
            } else {
                FG
            };
            let ty = y + (ROW_H - LINE_H) as i32 / 2;
            area.text(self.rect.x + PAD, ty, label(row), fg, None);
        }
    }
}

/// The text of `bytes` up to where they stop being UTF-8: a message cut
/// to a buffer's size shows what fits, not nothing, when the cut falls
/// inside a character (ADR 0076).
pub fn utf8_prefix(bytes: &[u8]) -> &str {
    match core::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => core::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or(""),
    }
}

/// Where the character before `pos` starts: one step back over UTF-8, at
/// most three continuation bytes (a stray one is a character of its own).
fn prev_char(buf: &[u8], pos: usize) -> usize {
    let mut p = pos.saturating_sub(1);
    while p > 0 && pos - p < 4 && buf[p] & 0xC0 == 0x80 {
        p -= 1;
    }
    p
}

/// Where the character after the one at `pos` starts, within `len`.
fn next_char(buf: &[u8], pos: usize, len: usize) -> usize {
    let mut p = (pos + 1).min(len);
    while p < len && p - pos < 4 && buf[p] & 0xC0 == 0x80 {
        p += 1;
    }
    p
}

/// The character that `buf[from..to]` holds, as it is shown: a tab as a
/// space, anything that is not printable UTF-8 as `?` (ADR 0076).
fn shown_char(unit: &[u8]) -> char {
    match core::str::from_utf8(unit)
        .ok()
        .and_then(|s| s.chars().next())
    {
        Some('\t') => ' ',
        Some(c) if !c.is_control() => c,
        _ => '?',
    }
}

/// Characters of `buf` from `from` to `to` (by `next_char`).
fn count_chars(buf: &[u8], from: usize, to: usize) -> usize {
    let (mut p, mut n) = (from, 0);
    while p < to {
        p = next_char(buf, p, to);
        n += 1;
    }
    n
}

/// `n` characters on from `pos`, stopping at `end`.
fn skip_chars(buf: &[u8], mut pos: usize, n: usize, end: usize) -> usize {
    for _ in 0..n {
        if pos >= end {
            break;
        }
        pos = next_char(buf, pos, end);
    }
    pos
}

/// The characters of `buf[from..to]` as shown, at most `cols`, in UTF-8
/// into `out`; returns the text.
fn shown_text<'a>(buf: &[u8], from: usize, to: usize, cols: usize, out: &'a mut [u8]) -> &'a str {
    let (mut p, mut n, mut len) = (from, 0, 0);
    while p < to && n < cols {
        let next = next_char(buf, p, to);
        let ch = shown_char(&buf[p..next]);
        if len + ch.len_utf8() > out.len() {
            break;
        }
        len += ch.encode_utf8(&mut out[len..]).len();
        p = next;
        n += 1;
    }
    core::str::from_utf8(&out[..len]).unwrap_or("")
}

/// A one-line text field of at most `N` bytes of UTF-8, edited with the
/// keys (ADR 0076). The caret and the cuts fall between characters, never
/// inside one (audit of phase C, M3); controls never get in.
#[derive(Debug, Clone, Copy)]
pub struct TextField<const N: usize> {
    buf: [u8; N],
    len: usize,
    caret: usize,
}

impl<const N: usize> Default for TextField<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> TextField<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            caret: 0,
        }
    }

    pub fn text(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// The caret, in bytes of the text.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Replaces the text (cut, between characters, to `N` bytes; a control
    /// becomes `?`) and puts the caret at its end.
    pub fn set(&mut self, text: &str) {
        self.len = 0;
        for ch in text.chars() {
            let ch = if ch.is_control() { '?' } else { ch };
            if self.len + ch.len_utf8() > N {
                break;
            }
            self.len += ch.encode_utf8(&mut self.buf[self.len..]).len();
        }
        self.caret = self.len;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// Edits with `key`; whether it was the field's. A character that does
    /// not fit in `N` bytes is refused (still the field's).
    pub fn key(&mut self, key: Key) -> bool {
        match key {
            Key::Char(ch) => {
                let n = ch.len_utf8();
                if self.len + n <= N {
                    self.buf.copy_within(self.caret..self.len, self.caret + n);
                    ch.encode_utf8(&mut self.buf[self.caret..self.caret + n]);
                    self.len += n;
                    self.caret += n;
                }
            }
            Key::Backspace if self.caret > 0 => {
                let from = prev_char(&self.buf, self.caret);
                self.buf.copy_within(self.caret..self.len, from);
                self.len -= self.caret - from;
                self.caret = from;
            }
            Key::Delete if self.caret < self.len => {
                let to = next_char(&self.buf, self.caret, self.len);
                self.buf.copy_within(to..self.len, self.caret);
                self.len -= to - self.caret;
            }
            Key::Left => self.caret = prev_char(&self.buf, self.caret),
            Key::Right => self.caret = next_char(&self.buf, self.caret, self.len),
            Key::Home => self.caret = 0,
            Key::End => self.caret = self.len,
            Key::Backspace | Key::Delete => {}
            _ => return false,
        }
        true
    }

    /// Draws it in `rect`, with the caret when `focused`; the text scrolls
    /// so the caret shows.
    pub fn draw(&self, c: &mut Canvas, rect: &Rect, focused: bool) {
        c.fill_rect(rect, if focused { ACCENT } else { EDGE });
        let inner = inset(rect, 1);
        c.fill_rect(&inner, FIELD_BG);
        let fits = ((inner.w as i32 - 2 * 4) / GLYPH_WIDTH as i32).max(1) as usize;
        let col = count_chars(&self.buf, 0, self.caret);
        let first = col.saturating_sub(fits - 1);
        let from = skip_chars(&self.buf, 0, first, self.len);
        let mut out = [0u8; 4 * 128];
        let shown = shown_text(&self.buf, from, self.len, fits, &mut out);
        let ty = rect.y + (rect.h as i32 - LINE_H as i32) / 2;
        let mut area = c.clipped(&inner);
        area.text(inner.x + 4, ty, shown, FG, None);
        if focused {
            let cx = inner.x + 4 + ((col - first) * GLYPH_WIDTH) as i32;
            area.fill_rect(&Rect::new(cx, ty, 1, LINE_H), FG);
        }
    }
}

/// A multi-line text of at most `N` bytes, edited with the keys (the
/// desktop's editor). Lines end with `\n`. The text is UTF-8 (ADR 0076);
/// bytes that are not are kept as they are and shown as `?`, one column
/// each. Long lines are not wrapped: the view scrolls sideways to the
/// caret. Columns count characters.
pub struct TextArea<const N: usize> {
    buf: [u8; N],
    len: usize,
    caret: usize,
    /// First line and column shown.
    top: usize,
    left: usize,
    /// Edited since `set` or `mark_saved`.
    changed: bool,
}

impl<const N: usize> Default for TextArea<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> TextArea<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            caret: 0,
            top: 0,
            left: 0,
            changed: false,
        }
    }

    pub fn text(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// The caret, in bytes of the text.
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn mark_saved(&mut self) {
        self.changed = false;
    }

    /// Replaces the text (cut to `N` bytes, before a character that would
    /// not fit whole); the caret goes to the start. Returns whether it all
    /// fitted.
    pub fn set(&mut self, text: &[u8]) -> bool {
        let mut n = text.len().min(N);
        if n < text.len() {
            // Not inside a character.
            while n > 0 && text[n] & 0xC0 == 0x80 && text.len() - n < 4 {
                n -= 1;
            }
        }
        self.buf[..n].copy_from_slice(&text[..n]);
        self.len = n;
        self.caret = 0;
        self.top = 0;
        self.left = 0;
        self.changed = false;
        n == text.len()
    }

    fn line_start(&self, pos: usize) -> usize {
        self.buf[..pos]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.buf[pos..self.len]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(self.len, |i| pos + i)
    }

    /// The caret's line and column (from 0; the column in characters).
    pub fn caret_line_col(&self) -> (usize, usize) {
        let line = self.buf[..self.caret]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        let start = self.line_start(self.caret);
        (line, count_chars(&self.buf, start, self.caret))
    }

    fn insert(&mut self, ch: char) {
        let n = ch.len_utf8();
        if self.len + n <= N {
            self.buf.copy_within(self.caret..self.len, self.caret + n);
            ch.encode_utf8(&mut self.buf[self.caret..self.caret + n]);
            self.len += n;
            self.caret += n;
            self.changed = true;
        }
    }

    /// Removes the character that starts at `at`.
    fn remove(&mut self, at: usize) {
        let to = next_char(&self.buf, at, self.len);
        self.buf.copy_within(to..self.len, at);
        self.len -= to - at;
        self.changed = true;
    }

    /// Moves to the line before (`up`) or after, keeping the column where
    /// the line is long enough.
    fn vertical(&mut self, up: bool) {
        let start = self.line_start(self.caret);
        let col = count_chars(&self.buf, start, self.caret);
        let target = if up {
            if start == 0 {
                return;
            }
            self.line_start(start - 1)
        } else {
            let end = self.line_end(self.caret);
            if end == self.len {
                return;
            }
            end + 1
        };
        self.caret = skip_chars(&self.buf, target, col, self.line_end(target));
    }

    /// Edits or moves with `key`; whether it was the area's. A character
    /// beyond `N` bytes is refused (still the area's).
    pub fn key(&mut self, key: Key) -> bool {
        match key {
            Key::Char(ch) => self.insert(ch),
            Key::Enter => self.insert('\n'),
            Key::Tab => self.insert('\t'),
            Key::Backspace if self.caret > 0 => {
                self.caret = prev_char(&self.buf, self.caret);
                self.remove(self.caret);
            }
            Key::Delete if self.caret < self.len => self.remove(self.caret),
            Key::Backspace | Key::Delete => {}
            Key::Left => self.caret = prev_char(&self.buf, self.caret),
            Key::Right => self.caret = next_char(&self.buf, self.caret, self.len),
            Key::Up => self.vertical(true),
            Key::Down => self.vertical(false),
            Key::Home => self.caret = self.line_start(self.caret),
            Key::End => self.caret = self.line_end(self.caret),
            _ => return false,
        }
        true
    }

    /// Scrolls so the caret is inside a view of `rows` by `cols`.
    fn follow(&mut self, rows: usize, cols: usize) {
        let (line, col) = self.caret_line_col();
        let (rows, cols) = (rows.max(1), cols.max(1));
        if line < self.top {
            self.top = line;
        } else if line >= self.top + rows {
            self.top = line + 1 - rows;
        }
        if col < self.left {
            self.left = col;
        } else if col >= self.left + cols {
            self.left = col + 1 - cols;
        }
    }

    /// Draws the lines shown in `rect`, with the caret when `focused`.
    pub fn draw(&mut self, c: &mut Canvas, rect: &Rect, focused: bool) {
        let rows = (rect.h / LINE_H) as usize;
        let cols = (rect.w as usize).saturating_sub(8) / GLYPH_WIDTH;
        self.follow(rows, cols);
        c.fill_rect(rect, FIELD_BG);
        let mut area = c.clipped(rect);
        let (caret_line, caret_col) = self.caret_line_col();
        let mut start = 0;
        let mut line = 0;
        // The first line shown.
        while line < self.top && start < self.len {
            start = self.line_end(start) + 1;
            line += 1;
        }
        let mut out = [0u8; 4 * 256];
        for row in 0..rows {
            if start > self.len || (start == self.len && line > caret_line) {
                break;
            }
            let end = self.line_end(start);
            let from = skip_chars(&self.buf, start, self.left, end);
            let text = shown_text(&self.buf, from, end, cols.min(256), &mut out);
            let y = rect.y + (row as u32 * LINE_H) as i32;
            area.text(rect.x + 4, y, text, FG, None);
            if focused && line == caret_line {
                let x = rect.x + 4 + ((caret_col - self.left) * GLYPH_WIDTH) as i32;
                area.fill_rect(&Rect::new(x, y, 1, LINE_H), FG);
            }
            start = end + 1;
            line += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    fn keys(bytes: &[u8]) -> std::vec::Vec<Key> {
        let mut d = KeyDecoder::new();
        bytes.iter().flat_map(|&b| d.feed(b)).flatten().collect()
    }

    #[test]
    fn text_area_edits_and_moves() {
        let mut t = TextArea::<64>::new();
        assert!(t.set(b"one\ntwo words\nthree"));
        assert!(!t.changed());
        for k in keys(b"\x1b[B\x1b[F") {
            t.key(k);
        }
        assert_eq!(t.caret_line_col(), (1, 9), "end of line 2");
        t.key(Key::Up);
        assert_eq!(t.caret_line_col(), (0, 3), "clamped to line 1's length");
        t.key(Key::Down);
        t.key(Key::Down);
        assert_eq!(t.caret_line_col(), (2, 3));
        t.key(Key::Down);
        assert_eq!(t.caret_line_col(), (2, 3), "no line after the last");
        for k in keys(b"\x1b[H\x7f!\r") {
            t.key(k);
        }
        assert_eq!(t.text(), b"one\ntwo words!\nthree");
        assert!(t.changed());
        t.key(Key::Delete);
        assert_eq!(
            t.text(),
            b"one\ntwo words!\nhree",
            "the caret starts line 3"
        );
        t.mark_saved();
        assert!(!t.changed());
        assert!(!t.set(&[b'x'; 65]), "cut to 64 bytes");
        assert_eq!(t.text().len(), 64);
        assert!(t.key(Key::Char('y')), "refused but the area's");
        assert_eq!(t.text().len(), 64);
    }

    #[test]
    fn text_area_edits_characters_not_bytes() {
        let mut t = TextArea::<64>::new();
        t.set("año\nüñ".as_bytes());
        t.key(Key::End);
        assert_eq!(t.caret_line_col(), (0, 3), "three characters");
        t.key(Key::Down);
        assert_eq!(t.caret_line_col(), (1, 2), "clamped to two");
        t.key(Key::Backspace);
        assert_eq!(t.text(), "año\nü".as_bytes());
        t.key(Key::Left);
        t.key(Key::Char('é'));
        assert_eq!(t.text(), "año\néü".as_bytes());
        t.key(Key::Up);
        t.key(Key::Delete);
        assert_eq!(t.text(), "ao\néü".as_bytes(), "the ñ, whole");
        // A cut keeps whole characters; bytes that are not UTF-8 stay.
        let mut small = TextArea::<3>::new();
        assert!(!small.set("añb".as_bytes()));
        assert_eq!(small.text(), "añ".as_bytes());
        assert!(!small.set("aañ".as_bytes()));
        assert_eq!(small.text(), b"aa");
        t.set(b"a\xFFb");
        t.key(Key::End);
        assert_eq!(t.caret_line_col(), (0, 3));
        let mut px = vec![0u32; 120 * 40];
        let mut c = Canvas::new(&mut px, 120, 40, 120).unwrap();
        t.draw(&mut c, &Rect::new(0, 0, 120, LINE_H), true);
    }

    #[test]
    fn a_cut_message_keeps_what_is_whole() {
        let msg = "no ñ".as_bytes();
        assert_eq!(utf8_prefix(msg), "no ñ");
        assert_eq!(utf8_prefix(&msg[..4]), "no ", "cut inside ñ");
    }

    #[test]
    fn text_area_scrolls_to_the_caret() {
        let mut px = vec![0u32; 120 * 40];
        let mut c = Canvas::new(&mut px, 120, 40, 120).unwrap();
        let mut t = TextArea::<256>::new();
        t.set(b"a\nb\nc\nd\n0123456789abcdefghijklmnopqrstuvwxyz");
        for _ in 0..4 {
            t.key(Key::Down);
        }
        t.key(Key::End);
        // Two rows of 16 px, (120 - 8) / 8 = 14 columns.
        t.draw(&mut c, &Rect::new(0, 0, 120, 2 * LINE_H), true);
        assert_eq!((t.top, t.left), (3, 36 - 13));
        t.key(Key::Home);
        t.draw(&mut c, &Rect::new(0, 0, 120, 2 * LINE_H), true);
        assert_eq!(t.left, 0);
    }

    #[test]
    fn text_field_edits() {
        let mut f = TextField::<8>::new();
        for k in keys(b"notes\x1b[D\x1b[D\x7fX\x1b[H\x1b[3~") {
            assert!(f.key(k));
        }
        assert_eq!(f.text(), "oXes");
        assert_eq!(f.caret(), 0);
        f.key(Key::End);
        for k in keys(b"1234") {
            f.key(k);
        }
        assert_eq!(f.text(), "oXes1234");
        assert!(f.key(Key::Char('5')), "refused but the field's");
        assert_eq!(f.text(), "oXes1234", "cut at 8 bytes");
        assert!(!f.key(Key::Up));
        f.set("a/b.txt");
        assert_eq!((f.text(), f.caret()), ("a/b.txt", 7));
        // UTF-8, cut between characters, edited a character at a time.
        f.set("año\tx");
        assert_eq!(f.text(), "año?x", "a control becomes ?");
        f.set("ññññé");
        assert_eq!(f.text(), "ññññ", "é would not fit whole in 8 bytes");
        f.key(Key::Char('€'));
        assert_eq!(f.text(), "ññññ", "no room for 3 more bytes");
        f.key(Key::Backspace);
        f.key(Key::Backspace);
        f.key(Key::Left);
        f.key(Key::Char('€'));
        assert_eq!(f.text(), "ñ€ñ");
        f.key(Key::Delete);
        assert_eq!((f.text(), f.caret()), ("ñ€", 5));
        let mut c = [0u32; 64 * 32];
        let mut canvas = Canvas::new(&mut c, 64, 32, 64).unwrap();
        f.draw(&mut canvas, &Rect::new(0, 0, 40, ROW_H), true);
    }

    #[test]
    fn list_selects_scrolls_and_hits() {
        let mut l = List::new(Rect::new(0, 10, 100, 3 * ROW_H));
        l.set_count(10);
        assert_eq!(l.selected, Some(0));
        for _ in 0..4 {
            l.key(Key::Down);
        }
        assert_eq!((l.selected, l.top), (Some(4), 2));
        assert_eq!(l.hit(5, 10 + ROW_H as i32), Some(3));
        assert_eq!(l.hit(5, 5), None);
        l.key(Key::End);
        assert_eq!((l.selected, l.top), (Some(9), 7));
        l.key(Key::Home);
        assert_eq!((l.selected, l.top), (Some(0), 0));
        l.select(6);
        l.set_count(2);
        assert_eq!(l.selected, Some(1));
        assert_eq!(l.hit(5, 10 + 2 * ROW_H as i32), None, "past the last row");
        l.set_count(0);
        assert_eq!(l.selected, None);
        assert!(!l.key(Key::Down));
    }

    #[test]
    fn widgets_draw_inside_their_rects() {
        let mut px = vec![0u32; 200 * 100];
        let mut c = Canvas::new(&mut px, 200, 100, 200).unwrap();
        let b = Button::at(10, 10, "Open");
        b.draw(&mut c, true);
        assert_eq!(b.rect.w, text_width(4) + 2 * PAD as u32);
        assert_eq!(c.get(11, 11), Some(ACCENT));
        assert_eq!(c.get(9, 9), Some(0), "nothing outside");
        let mut f = TextField::<32>::new();
        f.set("a very long name that does not fit");
        f.draw(&mut c, &Rect::new(10, 50, 80, ROW_H), true);
        assert_eq!(c.get(91, 60), Some(0), "the text is clipped to the field");
        let mut l = List::new(Rect::new(100, 0, 100, 2 * ROW_H));
        l.set_count(3);
        l.draw(&mut c, |_| "row");
        assert_eq!(c.get(150, 2), Some(ACCENT), "the selected row");
        assert_eq!(c.get(150, 2 * ROW_H as i32), Some(0), "below the list");
    }
}
