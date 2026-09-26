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
}
