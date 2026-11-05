// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Input events (Phase 25): the virtio-input format matches Linux evdev
//! (`type`, `code`, `value`), so the same library will serve USB HID when it
//! arrives. Includes a minimal US keyboard that turns key presses into
//! characters.

#![no_std]

use aricode_abi::ps2;

pub mod es;
pub mod term;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;

pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_WHEEL: u16 = 8;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
/// `<` key of ISO keyboards (next to the left shift).
pub const KEY_102ND: u16 = 86;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_CAPSLOCK: u16 = 58;

/// An event as delivered by the device (8 bytes, little endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: u32,
}

impl Event {
    pub const LEN: usize = 8;

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::LEN {
            return None;
        }
        Some(Self {
            kind: u16::from_le_bytes([b[0], b[1]]),
            code: u16::from_le_bytes([b[2], b[3]]),
            value: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        })
    }

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[..2].copy_from_slice(&self.kind.to_le_bytes());
        b[2..4].copy_from_slice(&self.code.to_le_bytes());
        b[4..].copy_from_slice(&self.value.to_le_bytes());
        b
    }

    pub fn is_key_press(&self) -> bool {
        self.kind == EV_KEY && self.value == 1
    }
    pub fn is_key_release(&self) -> bool {
        self.kind == EV_KEY && self.value == 0
    }
}

/// Keyboard layout: US (ANSI QWERTY) or Spanish (ISO, with `AltGr` and
/// dead keys, as Linux types it: [`es`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Us,
    Es,
}

impl Layout {
    /// Name used on the cmdline (`kbd=es`).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "us" => Some(Self::Us),
            "es" => Some(Self::Es),
            _ => None,
        }
    }

    /// A small number for the layout, to pass it in a process's `arg`
    /// (init to the console bridge).
    pub const fn code(self) -> u64 {
        match self {
            Self::Us => 0,
            Self::Es => 1,
        }
    }

    /// The layout of `code`; US for an unknown one.
    pub const fn from_code(code: u64) -> Self {
        match code {
            1 => Self::Es,
            _ => Self::Us,
        }
    }
}

/// Keyboard: tracks shift, Ctrl and `AltGr` and translates key presses
/// according to the layout into UTF-8 (ADR 0076). Ctrl with a letter gives
/// its control character (`0x01` for Ctrl+A), as a terminal sends it; with
/// any other key, nothing. A dead key gives nothing until the next key:
/// the accented letter, or the accent and that key when they do not
/// combine (as the Linux console does).
#[derive(Debug, Default, Clone, Copy)]
pub struct Keyboard {
    shift: bool,
    caps: bool,
    altgr: bool,
    /// Bits: left Ctrl, right Ctrl.
    ctrl: u8,
    layout: Layout,
    /// A dead key pressed, waiting for the next key.
    dead: Option<es::Dead>,
}

/// Row of evdev codes 1..=57 (esc..space) without and with shift.
const PLAIN: &[u8; 58] =
    b"\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 58] =
    b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

impl Keyboard {
    pub const fn new() -> Self {
        Self::with_layout(Layout::Us)
    }

    pub const fn with_layout(layout: Layout) -> Self {
        Self {
            shift: false,
            caps: false,
            altgr: false,
            ctrl: 0,
            layout,
            dead: None,
        }
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    /// Forgets Shift, Ctrl and AltGr held: a device whose releases were
    /// lost (its driver restarted, its read dropped) does not leave every
    /// letter shifted or a control character (audit of the editor, L1).
    pub fn release_modifiers(&mut self) {
        self.shift = false;
        self.altgr = false;
        self.ctrl = 0;
        self.dead = None;
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Processes an event; returns the bytes it produces (none, one
    /// character or an ANSI escape sequence for the editing keys).
    pub fn feed(&mut self, ev: Event) -> KeyBytes {
        if ev.kind != EV_KEY {
            return KeyBytes::NONE;
        }
        match ev.code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                self.shift = ev.value != 0;
                return KeyBytes::NONE;
            }
            KEY_CAPSLOCK => {
                if ev.value == 1 {
                    self.caps = !self.caps;
                }
                return KeyBytes::NONE;
            }
            KEY_RIGHTALT => {
                self.altgr = ev.value != 0;
                return KeyBytes::NONE;
            }
            KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                let bit = if ev.code == KEY_LEFTCTRL { 1 } else { 2 };
                if ev.value == 0 {
                    self.ctrl &= !bit;
                } else {
                    self.ctrl |= bit;
                }
                return KeyBytes::NONE;
            }
            _ => {}
        }
        if ev.value != 1 {
            return KeyBytes::NONE; // presses only (no repeats nor releases)
        }
        // Editing keys: the same sequences a serial terminal sends, so the
        // shell handles them the same no matter where they come from.
        let escape: Option<&[u8]> = match ev.code {
            KEY_UP => Some(b"\x1b[A"),
            KEY_DOWN => Some(b"\x1b[B"),
            KEY_RIGHT => Some(b"\x1b[C"),
            KEY_LEFT => Some(b"\x1b[D"),
            KEY_HOME => Some(b"\x1b[H"),
            KEY_END => Some(b"\x1b[F"),
            KEY_DELETE => Some(b"\x1b[3~"),
            _ => None,
        };
        if let Some(seq) = escape {
            // An editing key ends a dead key unused.
            self.dead = None;
            return KeyBytes::sequence(seq);
        }
        let i = ev.code as usize;
        let levels = match self.layout {
            Layout::Us if i < PLAIN.len() => [
                ascii(PLAIN[i]),
                ascii(SHIFTED[i]),
                es::Sym::None,
                es::Sym::None,
            ],
            Layout::Es if i < es::KEYS.len() => es::KEYS[i],
            // A key that types nothing ends a dead key unused (audit of
            // ADR 0076, L6).
            _ => {
                self.dead = None;
                return KeyBytes::NONE;
            }
        };
        if self.ctrl != 0 {
            // Ctrl with a letter of the alphabet is its control character;
            // anything else gives nothing and ends a dead key.
            self.dead = None;
            return match levels[0] {
                es::Sym::Char(c) if c.is_ascii_lowercase() => KeyBytes::single(c as u8 & 0x1F),
                _ => KeyBytes::NONE,
            };
        }
        // Caps Lock acts on a pair of levels that is a letter and its
        // capital, as xkb's alphabetic key types do: a and A, ñ and Ñ, æ and
        // Æ with AltGr; not º and ª (audit of ADR 0076, L5).
        let base = usize::from(self.altgr) * 2;
        let letter = es::is_case_pair(levels[base], levels[base + 1]);
        let upper = self.shift ^ (self.caps && letter);
        let sym = levels[base + usize::from(upper)];
        let ch = match sym {
            es::Sym::None => {
                self.dead = None;
                return KeyBytes::NONE;
            }
            es::Sym::Dead(dead) => {
                return match self.dead.replace(dead) {
                    // The same dead key twice: the accent itself.
                    Some(first) if first == dead => {
                        self.dead = None;
                        KeyBytes::chars(&[dead.alone()])
                    }
                    // Another dead key: the first one's accent, and this
                    // one waits.
                    Some(first) => KeyBytes::chars(&[first.alone()]),
                    None => KeyBytes::NONE,
                };
            }
            es::Sym::Char(ch) => ch,
        };
        match self.dead.take() {
            None => KeyBytes::chars(&[ch]),
            Some(dead) if ch == ' ' => KeyBytes::chars(&[dead.with_space()]),
            Some(dead) => match dead.combine(ch) {
                Some(accented) => KeyBytes::chars(&[accented]),
                // Control keys (Enter, Tab, Backspace, Esc) end it unused.
                None if ch.is_control() => KeyBytes::chars(&[ch]),
                None => KeyBytes::chars(&[dead.alone(), ch]),
            },
        }
    }
}

