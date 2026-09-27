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
}
