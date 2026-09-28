//! Eventos de entrada (Fase 25): el formato de virtio-input coincide con el
//! de evdev de Linux (`type`, `code`, `value`), así que la misma librería
//! servirá para USB HID cuando llegue. Incluye un teclado US mínimo que
//! convierte pulsaciones en caracteres.

#![no_std]

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

pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_CAPSLOCK: u16 = 58;

/// Un evento tal como lo entrega el dispositivo (8 bytes little endian).
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

/// Teclado US: seguimiento de mayúsculas y traducción de pulsaciones.
#[derive(Debug, Default, Clone, Copy)]
pub struct Keyboard {
    shift: bool,
    caps: bool,
}

/// Fila de códigos evdev 1..=57 (esc..espacio) sin y con mayúsculas.
const PLAIN: &[u8; 58] =
    b"\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 58] =
    b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            shift: false,
            caps: false,
        }
    }

    /// Procesa un evento; devuelve el carácter producido, si lo hay.
    pub fn feed(&mut self, ev: Event) -> Option<u8> {
        if ev.kind != EV_KEY {
            return None;
        }
        match ev.code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                self.shift = ev.value != 0;
                return None;
            }
            KEY_CAPSLOCK => {
                if ev.value == 1 {
                    self.caps = !self.caps;
                }
                return None;
            }
            _ => {}
        }
        if ev.value != 1 {
            return None; // solo pulsaciones (no repeticiones ni sueltas)
        }
        let i = ev.code as usize;
        if i >= PLAIN.len() {
            return None;
        }
        let (plain, shifted) = (PLAIN[i], SHIFTED[i]);
        if plain == 0 {
            return None;
        }
        let letter = plain.is_ascii_lowercase();
        let upper = self.shift ^ (self.caps && letter);
        Some(if upper { shifted } else { plain })
    }
}

/// Nombre legible de un evento no textual (para trazas).
pub fn describe(ev: &Event) -> &'static str {
    match (ev.kind, ev.code) {
        (EV_SYN, _) => "syn",
        (EV_REL, REL_X) => "rel x",
        (EV_REL, REL_Y) => "rel y",
        (EV_REL, REL_WHEEL) => "rueda",
        (EV_REL, _) => "rel",
        (EV_ABS, _) => "abs",
        (EV_KEY, BTN_LEFT) => "boton izquierdo",
        (EV_KEY, BTN_RIGHT) => "boton derecho",
        (EV_KEY, BTN_MIDDLE) => "boton central",
        (EV_KEY, _) => "tecla",
        _ => "?",
    }
}

/// Decodificador de scancodes del conjunto 1 (teclado PS/2 con traducción
/// del i8042) a eventos evdev: los códigos 0x01..=0x58 coinciden con los
/// keycodes de Linux; el prefijo `E0` introduce las teclas extendidas y
/// `E1` (Pausa) se descarta con sus dos bytes.
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

    /// Procesa un byte del teclado; devuelve el evento completo, si lo hay.
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

/// Eventos que puede producir un informe HID de arranque: 8 cambios de
/// modificador, 6 sueltas, 6 pulsaciones y `EV_SYN` (21); se reservan 24.
pub const MAX_REPORT_EVENTS: usize = 24;

/// Teclado HID de arranque (USB): informe de 8 bytes (modificadores,
/// reservado, hasta 6 usages pulsados). Traduce cada informe a eventos evdev
/// comparándolo con el anterior. `out` tiene sitio para el peor caso, así
/// que ningún cambio se pierde ni el estado avanza sin haberlo entregado.
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

/// Keycode evdev de un usage de la página de teclado HID (0 si no se conoce).
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