/// Bytes produced by a key press: nothing, a character or two in UTF-8
/// (an accent that did not combine and the key after it) or an escape
/// sequence; at most [`KeyBytes::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBytes {
    buf: [u8; KeyBytes::MAX],
    len: u8,
}

/// A level of a US key: its ASCII character, or nothing.
const fn ascii(b: u8) -> es::Sym {
    if b == 0 {
        es::Sym::None
    } else {
        es::Sym::Char(b as char)
    }
}

impl KeyBytes {
    /// Two characters of up to four bytes each in UTF-8.
    pub const MAX: usize = 8;

    pub const NONE: Self = Self {
        buf: [0; Self::MAX],
        len: 0,
    };

    const fn single(b: u8) -> Self {
        let mut buf = [0; Self::MAX];
        buf[0] = b;
        Self { buf, len: 1 }
    }

    /// `chars` in UTF-8; at most two fit.
    fn chars(chars: &[char]) -> Self {
        let mut k = Self::NONE;
        for ch in chars.iter().take(2) {
            let n = ch.encode_utf8(&mut k.buf[k.len as usize..]).len();
            k.len += n as u8;
        }
        k
    }

    fn sequence(seq: &[u8]) -> Self {
        let mut buf = [0u8; Self::MAX];
        let len = seq.len().min(Self::MAX);
        buf[..len].copy_from_slice(&seq[..len]);
        Self {
            buf,
            len: len as u8,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Human-readable name of a non-text event (for traces).
pub fn describe(ev: &Event) -> &'static str {
    match (ev.kind, ev.code) {
        (EV_SYN, _) => "syn",
        (EV_REL, REL_X) => "rel x",
        (EV_REL, REL_Y) => "rel y",
        (EV_REL, REL_WHEEL) => "wheel",
        (EV_REL, _) => "rel",
        (EV_ABS, _) => "abs",
        (EV_KEY, BTN_LEFT) => "left button",
        (EV_KEY, BTN_RIGHT) => "right button",
        (EV_KEY, BTN_MIDDLE) => "middle button",
        (EV_KEY, _) => "key",
        _ => "?",
    }
}

/// Decoder of set 1 scancodes (PS/2 keyboard with i8042 translation) into
/// evdev events: codes 0x01..=0x58 match the Linux keycodes; the `E0`
/// prefix introduces the extended keys and `E1` (Pause) is discarded along
/// with its two bytes.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScancodeSet1 {
    extended: bool,
    skip: u8,
}

pub const KEY_KPENTER: u16 = 96;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_KPSLASH: u16 = 98;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_COMPOSE: u16 = 127;

impl ScancodeSet1 {
    pub const fn new() -> Self {
        Self {
            extended: false,
            skip: 0,
        }
    }

    fn extended_code(code: u8) -> Option<u16> {
        Some(match code {
            0x1C => KEY_KPENTER,
            0x1D => KEY_RIGHTCTRL,
            0x35 => KEY_KPSLASH,
            0x38 => KEY_RIGHTALT,
            0x47 => KEY_HOME,
            0x48 => KEY_UP,
            0x49 => KEY_PAGEUP,
            0x4B => KEY_LEFT,
            0x4D => KEY_RIGHT,
            0x4F => KEY_END,
            0x50 => KEY_DOWN,
            0x51 => KEY_PAGEDOWN,
            0x52 => KEY_INSERT,
            0x53 => KEY_DELETE,
            0x5B => KEY_LEFTMETA,
            0x5C => KEY_RIGHTMETA,
            0x5D => KEY_COMPOSE,
            _ => return None,
        })
    }

    /// Processes a keyboard byte; returns the complete event, if any.
    pub fn feed(&mut self, byte: u8) -> Option<Event> {
        if self.skip > 0 {
            self.skip -= 1;
            return None;
        }
        match byte {
            0xE0 => {
                self.extended = true;
                return None;
            }
            0xE1 => {
                self.skip = 2;
                return None;
            }
            _ => {}
        }
        let released = byte & 0x80 != 0;
        let code = byte & 0x7F;
        let extended = core::mem::take(&mut self.extended);
        let key = if extended {
            Self::extended_code(code)?
        } else if (1..=0x58).contains(&code) {
            code as u16
        } else {
            return None;
        };
        Some(Event {
            kind: EV_KEY,
            code: key,
            value: if released { 0 } else { 1 },
        })
    }
}

/// Events a boot HID report can produce: 8 modifier changes, 6 releases,
/// 6 presses and `EV_SYN` (21); 24 are reserved.
pub const MAX_REPORT_EVENTS: usize = 24;

/// Boot HID keyboard (USB): 8-byte report (modifiers, reserved, up to 6
/// pressed usages). Translates each report into evdev events by comparing it
/// with the previous one. `out` has room for the worst case, so no change is
/// lost and the state never advances without having delivered it.
#[derive(Debug, Default, Clone, Copy)]
pub struct HidBootKeyboard {
    modifiers: u8,
    keys: [u8; 6],
}

pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_ESC: u16 = 1;
pub const KEY_TAB: u16 = 15;
pub const KEY_SPACE: u16 = 57;

/// evdev keycode of an HID keyboard page usage (0 if unknown).
pub fn hid_usage_to_key(usage: u8) -> u16 {
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    match usage {
        0x04..=0x1D => LETTERS[(usage - 0x04) as usize],
        0x1E..=0x26 => 2 + (usage - 0x1E) as u16, // 1..9
        0x27 => 11,                               // 0
        0x28 => KEY_ENTER,
        0x29 => KEY_ESC,
        0x2A => KEY_BACKSPACE,
        0x2B => KEY_TAB,
        0x2C => KEY_SPACE,
        0x2D => 12, // -
        0x2E => 13, // =
        0x2F => 26, // [
        0x30 => 27, // ]
        0x31 => 43, // backslash
        0x33 => 39, // ;
        0x34 => 40, // '
        0x35 => 41, // `
        0x36 => 51, // ,
        0x37 => 52, // .
        0x38 => 53, // /
        0x39 => KEY_CAPSLOCK,
        0x3A..=0x43 => 59 + (usage - 0x3A) as u16, // F1..F10
        0x44 => 87,                                // F11
        0x45 => 88,                                // F12
        0x49 => KEY_INSERT,
        0x4A => KEY_HOME,
        0x4B => KEY_PAGEUP,
        0x4C => KEY_DELETE,
        0x4D => KEY_END,
        0x4E => KEY_PAGEDOWN,
        0x4F => KEY_RIGHT,
        0x50 => KEY_LEFT,
        0x51 => KEY_DOWN,
        0x52 => KEY_UP,
        _ => 0,
    }
}

/// Keycodes of the eight modifier bits of the boot report.
const MODIFIER_KEYS: [u16; 8] = [
    KEY_LEFTCTRL,
    KEY_LEFTSHIFT,
    KEY_LEFTALT,
    KEY_LEFTMETA,
    KEY_RIGHTCTRL,
    KEY_RIGHTSHIFT,
    KEY_RIGHTALT,
    KEY_RIGHTMETA,
];

impl HidBootKeyboard {
    pub const fn new() -> Self {
        Self {
            modifiers: 0,
            keys: [0; 6],
        }
    }

