// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Text being edited, with a caret and a selection, at most `max` bytes of UTF-8. The
//! caret and every cut fall between characters; what is put in has no control
//! characters but the line breaks a multi-line edit asks for.

use alloc::string::String;

#[derive(Debug, Clone)]
pub struct TextEdit {
    text: String,
    caret: usize,
    anchor: usize,
    max: usize,
    lines: bool,
}

/// Where a word ends for Ctrl and the arrows: a space or punctuation.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextEdit {
    pub fn new(max: usize, lines: bool) -> Self {
        // What a person types in lines goes up to its size: no copy to grow it.
        let room = if lines { max } else { 0 };
        Self {
            text: String::with_capacity(room),
            caret: 0,
            anchor: 0,
            max,
            lines,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selection as `(start, end)`, in bytes; empty when they are equal.
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected(&self) -> &str {
        let (from, to) = self.selection();
        &self.text[from..to]
    }

    /// Replaces the text; the caret goes to its end.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        self.insert(text);
    }

    /// Puts `text` in place of the selection, as much as fits. Whether anything changed.
    pub fn insert(&mut self, text: &str) -> bool {
        let (from, to) = self.selection();
        let mut room = self.max - (self.text.len() - (to - from));
        let mut clean = String::new();
        for c in text.chars() {
            let c = match c {
                '\n' | '\t' | '\r' if self.lines => c,
                '\n' | '\t' | '\r' => ' ',
                c if c.is_control() => '?',
                c => c,
            };
            if c.len_utf8() > room {
                break;
            }
            room -= c.len_utf8();
            clean.push(c);
        }
        if clean.is_empty() && from == to {
            return false;
        }
        self.text.replace_range(from..to, &clean);
        self.caret = from + clean.len();
        self.anchor = self.caret;
        true
    }

    /// Removes the selection, or the character before the caret.
    pub fn backspace(&mut self) -> bool {
        if self.caret == self.anchor {
            self.anchor = self.before(self.caret);
        }
        self.insert("")
    }

    /// Removes the selection, or the character after the caret.
    pub fn delete(&mut self) -> bool {
        if self.caret == self.anchor {
            self.anchor = self.after(self.caret);
        }
        self.insert("")
    }

    /// Where the character before `pos` starts; a CRLF is one character.
    pub fn before(&self, pos: usize) -> usize {
        if self.text[..pos].ends_with("\r\n") {
            return pos - 2;
        }
        self.text[..pos]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    /// Where the character after `pos` starts; a CRLF is one character.
    pub fn after(&self, pos: usize) -> usize {
        if self.text[pos..].starts_with("\r\n") {
            return pos + 2;
        }
        self.text[pos..]
            .chars()
            .next()
            .map_or(pos, |c| pos + c.len_utf8())
    }

    /// The start of the word before `pos`.
    pub fn word_before(&self, pos: usize) -> usize {
        let mut chars = self.text[..pos].char_indices().rev().peekable();
        while chars.next_if(|&(_, c)| !is_word(c)).is_some() {}
        let mut at = chars.peek().map_or(0, |&(i, c)| i + c.len_utf8());
        while let Some((i, _)) = chars.next_if(|&(_, c)| is_word(c)) {
            at = i;
        }
        at
    }

    /// The end of the word after `pos`.
    pub fn word_after(&self, pos: usize) -> usize {
        let mut chars = self.text[pos..].char_indices().peekable();
        while chars.next_if(|&(_, c)| !is_word(c)).is_some() {}
        while chars.next_if(|&(_, c)| is_word(c)).is_some() {}
        chars.peek().map_or(self.text.len(), |&(i, _)| pos + i)
    }

    /// The start of the line that holds `pos`.
    pub fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    /// The end of the line that holds `pos`, before its line break, CRLF or LF.
    pub fn line_end(&self, pos: usize) -> usize {
        let rest = &self.text[pos..];
        let end = rest.find('\n').map_or(self.text.len(), |i| pos + i);
        if end > pos && self.text.as_bytes()[end - 1] == b'\r' && end < self.text.len() {
            end - 1
        } else {
            end
        }
    }

    /// Moves the caret to `pos`, a character's start; with `extend` the selection follows.
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        debug_assert!(self.text.is_char_boundary(pos));
        self.caret = pos;
        if !extend {
            self.anchor = pos;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str) -> TextEdit {
        let mut e = TextEdit::new(32, false);
        e.set_text(text);
        e
    }

    #[test]
    fn the_caret_stays_between_characters() {
        let mut e = edit("añ€");
        assert_eq!(e.caret(), 6);
        assert!(e.backspace());
        assert_eq!(e.text(), "añ");
        e.move_to(e.before(e.caret()), false);
        assert_eq!(e.caret(), 1);
        assert!(e.delete());
        assert_eq!(e.text(), "a");
        // Nothing to delete changes nothing.
        e.move_to(1, false);
        assert!(!e.delete());
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut e = edit("hello");
        e.move_to(1, false);
        e.move_to(4, true);
        assert_eq!(e.selected(), "ell");
        assert!(e.insert("ipp"));
        assert_eq!(e.text(), "hippo");
        assert_eq!(e.caret(), 4);
        assert_eq!(e.selected(), "");
        e.select_all();
        assert!(e.backspace());
        assert_eq!(e.text(), "");
    }

    #[test]
    fn it_never_goes_past_its_size_nor_cuts_a_character() {
        let mut e = TextEdit::new(5, false);
        e.set_text("abcd€");
        assert_eq!(e.text(), "abcd");
        e.select_all();
        // A selection's room counts.
        e.insert("12345");
        assert_eq!(e.text(), "12345");
        assert!(!e.insert("6"));
    }

    #[test]
    fn controls_do_not_get_in() {
        let e = edit("a\u{7}b\nc\td");
        assert_eq!(e.text(), "a?b c d");
        let mut lines = TextEdit::new(32, true);
        lines.set_text("a\u{7}b\nc\td");
        assert_eq!(lines.text(), "a?b\nc\td");
        lines.set_text("a\r\nb");
        assert_eq!(
            lines.text(),
            "a\r\nb",
            "a file's line ends stay as they were"
        );
    }

    #[test]
    fn a_crlf_is_one_line_end() {
        let mut e = TextEdit::new(32, true);
        e.set_text("ab\r\ncd");
        assert_eq!(e.line_end(0), 2);
        assert_eq!(e.after(2), 4);
        assert_eq!(e.before(4), 2);
        e.move_to(4, false);
        assert!(e.backspace());
        assert_eq!(e.text(), "abcd", "both go together");
        e.set_text("ab\r\ncd");
        e.move_to(2, false);
        assert!(e.delete());
        assert_eq!(e.text(), "abcd");
    }

    #[test]
    fn words() {
        let e = edit("uno, dos  tres");
        assert_eq!(e.word_before(14), 10);
        assert_eq!(e.word_before(10), 5);
        assert_eq!(e.word_before(5), 0);
        assert_eq!(e.word_before(0), 0);
        assert_eq!(e.word_after(0), 3);
        assert_eq!(e.word_after(3), 8);
        assert_eq!(e.word_after(8), 14);
        assert_eq!(e.word_after(14), 14);
    }
}
