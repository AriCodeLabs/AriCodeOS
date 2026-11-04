// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Keys back from the bytes a terminal sends: what [`crate::Keyboard`]
//! produces (printable ASCII, Enter, backspace, control characters and
//! the escape sequences of the editing keys, `ESC [ A`, `ESC [ 3 ~`), as
//! the shell's line editor and the desktop's windows receive it. One
//! decoder for both (audit of ADR 0074 phase C, L6).

/// A key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(u8),
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
    /// (`ESC [ 1 ; 5 A`) are ignored.
    pub fn feed(&mut self, b: u8) -> [Option<Key>; 2] {
        match self.state {
            State::Plain if b == 0x1B => {
                self.state = State::Escape;
                [None, None]
            }
            State::Plain => [plain(b), None],
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
                    [Some(Key::Escape), plain(b)]
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

fn plain(b: u8) -> Option<Key> {
    match b {
        b'\r' | b'\n' => Some(Key::Enter),
        0x08 | 0x7F => Some(Key::Backspace),
        b'\t' => Some(Key::Tab),
        0x01..=0x1A => Some(Key::Ctrl(b'a' + b - 1)),
        0x20..=0x7E => Some(Key::Char(b)),
        _ => None,
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
                Key::Char(b'a'),
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
        check(b"\x1bq", &[Key::Escape, Key::Char(b'q')]);
        check(b"\x1b\x1b[B", &[Key::Escape, Key::Down]);
        // Modifiers are ignored; parameters past the first do not count.
        check(b"\x1b[1;5A\x1b[3;5~", &[Key::Up, Key::Delete]);
        check(b"\x1b[15~x", &[Key::Char(b'x')]);
        // Unknown sequences are dropped whole; an ESC inside one restarts.
        check(b"\x1b[Z\x1b[9~k", &[Key::Char(b'k')]);
        check(b"\x1b[1\x1b[C", &[Key::Right]);
        check(b"\x1b[999~z", &[Key::Char(b'z')]);
    }
}