    /// Processes a report and writes the events into `out` (releases first,
    /// then presses, and `EV_SYN` last). Returns how many.
    pub fn feed(&mut self, report: &[u8], out: &mut [Event; MAX_REPORT_EVENTS]) -> usize {
        if report.len() < 8 {
            return 0;
        }
        let mods = report[0];
        let mut keys = [0u8; 6];
        keys.copy_from_slice(&report[2..8]);
        // An error report (0x01 in every position) is ignored.
        if keys.iter().all(|&k| k == 0x01) {
            return 0;
        }
        let mut n = 0;
        let mut push = |ev: Event, n: &mut usize| {
            if *n < out.len() {
                out[*n] = ev;
                *n += 1;
            }
        };
        for (bit, key) in MODIFIER_KEYS.iter().enumerate() {
            let was = self.modifiers & (1 << bit) != 0;
            let is = mods & (1 << bit) != 0;
            if was != is {
                push(
                    Event {
                        kind: EV_KEY,
                        code: *key,
                        value: is as u32,
                    },
                    &mut n,
                );
            }
        }
        for &k in self.keys.iter().filter(|&&k| k != 0 && !keys.contains(&k)) {
            let code = hid_usage_to_key(k);
            if code != 0 {
                push(
                    Event {
                        kind: EV_KEY,
                        code,
                        value: 0,
                    },
                    &mut n,
                );
            }
        }
        for &k in keys.iter().filter(|&&k| k != 0 && !self.keys.contains(&k)) {
            let code = hid_usage_to_key(k);
            if code != 0 {
                push(
                    Event {
                        kind: EV_KEY,
                        code,
                        value: 1,
                    },
                    &mut n,
                );
            }
        }
        self.modifiers = mods;
        self.keys = keys;
        if n > 0 {
            push(
                Event {
                    kind: EV_SYN,
                    code: 0,
                    value: 0,
                },
                &mut n,
            );
        }
        n
    }
}

/// Boot HID mouse: 3 or 4 byte report (buttons, dx, dy, wheel).
#[derive(Debug, Default, Clone, Copy)]
pub struct HidBootMouse {
    buttons: u8,
}

impl HidBootMouse {
    pub const fn new() -> Self {
        Self { buttons: 0 }
    }

    pub fn feed(&mut self, report: &[u8], out: &mut [Event; MAX_REPORT_EVENTS]) -> usize {
        if report.len() < 3 {
            return 0;
        }
        let wheel = if report.len() >= 4 {
            report[3] as i8 as i32
        } else {
            0
        };
        pointer_events(
            &mut self.buttons,
            report[0],
            report[1] as i8 as i32,
            report[2] as i8 as i32,
            wheel,
            out,
        )
    }
}

/// The events of one pointer report: the changes of the three buttons
/// against `held` (which follows them), the motion and the wheel, then
/// `EV_SYN` if anything changed. Shared by the HID and PS/2 mice.
fn pointer_events(
    held: &mut u8,
    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,
    out: &mut [Event; MAX_REPORT_EVENTS],
) -> usize {
    let mut n = 0;
    let mut push = |kind: u16, code: u16, value: i32, n: &mut usize| {
        if *n < out.len() {
            out[*n] = Event {
                kind,
                code,
                value: value as u32,
            };
            *n += 1;
        }
    };
    for (bit, code) in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE].iter().enumerate() {
        let was = *held & (1 << bit) != 0;
        let is = buttons & (1 << bit) != 0;
        if was != is {
            push(EV_KEY, *code, is as i32, &mut n);
        }
    }
    *held = buttons & 0x07;
    if dx != 0 {
        push(EV_REL, REL_X, dx, &mut n);
    }
    if dy != 0 {
        push(EV_REL, REL_Y, dy, &mut n);
    }
    if wheel != 0 {
        push(EV_REL, REL_WHEEL, wheel, &mut n);
    }
    if n > 0 {
        push(EV_SYN, 0, 0, &mut n);
    }
    n
}

/// PS/2 mouse (ADR 0075): the aux port's 3-byte packets, or 4-byte ones
/// once the IntelliMouse wheel was turned on. Byte 0 holds the buttons,
/// the 9th bits of X and Y and their overflow bits; bit 3 is always set,
/// which is how a decoder that lost a byte finds the next packet (as
/// Linux's `psmouse` does). PS/2 Y and the wheel grow the other way from
/// evdev's, so both are inverted.
#[derive(Debug, Clone, Copy)]
pub struct Ps2Mouse {
    packet: [u8; 4],
    have: usize,
    len: usize,
    buttons: u8,
}

impl Ps2Mouse {
    /// A decoder for packets of 4 bytes with `wheel`, 3 without.
    pub const fn new(wheel: bool) -> Self {
        Self {
            packet: [0; 4],
            have: 0,
            len: if wheel { 4 } else { 3 },
            buttons: 0,
        }
    }

    /// Forgets a packet half received (the stream was cut).
    pub fn resync(&mut self) {
        self.have = 0;
    }

    /// Processes one aux byte and writes the events of the packet it
    /// completes into `out`; returns how many. A first byte without bit 3
    /// is skipped: the decoder is out of step and waits for a packet start.
    /// A packet with an overflow bit set reports its buttons only.
    pub fn feed(&mut self, byte: u8, out: &mut [Event; MAX_REPORT_EVENTS]) -> usize {
        if self.have == 0 && byte & 0x08 == 0 {
            return 0;
        }
        self.packet[self.have] = byte;
        self.have += 1;
        if self.have < self.len {
            return 0;
        }
        self.have = 0;
        let [b0, b1, b2, b3] = self.packet;
        let (mut dx, mut dy) = (0, 0);
        if b0 & 0xC0 == 0 {
            dx = b1 as i32 - if b0 & 0x10 != 0 { 256 } else { 0 };
            dy = b2 as i32 - if b0 & 0x20 != 0 { 256 } else { 0 };
        }
        // The wheel is the low nibble of byte 3, as a signed 4-bit value.
        let wheel = if self.len == 4 {
            ((b3 << 4) as i8 >> 4) as i32
        } else {
            0
        };
        pointer_events(&mut self.buttons, b0 & 0x07, dx, -dy, -wheel, out)
    }