/// Keycodes de los ocho bits de modificadores del informe de arranque.
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

    /// Procesa un informe y escribe en `out` los eventos (sueltas primero,
    /// luego pulsaciones, y `EV_SYN` al final). Devuelve cuántos.
    pub fn feed(&mut self, report: &[u8], out: &mut [Event; MAX_REPORT_EVENTS]) -> usize {
        if report.len() < 8 {
            return 0;
        }
        let mods = report[0];
        let mut keys = [0u8; 6];
        keys.copy_from_slice(&report[2..8]);
        // Un informe de error (0x01 en todas las posiciones) se ignora.
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

/// Ratón HID de arranque: informe de 3 o 4 bytes (botones, dx, dy, rueda).
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
        let mut n = 0;
        let mut push = |ev: Event, n: &mut usize| {
            if *n < out.len() {
                out[*n] = ev;
                *n += 1;
            }
        };
        let buttons = report[0];
        for (bit, code) in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE].iter().enumerate() {
            let was = self.buttons & (1 << bit) != 0;
            let is = buttons & (1 << bit) != 0;
            if was != is {
                push(
                    Event {
                        kind: EV_KEY,
                        code: *code,
                        value: is as u32,
                    },
                    &mut n,
                );
            }
        }
        self.buttons = buttons;
        let dx = report[1] as i8 as i32;
        let dy = report[2] as i8 as i32;
        if dx != 0 {
            push(
                Event {
                    kind: EV_REL,
                    code: REL_X,
                    value: dx as u32,
                },
                &mut n,
            );
        }
        if dy != 0 {
            push(
                Event {
                    kind: EV_REL,
                    code: REL_Y,
                    value: dy as u32,
                },
                &mut n,
            );
        }
        if report.len() >= 4 && report[3] != 0 {
            push(
                Event {
                    kind: EV_REL,
                    code: REL_WHEEL,
                    value: report[3] as i8 as i32 as u32,
                },
                &mut n,
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(describe(&key(BTN_LEFT, 1)), "boton izquierdo");
    }

    #[test]
    fn keyboard_translates_hola_enter() {
        let mut k = Keyboard::new();
        let mut out = alloc_free_buf();
        let mut n = 0;
        for code in [35u16, 24, 38, 30, KEY_ENTER] {
            if let Some(c) = k.feed(key(code, 1)) {
                out[n] = c;
                n += 1;
            }
            assert_eq!(k.feed(key(code, 0)), None, "soltar no produce nada");
        }
        assert_eq!(&out[..n], b"hola\n");
    }

    #[test]
    fn shift_caps_and_repeats() {
        let mut k = Keyboard::new();
        assert_eq!(k.feed(key(KEY_LEFTSHIFT, 1)), None);
        assert_eq!(k.feed(key(30, 1)), Some(b'A'));
        assert_eq!(k.feed(key(2, 1)), Some(b'!'));
        k.feed(key(KEY_LEFTSHIFT, 0));
        assert_eq!(k.feed(key(2, 1)), Some(b'1'));
        assert_eq!(k.feed(key(KEY_CAPSLOCK, 1)), None);
        assert_eq!(k.feed(key(30, 1)), Some(b'A'), "bloq mayus");
        assert_eq!(
            k.feed(key(2, 1)),
            Some(b'1'),
            "bloq mayus no afecta a digitos"
        );
        k.feed(key(KEY_LEFTSHIFT, 1));
        assert_eq!(k.feed(key(30, 1)), Some(b'a'), "shift invierte bloq mayus");
        assert_eq!(k.feed(key(30, 2)), None, "repeticion ignorada");
        assert_eq!(k.feed(key(200, 1)), None, "codigo fuera de tabla");
        assert_eq!(
            k.feed(Event {
                kind: EV_REL,
                code: 0,
                value: 1
            }),
            None
        );
    }

    fn alloc_free_buf() -> [u8; 16] {
        [0; 16]
    }

    #[test]
    fn scancode_set1_to_evdev_and_ascii() {
        let mut d = ScancodeSet1::new();
        let mut k = Keyboard::new();
        // 'h' 'o' 'l' 'a' Enter, con sus sueltas.
        let mut out = [0u8; 8];
        let mut n = 0;
        for byte in [0x23u8, 0xA3, 0x18, 0x98, 0x26, 0xA6, 0x1E, 0x9E, 0x1C, 0x9C] {
            if let Some(ev) = d.feed(byte) {
                if let Some(c) = k.feed(ev) {
                    out[n] = c;
                    n += 1;
                }
            }
        }
        assert_eq!(&out[..n], b"hola\n");
        // Shift izquierdo (0x2A) + 'a' = 'A'.
        assert_eq!(
            d.feed(0x2A),
            Some(Event {
                kind: EV_KEY,
                code: KEY_LEFTSHIFT,
                value: 1
            })
        );
        k.feed(d.feed(0x2A).unwrap());
        assert_eq!(k.feed(d.feed(0x1E).unwrap()), Some(b'A'));
        k.feed(d.feed(0xAA).unwrap());
        // Extendidas: E0 48 = flecha arriba; E0 53 = suprimir; E0 con codigo desconocido se ignora.
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
        // Pausa (E1 1D 45) se descarta entera; lo siguiente se decodifica normal.
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
        // Bytes fuera del conjunto (ACK 0xFA, 0x00) no producen nada.
        assert_eq!(d.feed(0xFA), None);
        assert_eq!(d.feed(0x00), None);
    }

    #[test]
    fn hid_boot_keyboard_reports_to_events_and_text() {
        let mut kbd = HidBootKeyboard::new();
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        // 'h' pulsada (usage 0x0B), luego 'h' suelta y 'o' pulsada, luego todo suelto.
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
        // Shift izquierdo + 'a' => 'A' pasando por el teclado US.
        let mut text = Keyboard::new();
        let n = kbd.feed(&[0x02, 0, 0x04, 0, 0, 0, 0, 0], &mut out);
        let mut chars = [0u8; 4];
        let mut c = 0;
        for ev in &out[..n] {
            if let Some(ch) = text.feed(*ev) {
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
        // Informe vacio: sueltas de shift y de 'a' ('o' ya se solto al no
        // venir en el informe anterior).
        let n = kbd.feed(&[0; 8], &mut out);
        assert_eq!(n, 3);
        assert!(out[..2].iter().all(|e| e.kind == EV_KEY && e.value == 0));
        // Sin cambios: nada. Informe de error (0x01 x6): nada.
        assert_eq!(kbd.feed(&[0; 8], &mut out), 0);
        assert_eq!(kbd.feed(&[0, 0, 1, 1, 1, 1, 1, 1], &mut out), 0);
        assert_eq!(kbd.feed(&[0; 7], &mut out), 0, "informe corto");
        assert_eq!(hid_usage_to_key(0x28), KEY_ENTER);
        assert_eq!(hid_usage_to_key(0x27), 11);
        assert_eq!(hid_usage_to_key(0x45), 88);
        assert_eq!(hid_usage_to_key(0xFF), 0);
    }

    #[test]
    fn hid_boot_keyboard_worst_case_transition_loses_nothing() {
        // Auditoria 4836023, hallazgo 3: ocho modificadores y seis teclas
        // cambian a la vez: 8 + 6 + 6 eventos de tecla mas EV_SYN.
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
        // Todas las pulsaciones nuevas estan: g h i j k l.
        for usage in 0x0A..=0x0F {
            let code = hid_usage_to_key(usage);
            assert!(out[..n]
                .iter()
                .any(|e| e.kind == EV_KEY && e.code == code && e.value == 1));
        }
        // El mismo informe otra vez no produce nada; el informe vacio suelta las seis.
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
}
