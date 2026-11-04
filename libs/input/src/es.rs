// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The Spanish keyboard (ISO) as Linux types it (ADR 0076): four levels
//! per key (plain, Shift, AltGr, Shift+AltGr) and the dead keys of the
//! accents, which combine with the next key. The facts are those of the
//! `es` layout of xkeyboard-config and of the dead-key rules of its
//! `Compose` file; `tools/check-keymap.py` compares this table with them
//! on a Linux host, key by key.

/// What one level of a key gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sym {
    None,
    Char(char),
    Dead(Dead),
}

/// The dead keys combined here: the accents of Spanish, Catalan,
/// Portuguese and French that make a Latin-1 letter. The other dead keys
/// of the layout (caron, breve, macron, ogonek, ...) give nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dead {
    Grave,
    Acute,
    Circumflex,
    Diaeresis,
    Tilde,
    Cedilla,
}

impl Dead {
    /// The dead key with a space (the `Compose` file's rule).
    pub const fn with_space(self) -> char {
        match self {
            Self::Grave => '`',
            Self::Acute => '\'',
            Self::Circumflex => '^',
            Self::Diaeresis => '"',
            Self::Tilde => '~',
            Self::Cedilla => '¸',
        }
    }

    /// The dead key pressed twice, and what a key it does not combine with
    /// is preceded by (as the Linux console does).
    pub const fn alone(self) -> char {
        match self {
            Self::Grave => '`',
            Self::Acute => '´',
            Self::Circumflex => '^',
            Self::Diaeresis => '¨',
            Self::Tilde => '~',
            Self::Cedilla => '¸',
        }
    }

    /// `base` with this accent, when Latin-1 has that letter.
    pub fn combine(self, base: char) -> Option<char> {
        let (from, to): (&str, &str) = match self {
            Self::Grave => ("aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
            Self::Acute => ("aeiouyAEIOUY", "áéíóúýÁÉÍÓÚÝ"),
            Self::Circumflex => ("aeiouAEIOU.123", "âêîôûÂÊÎÔÛ·¹²³"),
            Self::Diaeresis => ("aeiouyAEIOU", "äëïöüÿÄËÏÖÜ"),
            Self::Tilde => ("anoANO", "ãñõÃÑÕ"),
            Self::Cedilla => ("cC", "çÇ"),
        };
        let i = from.chars().position(|c| c == base)?;
        to.chars().nth(i)
    }
}

/// Whether two levels are a letter and its capital (`a`/`A`, `ñ`/`Ñ`,
/// `æ`/`Æ`): the levels Caps Lock swaps.
pub fn is_case_pair(lower: Sym, upper: Sym) -> bool {
    let (Sym::Char(l), Sym::Char(u)) = (lower, upper) else {
        return false;
    };
    let mut up = l.to_uppercase();
    l != u && up.next() == Some(u) && up.next().is_none()
}

const N: Sym = Sym::None;

const fn c(ch: char) -> Sym {
    Sym::Char(ch)
}

const fn d(dead: Dead) -> Sym {
    Sym::Dead(dead)
}

/// A key whose four levels are the same character (Tab, space, ...).
const fn same(ch: char) -> [Sym; 4] {
    [c(ch), c(ch), c(ch), c(ch)]
}

/// A letter with its capital and two AltGr characters.
const fn letter(lower: char, upper: char, l3: Sym, l4: Sym) -> [Sym; 4] {
    [c(lower), c(upper), l3, l4]
}

/// Evdev codes 1..=86 (Esc..`<`), each with its four levels. Escape,
/// Backspace, Tab and Enter give their control character on every level;
/// the keypad and the function keys give nothing here.
pub const KEYS: [[Sym; 4]; 87] = {
    let mut k = [[N; 4]; 87];
    k[1] = same('\x1b');
    k[2] = [c('1'), c('!'), c('|'), c('¡')];
    k[3] = [c('2'), c('"'), c('@'), c('⅛')];
    k[4] = [c('3'), c('·'), c('#'), c('£')];
    k[5] = [c('4'), c('$'), c('~'), c('$')];
    k[6] = [c('5'), c('%'), c('½'), c('⅜')];
    k[7] = [c('6'), c('&'), c('¬'), c('⅝')];
    k[8] = [c('7'), c('/'), c('{'), c('⅞')];
    k[9] = [c('8'), c('('), c('['), c('™')];
    k[10] = [c('9'), c(')'), c(']'), c('±')];
    k[11] = [c('0'), c('='), c('}'), c('°')];
    k[12] = [c('\''), c('?'), c('\\'), c('¿')];
    // Ogonek on the fourth level: not combined.
    k[13] = [c('¡'), c('¿'), d(Dead::Cedilla), N];
    k[14] = same('\x08');
    k[15] = same('\t');
    k[16] = letter('q', 'Q', c('@'), c('Ω'));
    k[17] = letter('w', 'W', c('ſ'), c('§'));
    k[18] = letter('e', 'E', c('€'), c('¢'));
    k[19] = letter('r', 'R', c('¶'), c('®'));
    k[20] = letter('t', 'T', c('ŧ'), c('Ŧ'));
    k[21] = letter('y', 'Y', c('←'), c('¥'));
    k[22] = letter('u', 'U', c('↓'), c('↑'));
    k[23] = letter('i', 'I', c('→'), c('ı'));
    k[24] = letter('o', 'O', c('ø'), c('Ø'));
    k[25] = letter('p', 'P', c('þ'), c('Þ'));
    // Ring above on the fourth level.
    k[26] = [d(Dead::Grave), d(Dead::Circumflex), c('['), N];
    // Macron on the fourth level.
    k[27] = [c('+'), c('*'), c(']'), N];
    k[28] = same('\n');
    k[30] = letter('a', 'A', c('æ'), c('Æ'));
    k[31] = letter('s', 'S', c('ß'), c('ẞ'));
    k[32] = letter('d', 'D', c('ð'), c('Ð'));
    k[33] = letter('f', 'F', c('đ'), c('ª'));
    k[34] = letter('g', 'G', c('ŋ'), c('Ŋ'));
    k[35] = letter('h', 'H', c('ħ'), c('Ħ'));
    // Hook and horn: not combined.
    k[36] = letter('j', 'J', N, N);
    k[37] = letter('k', 'K', c('ĸ'), c('&'));
    k[38] = letter('l', 'L', c('ł'), c('Ł'));
    // Double acute on the fourth level.
    k[39] = letter('ñ', 'Ñ', d(Dead::Tilde), N);
    // Caron on the fourth level.
    k[40] = [d(Dead::Acute), d(Dead::Diaeresis), c('{'), N];
    k[41] = [c('º'), c('ª'), c('\\'), c('\\')];
    // Breve on the fourth level.
    k[43] = letter('ç', 'Ç', c('}'), N);
    k[44] = letter('z', 'Z', c('«'), c('<'));
    k[45] = letter('x', 'X', c('»'), c('>'));
    k[46] = letter('c', 'C', c('¢'), c('©'));
    k[47] = letter('v', 'V', c('„'), c('‚'));
    k[48] = letter('b', 'B', c('“'), c('‘'));
    k[49] = letter('n', 'N', c('”'), c('’'));
    k[50] = letter('m', 'M', c('µ'), c('º'));
    k[51] = [c(','), c(';'), c('•'), c('×')];
    k[52] = [c('.'), c(':'), c('·'), c('÷')];
    // Dot below and dot above: not combined.
    k[53] = [c('-'), c('_'), N, N];
    k[55] = same('*');
    k[57] = same(' ');
    k[86] = [c('<'), c('>'), c('|'), c('¦')];
    k
};