    /// The releases of the buttons held, so none stays down when the
    /// stream is cut; returns how many events.
    pub fn release_all(&mut self, out: &mut [Event; MAX_REPORT_EVENTS]) -> usize {
        self.have = 0;
        pointer_events(&mut self.buttons, 0, 0, 0, 0, out)
    }
}

/// The keys a keyboard holds down, so a stream cut in the middle (a lost
/// release, the end of a session) can be closed with their releases and no
/// modifier stays stuck at the reader (ADR 0075).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeysDown {
    bits: [u64; 4],
}

impl KeysDown {
    pub const fn new() -> Self {
        Self { bits: [0; 4] }
    }

    /// Follows one event (key codes from 256 up are not tracked).
    pub fn follow(&mut self, ev: &Event) {
        if ev.kind != EV_KEY || ev.code >= 256 {
            return;
        }
        let (word, bit) = (ev.code as usize / 64, 1u64 << (ev.code % 64));
        if ev.value == 0 {
            self.bits[word] &= !bit;
        } else {
            self.bits[word] |= bit;
        }
    }

    /// Calls `release` with a release of each key held, lowest code first,
    /// and forgets them.
    pub fn release_all(&mut self, mut release: impl FnMut(Event)) {
        for (word, bits) in self.bits.iter_mut().enumerate() {
            while *bits != 0 {
                let bit = bits.trailing_zeros();
                *bits &= !(1u64 << bit);
                release(Event {
                    kind: EV_KEY,
                    code: (word * 64) as u16 + bit as u16,
                    value: 0,
                });
            }
        }
    }
}

/// The PS/2 stream's queue (ADR 0075), kept by the kernel under its lock:
/// (tag, byte) entries (`aricode_abi::ps2`) for the one process that
/// drains them. The last free entry is kept for a reset mark, so when the
/// queue fills the mark goes in where the bytes start being lost: the
/// reader releases every key after the entries before the loss, never
/// before them (a press queued before a lost release would stay down).
pub struct Ps2Queue {
    buf: [[u8; ps2::ENTRY]; ps2::CAPACITY],
    head: usize,
    len: usize,
    /// Bytes dropped because the queue was full, not taken yet.
    dropped: u64,
    /// Cut (emptied when the screen changed hands, or entries lost on
    /// their way to the reader): the next read starts with a reset mark.
    cut: bool,
    /// The one process that drains the queue: the first to read it, until
    /// it is gone.
    reader: Option<u32>,
}

impl Default for Ps2Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Ps2Queue {
    pub const fn new() -> Self {
        Self {
            buf: [[0; ps2::ENTRY]; ps2::CAPACITY],
            head: 0,
            len: 0,
            dropped: 0,
            cut: false,
            reader: None,
        }
    }

    fn append(&mut self, entry: [u8; ps2::ENTRY]) {
        self.buf[(self.head + self.len) % ps2::CAPACITY] = entry;
        self.len += 1;
    }

    /// Queues one byte from port `tag`; whether the reader has something
    /// new (the byte, or the mark that took its place).
    pub fn push(&mut self, tag: u8, byte: u8) -> bool {
        if self.len + 1 < ps2::CAPACITY {
            self.append([tag, byte]);
            return true;
        }
        self.dropped += 1;
        if self.len + 1 == ps2::CAPACITY {
            self.append([ps2::TAG_RESET, 0]);
            return true;
        }
        false
    }

    /// Empties the queue; the next read starts with a reset mark.
    pub fn cut(&mut self) {
        self.head = 0;
        self.len = 0;
        self.cut = true;
    }

    /// Entries taken by `read` were not delivered: the reader starts over
    /// from a reset mark.
    pub fn lost(&mut self) {
        self.cut = true;
    }

    /// The entries waiting, a reset mark owed counted as one.
    pub fn pending(&self) -> usize {
        self.len + self.cut as usize
    }

    /// Process `pid` is gone: if it drained the queue, another may now.
    pub fn reader_gone(&mut self, pid: u32) {
        if self.reader == Some(pid) {
            self.reader = None;
        }
    }

    /// Copies whole entries into `out` for process `pid`: a reset mark
    /// first if the queue was cut, then the oldest entries. Returns the
    /// bytes written; `None` if another process drains the queue.
    pub fn read(&mut self, pid: u32, out: &mut [u8]) -> Option<usize> {
        match self.reader {
            Some(reader) if reader != pid => return None,
            _ => self.reader = Some(pid),
        }
        let mut n = 0;
        if self.cut && out.len() >= ps2::ENTRY {
            self.cut = false;
            out[..ps2::ENTRY].copy_from_slice(&[ps2::TAG_RESET, 0]);
            n = ps2::ENTRY;
        }
        while self.len > 0 && out.len() - n >= ps2::ENTRY {
            out[n..n + ps2::ENTRY].copy_from_slice(&self.buf[self.head]);
            self.head = (self.head + 1) % ps2::CAPACITY;
            self.len -= 1;
            n += ps2::ENTRY;
        }
        Some(n)
    }

    /// The bytes dropped since the last call.
    pub fn take_dropped(&mut self) -> u64 {
        core::mem::take(&mut self.dropped)
    }
}

/// Ctrl+Alt+Backspace (ADR 0074 phase B): the owner's way back to the text
/// console from a desktop, typed on any keyboard. The compositor leaves when
/// it sees it on its keyboards; the kernel stops whoever holds the screen
/// when it sees it on the PS/2 keyboard. Either Ctrl and either Alt count.
#[derive(Debug, Default, Clone, Copy)]
pub struct EscapeChord {
    /// Bits: left Ctrl, right Ctrl, left Alt, right Alt.
    held: u8,
}

impl EscapeChord {
    pub const fn new() -> Self {
        Self { held: 0 }
    }

