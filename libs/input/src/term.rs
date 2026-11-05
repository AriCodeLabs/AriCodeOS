// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Keys back from the bytes a terminal sends: what [`crate::Keyboard`]
//! produces (characters in UTF-8, Enter, backspace, control characters and
//! the escape sequences of the editing keys, `ESC [ A`, `ESC [ 3 ~`), as
//! the shell's line editor and the desktop's windows receive it. One
//! decoder for both (audit of ADR 0074 phase C, L6).

/// A key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character: never a control (C0, DEL or C1).
    Char(char),
    Enter,
    Backspace,
    Tab,
    /// A lone `ESC`: told only when the next byte shows it was not a
    /// sequence's start, so it arrives with the key after it.
    Escape,
    /// Ctrl with a letter: `Ctrl(b'a')` for `0x01`.
    Ctrl(u8),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Plain,
    /// After `ESC`.
    Escape,
    /// After `ESC [` or `ESC O`; `first` is the first parameter so far and
    /// `more` whether a `;` ended it.
    Sequence { first: u8, more: bool },
    /// Inside a character of UTF-8 (ADR 0076): its bytes so far, and how
    /// many it has.
    Utf8 { buf: [u8; 4], have: u8, need: u8 },
}

/// Decodes bytes into keys, one byte at a time.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyDecoder {
    state: State,
}

impl KeyDecoder {
    pub const fn new() -> Self {
        Self {
            state: State::Plain,
        }
    }

    /// Feeds one byte; returns up to two keys (a lone `ESC` and the key
    /// after it). An unknown sequence is dropped whole; its modifiers
    /// (`ESC [ 1 ; 5 A`) are ignored. A character that is not valid UTF-8
    /// is dropped, and the byte that showed it starts over.
    pub fn feed(&mut self, b: u8) -> [Option<Key>; 2] {
        match self.state {
            State::Plain if b == 0x1B => {
                self.state = State::Escape;
                [None, None]
            }
            State::Plain => [self.plain(b), None],
            State::Utf8 {
                mut buf,
                have,
                need,
            } => {
                if b & 0xC0 != 0x80 {
                    // Cut short: the character is lost, this byte is not.
                    self.state = State::Plain;
                    return self.feed(b);
                }
                buf[have as usize] = b;
                let have = have + 1;
                if have < need {
                    self.state = State::Utf8 { buf, have, need };
                    return [None, None];
                }
                self.state = State::Plain;
                let ch = core::str::from_utf8(&buf[..have as usize])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .filter(|c| !c.is_control());
                [ch.map(Key::Char), None]
            }
            State::Escape => match b {
                b'[' | b'O' => {
                    self.state = State::Sequence {
                        first: 0,
                        more: false,
                    };
                    [None, None]
                }
                0x1B => [Some(Key::Escape), None],
                _ => {
                    self.state = State::Plain;
                    [Some(Key::Escape), self.plain(b)]
                }
            },
            State::Sequence { first, more } => match b {
                b'0'..=b'9' if !more => {
                    self.state = State::Sequence {
                        first: first.saturating_mul(10).saturating_add(b - b'0'),
                        more,
                    };
                    [None, None]
                }
                b'0'..=b'9' | b';' => {
                    self.state = State::Sequence { first, more: true };
                    [None, None]
                }
                // A new sequence starts over.
                0x1B => {
                    self.state = State::Escape;
                    [None, None]
                }
                _ => {
                    self.state = State::Plain;
                    [final_key(b, first), None]
                }
            },
        }
    }
}

impl KeyDecoder {
    /// A byte outside any sequence: a key, or the start of a character of
    /// two to four bytes.
    fn plain(&mut self, b: u8) -> Option<Key> {
        let need = match b {
            b'\r' | b'\n' => return Some(Key::Enter),
            0x08 | 0x7F => return Some(Key::Backspace),
            b'\t' => return Some(Key::Tab),
            0x01..=0x1A => return Some(Key::Ctrl(b'a' + b - 1)),
            0x20..=0x7E => return Some(Key::Char(b as char)),
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return None,
        };
        let mut buf = [0u8; 4];
        buf[0] = b;
        self.state = State::Utf8 { buf, have: 1, need };
        None
    }
}

fn final_key(b: u8, first: u8) -> Option<Key> {
    Some(match (b, first) {
        (b'A', _) => Key::Up,
        (b'B', _) => Key::Down,
        (b'C', _) => Key::Right,
        (b'D', _) => Key::Left,
        (b'H', _) | (b'~', 1) | (b'~', 7) => Key::Home,
        (b'F', _) | (b'~', 4) | (b'~', 8) => Key::End,
        (b'~', 3) => Key::Delete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8], out: &mut [Option<Key>; 16]) -> usize {
        let mut d = KeyDecoder::new();
        let mut n = 0;
        for &b in bytes {
            for k in d.feed(b).into_iter().flatten() {
                out[n] = Some(k);
                n += 1;
            }
        }
        n
    }

    fn check(bytes: &[u8], want: &[Key]) {
        let mut out = [None; 16];
        let n = keys(bytes, &mut out);
        let got: [Option<Key>; 16] = out;
        assert_eq!(n, want.len(), "{bytes:?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(*g, Some(*w), "{bytes:?}");
        }
    }

    #[test]
    fn decodes_what_the_keyboard_sends() {
        check(
            b"a\x1b[A\x1b[3~\r\x7f\x1b[D\t\x01\x15",
            &[
                Key::Char('a'),
                Key::Up,
                Key::Delete,
                Key::Enter,
                Key::Backspace,
                Key::Left,
                Key::Tab,
                Key::Ctrl(b'a'),
                Key::Ctrl(b'u'),
            ],
        );
        check(b"\x1bOH\x1b[4~\x1b[7~", &[Key::Home, Key::End, Key::Home]);
    }

    #[test]
    fn escapes_and_odd_sequences() {
        // A lone ESC comes with the key after it; two ESCs give the first.
        check(b"\x1bq", &[Key::Escape, Key::Char('q')]);
        check(b"\x1b\x1b[B", &[Key::Escape, Key::Down]);
        // Modifiers are ignored; parameters past the first do not count.
        check(b"\x1b[1;5A\x1b[3;5~", &[Key::Up, Key::Delete]);
        check(b"\x1b[15~x", &[Key::Char('x')]);
        // Unknown sequences are dropped whole; an ESC inside one restarts.
        check(b"\x1b[Z\x1b[9~k", &[Key::Char('k')]);
        check(b"\x1b[1\x1b[C", &[Key::Right]);
        check(b"\x1b[999~z", &[Key::Char('z')]);
    }

    #[test]
    fn characters_in_utf8() {
        check(
            "añ€😀".as_bytes(),
            &[
                Key::Char('a'),
                Key::Char('ñ'),
                Key::Char('€'),
                Key::Char('😀'),
            ],
        );
        // A cut character is dropped; the byte that cut it still counts.
        check(b"\xC3a\xE2\x82\r", &[Key::Char('a'), Key::Enter]);
        // Bytes that start nothing, and C1 controls, are no key.
        check(b"\x80\xFF\xC2\x85z", &[Key::Char('z')]);
        // After a lone ESC.
        check("\x1bñ".as_bytes(), &[Key::Escape, Key::Char('ñ')]);
    }
}
