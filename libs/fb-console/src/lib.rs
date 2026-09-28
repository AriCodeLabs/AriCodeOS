//! Consola de texto sobre un framebuffer lineal de 32 bpp.
//!
//! `no_std`, sin `alloc`: la rejilla de caracteres vive dentro de la propia
//! `Console` (tamaño máximo fijo, unas 32 KiB). Por eso la consola se
//! construye vacía y constante (`Console::empty`) en un `static` y se le
//! acopla la superficie después (`attach`): devolverla por valor pondría la
//! rejilla en la pila, que en el arranque del kernel es pequeña. El acceso a
//! los píxeles se abstrae en [`Surface`] para que el kernel escriba MMIO con
//! `write_volatile` y los tests de host usen un `Vec<u32>`.
//!
//! El scroll no lee del framebuffer (leer VRAM es lento): se conserva una
//! copia de los caracteres y se vuelve a pintar la pantalla.

#![no_std]

pub mod font;

use font::{FIRST_CHAR, GLYPHS, GLYPH_HEIGHT, GLYPH_WIDTH, LAST_CHAR};

/// Color RGB de 8 bits por canal; la `Surface` lo codifica a su formato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Destino de píxeles. Coordenadas en píxeles, origen arriba a la izquierda.
pub trait Surface {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn put(&mut self, x: usize, y: usize, color: Rgb);
    /// Rellena `rows` filas de píxeles completas desde `y`.
    fn fill_rows(&mut self, y: usize, rows: usize, color: Rgb) {
        for yy in y..(y + rows).min(self.height()) {
            for x in 0..self.width() {
                self.put(x, yy, color);
            }
        }
    }
}

/// Máximo de celdas de texto: 240 columnas x 135 filas cubre 1920x2160.
pub const MAX_CELLS: usize = 240 * 135;

pub struct Console<S: Surface> {
    /// `None` hasta `attach`: sin pantalla, la consola no dibuja nada.
    surface: Option<S>,
    cols: usize,
    rows: usize,
    cursor_col: usize,
    cursor_row: usize,
    fg: Rgb,
    bg: Rgb,
    cells: [u8; MAX_CELLS],
}

impl<S: Surface> Console<S> {
    pub const DEFAULT_FG: Rgb = Rgb(0xD8, 0xDE, 0xE9);
    pub const DEFAULT_BG: Rgb = Rgb(0x10, 0x12, 0x18);

    /// Consola sin pantalla, construible en un `static`.
    pub const fn empty() -> Self {
        Self {
            surface: None,
            cols: 0,
            rows: 0,
            cursor_col: 0,
            cursor_row: 0,
            fg: Self::DEFAULT_FG,
            bg: Self::DEFAULT_BG,
            cells: [b' '; MAX_CELLS],
        }
    }

    /// Acopla la pantalla y la limpia. Se hace sobre la consola ya alojada
    /// (en un `static`), sin mover la rejilla.
    pub fn attach(&mut self, surface: S) {
        let cols = (surface.width() / GLYPH_WIDTH).max(1);
        let rows = (surface.height() / GLYPH_HEIGHT).max(1);
        // Si la pantalla es mayor que MAX_CELLS se recortan filas, no columnas.
        self.cols = cols;
        self.rows = rows.min(MAX_CELLS / cols);
        self.surface = Some(surface);
        self.clear();
    }

    pub fn is_attached(&self) -> bool {
        self.surface.is_some()
    }

    /// Crea la consola con pantalla y la limpia (tests de host: la rejilla
    /// pasa por la pila, que ahí es grande).
    pub fn new(surface: S) -> Self {
        let mut c = Self::empty();
        c.attach(surface);
        c
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_col, self.cursor_row)
    }
    pub fn set_colors(&mut self, fg: Rgb, bg: Rgb) {
        self.fg = fg;
        self.bg = bg;
    }
    pub fn surface(&self) -> Option<&S> {
        self.surface.as_ref()
    }

    pub fn clear(&mut self) {
        self.cells[..self.cols * self.rows].fill(b' ');
        self.cursor_col = 0;
        self.cursor_row = 0;
        if let Some(s) = self.surface.as_mut() {
            let h = s.height();
            s.fill_rows(0, h, self.bg);
        }
    }

    /// Carácter en una celda (para tests y depuración).
    pub fn cell(&self, col: usize, row: usize) -> u8 {
        self.cells[row * self.cols + col]
    }

    pub fn write_char(&mut self, ch: char) {
        match ch {
            '\n' => self.newline(),
            '\r' => self.cursor_col = 0,
            '\t' => {
                let next = (self.cursor_col / 8 + 1) * 8;
                while self.cursor_col < next.min(self.cols) {
                    self.put_char(b' ');
                }
            }
            _ => {
                let b = if ch.is_ascii() && (ch as u8) >= FIRST_CHAR && (ch as u8) <= LAST_CHAR {
                    ch as u8
                } else {
                    b'?'
                };
                self.put_char(b);
            }
        }
    }

    fn put_char(&mut self, b: u8) {
        if self.cursor_col >= self.cols {
            self.newline();
        }
        let (col, row) = (self.cursor_col, self.cursor_row);
        self.cells[row * self.cols + col] = b;
        self.draw_cell(col, row);
        self.cursor_col += 1;
    }

    fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll_up();
        }
    }

    fn scroll_up(&mut self) {
        let cols = self.cols;
        let used = cols * self.rows;
        self.cells.copy_within(cols..used, 0);
        self.cells[used - cols..used].fill(b' ');
        for row in 0..self.rows {
            for col in 0..cols {
                self.draw_cell(col, row);
            }
        }
    }

    fn draw_cell(&mut self, col: usize, row: usize) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let b = self.cells[row * self.cols + col];
        let glyph = &GLYPHS[(b - FIRST_CHAR) as usize];
        let x0 = col * GLYPH_WIDTH;
        let y0 = row * GLYPH_HEIGHT;
        for (dy, bits) in glyph.iter().enumerate() {
            for dx in 0..GLYPH_WIDTH {
                let on = bits & (0x80 >> dx) != 0;
                surface.put(x0 + dx, y0 + dy, if on { self.fg } else { self.bg });
            }
        }
    }
}