    /// Follows one event; `true` on the Backspace press made with a Ctrl
    /// and an Alt held.
    pub fn feed(&mut self, ev: Event) -> bool {
        if ev.kind != EV_KEY {
            return false;
        }
        let bit = match ev.code {
            KEY_LEFTCTRL => 1,
            KEY_RIGHTCTRL => 2,
            KEY_LEFTALT => 4,
            KEY_RIGHTALT => 8,
            KEY_BACKSPACE => {
                return ev.value == 1 && self.held & 3 != 0 && self.held & 12 != 0;
            }
            _ => return false,
        };
        if ev.value == 0 {
            self.held &= !bit;
        } else {
            self.held |= bit;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    fn key(code: u16, value: u32) -> Event {
        Event {
            kind: EV_KEY,
            code,
            value,
        }
    }

    #[test]
    fn event_round_trip() {
        let e = Event {
            kind: EV_REL,
            code: REL_Y,
            value: (-5i32) as u32,
        };
        assert_eq!(Event::decode(&e.encode()), Some(e));
        assert!(Event::decode(&[0; 7]).is_none());
        assert_eq!(describe(&e), "rel y");
        assert_eq!(describe(&key(BTN_LEFT, 1)), "left button");
    }

    #[test]
    fn keyboard_translates_hola_enter() {
        let mut k = Keyboard::new();
        let mut out = alloc_free_buf();
        let mut n = 0;
        for code in [35u16, 24, 38, 30, KEY_ENTER] {
            for &c in k.feed(key(code, 1)).as_slice() {
                out[n] = c;
                n += 1;
            }
            assert!(k.feed(key(code, 0)).is_empty(), "release produces nothing");
        }
        assert_eq!(&out[..n], b"hola\n");
        // Editing keys: ANSI sequences; repeats produce nothing.
        assert_eq!(k.feed(key(KEY_UP, 1)).as_slice(), b"\x1b[A");
        assert_eq!(k.feed(key(KEY_DOWN, 1)).as_slice(), b"\x1b[B");
        assert_eq!(k.feed(key(KEY_LEFT, 1)).as_slice(), b"\x1b[D");
        assert_eq!(k.feed(key(KEY_DELETE, 1)).as_slice(), b"\x1b[3~");
        assert!(k.feed(key(KEY_UP, 2)).is_empty());
    }

    #[test]
    fn shift_caps_and_repeats() {
        let mut k = Keyboard::new();
        assert!(k.feed(key(KEY_LEFTSHIFT, 1)).is_empty());
        assert_eq!(k.feed(key(30, 1)).as_slice(), b"A");
        assert_eq!(k.feed(key(2, 1)).as_slice(), b"!");
        k.feed(key(KEY_LEFTSHIFT, 0));
        assert_eq!(k.feed(key(2, 1)).as_slice(), b"1");
        assert!(k.feed(key(KEY_CAPSLOCK, 1)).is_empty());
        assert_eq!(k.feed(key(30, 1)).as_slice(), b"A", "caps lock");
        assert_eq!(
            k.feed(key(2, 1)).as_slice(),
            b"1",
            "caps lock does not affect digits"
        );
        k.feed(key(KEY_LEFTSHIFT, 1));
        assert_eq!(
            k.feed(key(30, 1)).as_slice(),
            b"a",
            "shift inverts caps lock"
        );
        assert!(k.feed(key(30, 2)).is_empty(), "repeat ignored");
        assert!(k.feed(key(200, 1)).is_empty(), "code outside the table");
        assert!(k
            .feed(Event {
                kind: EV_REL,
                code: 0,
                value: 1
            })
            .is_empty());
    }

    fn alloc_free_buf() -> [u8; 16] {
        [0; 16]
    }

    #[test]
    fn spanish_layout_shift_altgr_and_dead_keys() {
        let mut k = Keyboard::with_layout(Layout::Es);
        let mut typed = |k: &mut Keyboard, code: u16| {
            let out = k.feed(key(code, 1));
            k.feed(key(code, 0));
            std::string::String::from_utf8(out.as_slice().to_vec()).unwrap()
        };
        assert_eq!(Layout::parse("es"), Some(Layout::Es));
        assert_eq!(Layout::parse("fr"), None);
        // `.` and, with shift, `:`; `7` and `/`; `-` and `_`; `<` and `>`.
        assert_eq!(typed(&mut k, KEY_DOT), ".");
        k.feed(key(KEY_LEFTSHIFT, 1));
        assert_eq!(typed(&mut k, KEY_DOT), ":");
        assert_eq!(typed(&mut k, 8), "/");
        assert_eq!(typed(&mut k, KEY_SLASH), "_");
        assert_eq!(typed(&mut k, KEY_102ND), ">");
        assert_eq!(typed(&mut k, 3), "\"");
        assert_eq!(typed(&mut k, 4), "·");
        assert_eq!(typed(&mut k, KEY_SEMICOLON), "Ñ");
        assert_eq!(typed(&mut k, KEY_EQUAL), "¿");
        k.feed(key(KEY_LEFTSHIFT, 0));
        assert_eq!(typed(&mut k, KEY_102ND), "<");
        assert_eq!(typed(&mut k, KEY_SLASH), "-");
        assert_eq!(typed(&mut k, KEY_MINUS), "'");
        assert_eq!(typed(&mut k, KEY_SEMICOLON), "ñ");
        assert_eq!(typed(&mut k, KEY_BACKSLASH), "ç");
        assert_eq!(typed(&mut k, KEY_GRAVE), "º");
        assert_eq!(typed(&mut k, KEY_EQUAL), "¡");
        // AltGr, as Linux's `es`: @ # [ ] { } \\ | ~ € ¬, and { } on 7 and 0.
        k.feed(key(KEY_RIGHTALT, 1));
        for (code, out) in [
            (3, "@"),
            (4, "#"),
            (KEY_LEFTBRACE, "["),
            (KEY_RIGHTBRACE, "]"),
            (KEY_APOSTROPHE, "{"),
            (KEY_BACKSLASH, "}"),
            (KEY_GRAVE, "\\"),
            (2, "|"),
            (5, "~"),
            (18, "€"),
            (7, "¬"),
            (8, "{"),
            (11, "}"),
            (30, "æ"),
        ] {
            assert_eq!(typed(&mut k, code), out, "AltGr+{code}");
        }
        k.feed(key(KEY_RIGHTALT, 0));
        assert_eq!(typed(&mut k, 30), "a");
        // Caps Lock reaches ñ and ç, as letters, and æ with AltGr; not º.
        typed(&mut k, KEY_CAPSLOCK);
        assert_eq!(typed(&mut k, KEY_SEMICOLON), "Ñ");
        assert_eq!(typed(&mut k, KEY_BACKSLASH), "Ç");
        assert_eq!(typed(&mut k, KEY_GRAVE), "º");
        k.feed(key(KEY_RIGHTALT, 1));
        assert_eq!(typed(&mut k, 30), "Æ");
        k.feed(key(KEY_RIGHTALT, 0));
        typed(&mut k, KEY_CAPSLOCK);
        // Ctrl with ñ gives nothing; with a letter, its control character.
        k.feed(key(KEY_LEFTCTRL, 1));
        assert_eq!(typed(&mut k, KEY_SEMICOLON), "");
        assert_eq!(typed(&mut k, 30), "\x01");
        k.feed(key(KEY_LEFTCTRL, 0));
        // The US keyboard does not change.
        let mut us = Keyboard::new();
        us.feed(key(KEY_LEFTSHIFT, 1));
        assert_eq!(us.feed(key(KEY_DOT, 1)).as_slice(), b">");
        us.set_layout(Layout::Es);
        assert_eq!(us.feed(key(KEY_DOT, 1)).as_slice(), b":");
    }

    #[test]
    fn spanish_dead_keys_combine_as_linux_does() {
        let mut k = Keyboard::with_layout(Layout::Es);
        let mut typed = |k: &mut Keyboard, codes: &[(u16, bool)]| {
            let mut out = std::vec::Vec::new();
            for &(code, shift) in codes {
                if shift {
                    k.feed(key(KEY_LEFTSHIFT, 1));
                }
                out.extend_from_slice(k.feed(key(code, 1)).as_slice());
                k.feed(key(code, 0));
                if shift {
                    k.feed(key(KEY_LEFTSHIFT, 0));
                }
            }
            std::string::String::from_utf8(out).unwrap()
        };
        let acute = (KEY_APOSTROPHE, false);
        let diaeresis = (KEY_APOSTROPHE, true);
        let grave = (KEY_LEFTBRACE, false);
        let circumflex = (KEY_LEFTBRACE, true);
        let (a, e, u, n, x) = (
            (30, false),
            (18, false),
            (22, false),
            (49, false),
            (45, false),
        );
        let space = (KEY_SPACE, false);
        // The dead key gives nothing until the next key.
        assert_eq!(typed(&mut k, &[acute]), "");
        assert_eq!(typed(&mut k, &[a]), "á");
        assert_eq!(typed(&mut k, &[acute, (30, true)]), "Á");
        assert_eq!(typed(&mut k, &[diaeresis, u]), "ü");
        assert_eq!(typed(&mut k, &[grave, e]), "è");
        assert_eq!(typed(&mut k, &[circumflex, e]), "ê");
        // With a space, the `Compose` file's character; twice, the accent.
        assert_eq!(typed(&mut k, &[acute, space]), "'");
        assert_eq!(typed(&mut k, &[grave, space]), "`");
        assert_eq!(typed(&mut k, &[circumflex, space]), "^");
        assert_eq!(typed(&mut k, &[diaeresis, space]), "\"");
        assert_eq!(typed(&mut k, &[acute, acute]), "´");
        // With a key it does not combine with: the accent, then the key.
        assert_eq!(typed(&mut k, &[acute, x]), "´x");
        assert_eq!(typed(&mut k, &[acute, n]), "´n");
        // Another dead key: the first one's accent, and the second waits.
        assert_eq!(typed(&mut k, &[acute, grave, a]), "´à");
        // AltGr+ñ is the dead tilde; AltGr+¡ the dead cedilla.
        k.feed(key(KEY_RIGHTALT, 1));
        assert_eq!(typed(&mut k, &[(KEY_SEMICOLON, false)]), "");
        k.feed(key(KEY_RIGHTALT, 0));
        assert_eq!(typed(&mut k, &[n]), "ñ");
        k.feed(key(KEY_RIGHTALT, 1));
        typed(&mut k, &[(KEY_EQUAL, false)]);
        k.feed(key(KEY_RIGHTALT, 0));
        assert_eq!(typed(&mut k, &[(46, false)]), "ç");
        // Enter, an arrow or a lost release end it unused.
        assert_eq!(typed(&mut k, &[acute, (KEY_ENTER, false)]), "\n");
        assert_eq!(typed(&mut k, &[a]), "a");
        typed(&mut k, &[acute]);
        assert_eq!(k.feed(key(KEY_LEFT, 1)).as_slice(), b"\x1b[D");
        assert_eq!(typed(&mut k, &[a]), "a");
        typed(&mut k, &[acute]);
        k.release_modifiers();
        assert_eq!(typed(&mut k, &[a]), "a");
        // So does a key that types nothing: F1 (59) or Page Down (109).
        assert_eq!(typed(&mut k, &[acute, (59, false), a]), "a");
        assert_eq!(typed(&mut k, &[acute, (109, false), a]), "a");
        // The longest output fits: an accent and the euro sign.
        typed(&mut k, &[acute]);
        k.feed(key(KEY_RIGHTALT, 1));
        assert_eq!(typed(&mut k, &[e]), "´€");
    }

    #[test]
    fn scancode_set1_to_evdev_and_ascii() {
        let mut d = ScancodeSet1::new();
        let mut k = Keyboard::new();
        // 'h' 'o' 'l' 'a' Enter, with their releases.
        let mut out = [0u8; 8];
        let mut n = 0;
        for byte in [0x23u8, 0xA3, 0x18, 0x98, 0x26, 0xA6, 0x1E, 0x9E, 0x1C, 0x9C] {
            if let Some(ev) = d.feed(byte) {
                for &c in k.feed(ev).as_slice() {
                    out[n] = c;
                    n += 1;
                }
            }
        }
        assert_eq!(&out[..n], b"hola\n");
        // Left shift (0x2A) + 'a' = 'A'.
        assert_eq!(
            d.feed(0x2A),
            Some(Event {
                kind: EV_KEY,
                code: KEY_LEFTSHIFT,
                value: 1
            })
        );
        k.feed(d.feed(0x2A).unwrap());
        assert_eq!(k.feed(d.feed(0x1E).unwrap()).as_slice(), b"A");
        k.feed(d.feed(0xAA).unwrap());
        // Extended: E0 48 = up arrow; E0 53 = delete; E0 with an unknown code is ignored.
        assert_eq!(d.feed(0xE0), None);
        assert_eq!(
            d.feed(0x48),
            Some(Event {
                kind: EV_KEY,
                code: KEY_UP,
                value: 1
            })
        );
        assert_eq!(d.feed(0xE0), None);
        assert_eq!(
            d.feed(0xD3),
            Some(Event {
                kind: EV_KEY,
                code: KEY_DELETE,
                value: 0
            })
        );
        assert_eq!(d.feed(0xE0), None);
        assert_eq!(d.feed(0x7F), None);
        // Pause (E1 1D 45) is discarded entirely; what follows decodes normally.
        assert_eq!(d.feed(0xE1), None);
        assert_eq!(d.feed(0x1D), None);
        assert_eq!(d.feed(0x45), None);
        assert_eq!(
            d.feed(0x39),
            Some(Event {
                kind: EV_KEY,
                code: 57,
                value: 1
            })
        );
        // Bytes outside the set (ACK 0xFA, 0x00) produce nothing.
        assert_eq!(d.feed(0xFA), None);
        assert_eq!(d.feed(0x00), None);
    }

    #[test]
    fn hid_boot_keyboard_reports_to_events_and_text() {
        let mut kbd = HidBootKeyboard::new();
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        // 'h' pressed (usage 0x0B), then 'h' released and 'o' pressed, then all released.
        let n = kbd.feed(&[0, 0, 0x0B, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(n, 2);
        assert_eq!(
            out[0],
            Event {
                kind: EV_KEY,
                code: 35,
                value: 1
            }
        );
        assert_eq!(out[1].kind, EV_SYN);
        let n = kbd.feed(&[0, 0, 0x12, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(
            &out[..n],
            &[
                Event {
                    kind: EV_KEY,
                    code: 35,
                    value: 0
                },
                Event {
                    kind: EV_KEY,
                    code: 24,
                    value: 1
                },
                Event {
                    kind: EV_SYN,
                    code: 0,
                    value: 0
                },
            ]
        );
        // Left shift + 'a' => 'A' through the US keyboard.
        let mut text = Keyboard::new();
        let n = kbd.feed(&[0x02, 0, 0x04, 0, 0, 0, 0, 0], &mut out);
        let mut chars = [0u8; 4];
        let mut c = 0;
        for ev in &out[..n] {
            for &ch in text.feed(*ev).as_slice() {
                chars[c] = ch;
                c += 1;
            }
        }
        assert_eq!(&chars[..c], b"A");
        assert_eq!(
            out[0],
            Event {
                kind: EV_KEY,
                code: KEY_LEFTSHIFT,
                value: 1
            }
        );
        // Empty report: releases of shift and 'a' ('o' was already released by
        // not being in the previous report).
        let n = kbd.feed(&[0; 8], &mut out);
        assert_eq!(n, 3);
        assert!(out[..2].iter().all(|e| e.kind == EV_KEY && e.value == 0));
        // No changes: nothing. Error report (0x01 x6): nothing.
        assert_eq!(kbd.feed(&[0; 8], &mut out), 0);
        assert_eq!(kbd.feed(&[0, 0, 1, 1, 1, 1, 1, 1], &mut out), 0);
        assert_eq!(kbd.feed(&[0; 7], &mut out), 0, "short report");
        assert_eq!(hid_usage_to_key(0x28), KEY_ENTER);
        assert_eq!(hid_usage_to_key(0x27), 11);
        assert_eq!(hid_usage_to_key(0x45), 88);
        assert_eq!(hid_usage_to_key(0xFF), 0);
    }

    #[test]
    fn hid_boot_keyboard_worst_case_transition_loses_nothing() {
        // Audit 4836023, finding 3: eight modifiers and six keys change at
        // once: 8 + 6 + 6 key events plus EV_SYN.
        let mut kbd = HidBootKeyboard::new();
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        let n = kbd.feed(&[0xFF, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09], &mut out);
        assert_eq!(n, 8 + 6 + 1);
        let n = kbd.feed(&[0x00, 0, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F], &mut out);
        assert_eq!(n, 21);
        let releases = out[..n]
            .iter()
            .filter(|e| e.kind == EV_KEY && e.value == 0)
            .count();
        let presses = out[..n]
            .iter()
            .filter(|e| e.kind == EV_KEY && e.value == 1)
            .count();
        assert_eq!((releases, presses), (8 + 6, 6));
        assert_eq!(out[n - 1].kind, EV_SYN);
        // All new presses are there: g h i j k l.
        for usage in 0x0A..=0x0F {
            let code = hid_usage_to_key(usage);
            assert!(out[..n]
                .iter()
                .any(|e| e.kind == EV_KEY && e.code == code && e.value == 1));
        }
        // The same report again produces nothing; the empty report releases all six.
        assert_eq!(
            kbd.feed(&[0x00, 0, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F], &mut out),
            0
        );
        let n = kbd.feed(&[0; 8], &mut out);
        assert_eq!(n, 7);
        assert!(out[..6].iter().all(|e| e.value == 0));
    }

    #[test]
    fn hid_boot_mouse_reports_to_events() {
        let mut m = HidBootMouse::new();
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        let n = m.feed(&[0x01, 10, (-5i8) as u8], &mut out);
        assert_eq!(
            &out[..n],
            &[
                Event {
                    kind: EV_KEY,
                    code: BTN_LEFT,
                    value: 1
                },
                Event {
                    kind: EV_REL,
                    code: REL_X,
                    value: 10
                },
                Event {
                    kind: EV_REL,
                    code: REL_Y,
                    value: (-5i32) as u32
                },
                Event {
                    kind: EV_SYN,
                    code: 0,
                    value: 0
                },
            ]
        );
        let n = m.feed(&[0x00, 0, 0, 1], &mut out);
        assert_eq!(
            &out[..n],
            &[
                Event {
                    kind: EV_KEY,
                    code: BTN_LEFT,
                    value: 0
                },
                Event {
                    kind: EV_REL,
                    code: REL_WHEEL,
                    value: 1
                },
                Event {
                    kind: EV_SYN,
                    code: 0,
                    value: 0
                },
            ]
        );
        assert_eq!(m.feed(&[0, 0, 0], &mut out), 0);
        assert_eq!(m.feed(&[0, 0], &mut out), 0);
    }

    #[test]
    fn released_modifiers_type_plain_letters() {
        let key = |code, value| Event {
            kind: EV_KEY,
            code,
            value,
        };
        let mut k = Keyboard::new();
        k.feed(key(KEY_LEFTCTRL, 1));
        k.feed(key(KEY_LEFTSHIFT, 1));
        k.release_modifiers();
        assert_eq!(k.feed(key(31, 1)).as_slice(), b"s");
    }

    #[test]
    fn ctrl_with_a_letter_gives_its_control_character() {
        let key = |code, value| Event {
            kind: EV_KEY,
            code,
            value,
        };
        const KEY_S: u16 = 31;
        const KEY_U: u16 = 22;
        const KEY_1: u16 = 2;
        for layout in [Layout::Us, Layout::Es] {
            let mut k = Keyboard::with_layout(layout);
            assert!(k.feed(key(KEY_LEFTCTRL, 1)).is_empty());
            assert_eq!(k.feed(key(KEY_S, 1)).as_slice(), &[0x13]);
            assert!(k.feed(key(KEY_1, 1)).is_empty(), "not a letter");
            assert!(k.feed(key(KEY_RIGHTCTRL, 1)).is_empty());
            assert!(k.feed(key(KEY_LEFTCTRL, 0)).is_empty());
            assert_eq!(k.feed(key(KEY_U, 1)).as_slice(), &[0x15], "the other Ctrl");
            assert!(k.feed(key(KEY_RIGHTCTRL, 0)).is_empty());
            assert_eq!(k.feed(key(KEY_S, 1)).as_slice(), b"s");
        }
    }

    #[test]
    fn escape_chord_needs_a_ctrl_and_an_alt_held() {
        let mut c = EscapeChord::new();
        assert!(!c.feed(key(KEY_BACKSPACE, 1)));
        assert!(!c.feed(key(KEY_LEFTCTRL, 1)));
        assert!(!c.feed(key(KEY_BACKSPACE, 1)), "Ctrl alone");
        assert!(!c.feed(key(KEY_RIGHTALT, 1)));
        assert!(!c.feed(key(KEY_BACKSPACE, 0)), "a release");
        assert!(!c.feed(key(KEY_BACKSPACE, 2)), "a repeat");
        assert!(c.feed(key(KEY_BACKSPACE, 1)));
        assert!(!c.feed(key(KEY_LEFTCTRL, 0)));
        assert!(!c.feed(key(KEY_BACKSPACE, 1)), "Ctrl released");
        assert!(!c.feed(key(KEY_RIGHTCTRL, 1)));
        assert!(c.feed(key(KEY_BACKSPACE, 1)), "the other Ctrl");
        assert!(!c.feed(Event {
            kind: 2,
            code: KEY_BACKSPACE,
            value: 1
        }));
    }

    fn ev(kind: u16, code: u16, value: i32) -> Event {
        Event {
            kind,
            code,
            value: value as u32,
        }
    }

    #[test]
    fn ps2_mouse_packets_to_events() {
        let mut m = Ps2Mouse::new(false);
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        // Left button, X +10, Y +5 (up in PS/2, so -5 in evdev).
        assert_eq!(m.feed(0x09, &mut out), 0);
        assert_eq!(m.feed(10, &mut out), 0);
        let n = m.feed(5, &mut out);
        assert_eq!(
            &out[..n],
            &[
                ev(EV_KEY, BTN_LEFT, 1),
                ev(EV_REL, REL_X, 10),
                ev(EV_REL, REL_Y, -5),
                ev(EV_SYN, 0, 0)
            ]
        );
        // The 9th bits: X -3 (0x1FD), Y -200 (0x138) is down, +200 in evdev.
        m.feed(0x08 | 0x10 | 0x20, &mut out);
        m.feed(0xFD, &mut out);
        let n = m.feed(0x38, &mut out);
        assert_eq!(
            &out[..n],
            &[
                ev(EV_KEY, BTN_LEFT, 0),
                ev(EV_REL, REL_X, -3),
                ev(EV_REL, REL_Y, 200),
                ev(EV_SYN, 0, 0)
            ]
        );
    }

    #[test]
    fn ps2_mouse_resyncs_and_drops_overflow() {
        let mut m = Ps2Mouse::new(false);
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        // A stray byte without bit 3 is skipped until a packet starts.
        assert_eq!(m.feed(0x05, &mut out), 0);
        m.feed(0x08, &mut out);
        m.feed(1, &mut out);
        assert_eq!(m.feed(0, &mut out), 2, "X only, then EV_SYN");
        // X overflow: the motion is dropped, the right button kept.
        m.feed(0x08 | 0x40 | 0x02, &mut out);
        m.feed(0xFF, &mut out);
        let n = m.feed(0x10, &mut out);
        assert_eq!(&out[..n], &[ev(EV_KEY, BTN_RIGHT, 1), ev(EV_SYN, 0, 0)]);
        // A cut stream: the half packet is forgotten and the button released.
        m.feed(0x08, &mut out);
        let n = m.release_all(&mut out);
        assert_eq!(&out[..n], &[ev(EV_KEY, BTN_RIGHT, 0), ev(EV_SYN, 0, 0)]);
        m.feed(0x08 | 0x04, &mut out);
        m.feed(0, &mut out);
        let n = m.feed(0, &mut out);
        assert_eq!(&out[..n], &[ev(EV_KEY, BTN_MIDDLE, 1), ev(EV_SYN, 0, 0)]);
        assert_eq!(m.release_all(&mut out), 2);
        assert_eq!(m.release_all(&mut out), 0, "nothing left held");
    }

    #[test]
    fn ps2_mouse_wheel() {
        let mut m = Ps2Mouse::new(true);
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        // Z +1 (towards the user) is a wheel step down: -1 in evdev.
        for b in [0x08, 0, 0] {
            assert_eq!(m.feed(b, &mut out), 0, "4-byte packets");
        }
        let n = m.feed(0x01, &mut out);
        assert_eq!(&out[..n], &[ev(EV_REL, REL_WHEEL, -1), ev(EV_SYN, 0, 0)]);
        // Z -2 is 0xE in the low nibble; the high bits (buttons 4 and 5)
        // are not the wheel's.
        for b in [0x08, 0, 0] {
            m.feed(b, &mut out);
        }
        let n = m.feed(0x3E, &mut out);
        assert_eq!(&out[..n], &[ev(EV_REL, REL_WHEEL, 2), ev(EV_SYN, 0, 0)]);
        m.resync();
        for b in [0x08, 4, 0] {
            m.feed(b, &mut out);
        }
        assert_eq!(m.feed(0, &mut out), 2, "in step again after resync");
    }

    #[test]
    fn keys_down_releases_what_is_held() {
        let mut k = KeysDown::new();
        for e in [
            ev(EV_KEY, KEY_LEFTCTRL, 1),
            ev(EV_KEY, KEY_SPACE, 1),
            ev(EV_KEY, KEY_SPACE, 0),
            ev(EV_KEY, KEY_RIGHTMETA, 1),
            ev(EV_KEY, KEY_RIGHTMETA, 1),
            ev(EV_REL, REL_X, 1),
            ev(EV_KEY, BTN_LEFT, 1),
        ] {
            k.follow(&e);
        }
        let mut released = [Event::default(); 4];
        let mut n = 0;
        k.release_all(|e| {
            released[n] = e;
            n += 1;
        });
        assert_eq!(
            released[..n],
            [ev(EV_KEY, KEY_LEFTCTRL, 0), ev(EV_KEY, KEY_RIGHTMETA, 0)]
        );
        k.release_all(|_| panic!("nothing left held"));
    }

    fn entries(q: &mut Ps2Queue, pid: u32) -> ([[u8; 2]; 8], usize) {
        let mut buf = [0u8; 16];
        let n = q.read(pid, &mut buf).expect("the reader");
        let mut out = [[0u8; 2]; 8];
        for (o, e) in out.iter_mut().zip(buf[..n].as_chunks::<2>().0) {
            *o = *e;
        }
        (out, n / 2)
    }

    #[test]
    fn layout_codes_round_trip() {
        for l in [Layout::Us, Layout::Es] {
            assert_eq!(Layout::from_code(l.code()), l);
        }
        assert_eq!(Layout::from_code(9), Layout::Us);
    }

    #[test]
    fn ps2_queue_marks_where_bytes_are_lost() {
        let mut q = Ps2Queue::new();
        for i in 0..ps2::CAPACITY - 1 {
            assert!(q.push(ps2::TAG_KEYBOARD, i as u8));
        }
        // The last entry becomes the mark; bytes after it are dropped.
        assert!(q.push(ps2::TAG_KEYBOARD, 0xAA), "the mark is new");
        assert!(!q.push(ps2::TAG_AUX, 0xBB));
        assert_eq!(q.pending(), ps2::CAPACITY);
        assert_eq!(q.take_dropped(), 2);
        let mut buf = [0u8; ps2::CAPACITY * ps2::ENTRY];
        let n = q.read(7, &mut buf).unwrap();
        assert_eq!(n, ps2::CAPACITY * ps2::ENTRY);
        assert_eq!(&buf[n - 4..], &[ps2::TAG_KEYBOARD, 254, ps2::TAG_RESET, 0]);
        // Room again: bytes queue normally, no mark owed.
        assert!(q.push(ps2::TAG_AUX, 1));
        let (e, n) = entries(&mut q, 7);
        assert_eq!(&e[..n], &[[ps2::TAG_AUX, 1]]);
    }

    #[test]
    fn ps2_queue_cut_and_one_reader() {
        let mut q = Ps2Queue::new();
        q.push(ps2::TAG_KEYBOARD, 0x1E);
        q.cut();
        assert_eq!(q.pending(), 1, "the mark owed");
        q.push(ps2::TAG_KEYBOARD, 0x9E);
        let (e, n) = entries(&mut q, 3);
        assert_eq!(&e[..n], &[[ps2::TAG_RESET, 0], [ps2::TAG_KEYBOARD, 0x9E]]);
        // Process 3 drains it: another is refused until 3 is gone.
        assert!(q.read(4, &mut [0u8; 4]).is_none());
        q.reader_gone(4);
        assert!(q.read(4, &mut [0u8; 4]).is_none(), "4 never was the reader");
        q.reader_gone(3);
        q.lost();
        let (e, n) = entries(&mut q, 4);
        assert_eq!(&e[..n], &[[ps2::TAG_RESET, 0]]);
        // A buffer too small for an entry takes nothing.
        q.push(ps2::TAG_AUX, 9);
        assert_eq!(q.read(4, &mut [0u8; 1]), Some(0));
        assert_eq!(q.pending(), 1);
    }
}