impl<S: Surface> core::fmt::Write for Console<S> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.chars() {
            self.write_char(ch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::fmt::Write;
    use std::vec;
    use std::vec::Vec;

    struct Mem {
        w: usize,
        h: usize,
        px: Vec<Rgb>,
    }
    impl Mem {
        fn new(w: usize, h: usize) -> Self {
            Self {
                w,
                h,
                px: vec![Rgb(0, 0, 0); w * h],
            }
        }
        fn at(&self, x: usize, y: usize) -> Rgb {
            self.px[y * self.w + x]
        }
    }
    impl Surface for Mem {
        fn width(&self) -> usize {
            self.w
        }
        fn height(&self) -> usize {
            self.h
        }
        fn put(&mut self, x: usize, y: usize, c: Rgb) {
            self.px[y * self.w + x] = c;
        }
    }

    type C = Console<Mem>;

    #[test]
    fn grid_size_and_clear() {
        let c = C::new(Mem::new(640, 480));
        assert_eq!((c.cols(), c.rows()), (80, 30));
        assert_eq!(c.cursor(), (0, 0));
        assert_eq!(c.surface().unwrap().at(639, 479), C::DEFAULT_BG);
    }

    #[test]
    fn renders_glyph_pixels_from_font() {
        let mut c = C::new(Mem::new(64, 32));
        c.write_char('|');
        // '|' de Terminus: columna central encendida en las filas del medio.
        let glyph = &GLYPHS[(b'|' - FIRST_CHAR) as usize];
        let mut lit = 0;
        for (dy, bits) in glyph.iter().enumerate() {
            for dx in 0..8 {
                let expect = if bits & (0x80 >> dx) != 0 {
                    C::DEFAULT_FG
                } else {
                    C::DEFAULT_BG
                };
                assert_eq!(c.surface().unwrap().at(dx, dy), expect, "pixel ({dx},{dy})");
                lit += (bits & (0x80 >> dx) != 0) as usize;
            }
        }
        assert!(lit > 0, "el glifo no puede estar vacio");
        assert_eq!(c.cursor(), (1, 0));
    }

    #[test]
    fn newline_wrap_and_tab() {
        let mut c = C::new(Mem::new(64, 64)); // 8 cols x 4 rows
        write!(c, "abcdefghij").unwrap(); // 10 chars -> wrap
        assert_eq!(c.cursor(), (2, 1));
        assert_eq!(c.cell(7, 0), b'h');
        assert_eq!(c.cell(0, 1), b'i');
        write!(c, "\r\n\tx").unwrap();
        assert_eq!(c.cursor(), (1, 3));
        assert_eq!(c.cell(0, 3), b'x');
    }

    #[test]
    fn scrolls_when_full() {
        let mut c = C::new(Mem::new(64, 64)); // 8 x 4
        for i in 0..5 {
            writeln!(c, "L{i}").unwrap();
        }
        // Cinco lineas + salto final en 4 filas: quedan L2, L3, L4 y una vacia.
        assert_eq!(c.cell(1, 0), b'2');
        assert_eq!(c.cell(1, 2), b'4');
        assert_eq!(c.cell(0, 3), b' ');
        assert_eq!(c.cursor(), (0, 3));
        // Lo pintado coincide con las celdas: fila 0 muestra 'L'.
        let glyph = &GLYPHS[(b'L' - FIRST_CHAR) as usize];
        let (dx, dy) = (1, 8);
        let expect = if glyph[dy] & (0x80 >> dx) != 0 {
            C::DEFAULT_FG
        } else {
            C::DEFAULT_BG
        };
        assert_eq!(c.surface().unwrap().at(dx, dy), expect);
    }

    #[test]
    fn non_ascii_becomes_question_mark() {
        let mut c = C::new(Mem::new(64, 32));
        write!(c, "ñ\u{1F600}").unwrap();
        assert_eq!(c.cell(0, 0), b'?');
        assert_eq!(c.cell(1, 0), b'?');
    }

    #[test]
    fn empty_console_is_const_and_attaches_in_place() {
        const EMPTY: Console<Mem> = Console::empty();
        let mut c = EMPTY;
        assert!(!c.is_attached());
        assert!(c.surface().is_none());
        // Sin pantalla no dibuja ni falla.
        c.write_str("nada").unwrap();
        assert_eq!(c.cols(), 0);
        c.attach(Mem::new(640, 480));
        assert!(c.is_attached());
        assert_eq!((c.cols(), c.rows()), (80, 30));
        c.write_str("ok").unwrap();
        assert_eq!(c.cell(0, 0), b'o');
        assert_eq!(c.cell(1, 0), b'k');
    }

    #[test]
    fn huge_surface_is_clamped_to_max_cells() {
        let c = C::new(Mem::new(4096, 4096));
        assert!(c.cols() * c.rows() <= MAX_CELLS);
        assert_eq!(c.cols(), 512);
    }
}
