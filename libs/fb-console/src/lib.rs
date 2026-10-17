// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Text console on top of a 32 bpp linear framebuffer.
//!
//! `no_std`, no `alloc`: the character grid lives inside the `Console`
//! itself (fixed maximum size, about 32 KiB). That is why the console is
//! built empty and const (`Console::empty`) in a `static` and the surface is
//! attached afterwards (`attach`): returning it by value would put the grid
//! on the stack, which is small during kernel boot. Pixel access is
//! abstracted in [`Surface`] so the kernel writes MMIO with
//! `write_volatile` and host tests use a `Vec<u32>`.
//!
//! Scrolling does not read from the framebuffer (reading VRAM is slow): a
//! copy of the characters is kept and the screen is repainted.

#![no_std]

pub mod font;

use font::{FIRST_CHAR, GLYPHS, GLYPH_HEIGHT, GLYPH_WIDTH, LAST_CHAR};

/// RGB color, 8 bits per channel; the `Surface` encodes it to its format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Pixel sink. Coordinates in pixels, origin at the top left.
pub trait Surface {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn put(&mut self, x: usize, y: usize, color: Rgb);
    /// Fills `rows` full pixel rows starting at `y`.
    fn fill_rows(&mut self, y: usize, rows: usize, color: Rgb) {
        for yy in y..(y + rows).min(self.height()) {
            for x in 0..self.width() {
                self.put(x, yy, color);
            }
        }
    }
}

/// Maximum text cells: 240 columns x 135 rows covers 1920x2160.
pub const MAX_CELLS: usize = 240 * 135;

pub struct Console<S: Surface> {
    /// `None` until `attach`: without a screen, the console draws nothing.
    surface: Option<S>,
    cols: usize,
    rows: usize,
    cursor_col: usize,
    cursor_row: usize,
    /// Cell where the cursor (`_`) is drawn, if it is.
    cursor_drawn: Option<(usize, usize)>,
    /// There were writes since the last blink: the console is not idle.
    wrote_since_blink: bool,
    fg: Rgb,
    bg: Rgb,
    cells: [u8; MAX_CELLS],
}

impl<S: Surface> Console<S> {
    pub const DEFAULT_FG: Rgb = Rgb(0xD8, 0xDE, 0xE9);
    pub const DEFAULT_BG: Rgb = Rgb(0x10, 0x12, 0x18);

    /// Console without a screen, constructible in a `static`.
    pub const fn empty() -> Self {
        Self {
            surface: None,
            cols: 0,
            rows: 0,
            cursor_col: 0,
            cursor_row: 0,
            cursor_drawn: None,
            wrote_since_blink: false,
            fg: Self::DEFAULT_FG,
            bg: Self::DEFAULT_BG,
            cells: [b' '; MAX_CELLS],
        }
    }

    /// Attaches the screen and clears it. Done on the already placed console
    /// (in a `static`), without moving the grid.
    pub fn attach(&mut self, surface: S) {
        let cols = (surface.width() / GLYPH_WIDTH).max(1);
        let rows = (surface.height() / GLYPH_HEIGHT).max(1);
        // If the screen is larger than MAX_CELLS, rows are cut, not columns.
        self.cols = cols;
        self.rows = rows.min(MAX_CELLS / cols);
        self.surface = Some(surface);
        self.clear();
    }

    pub fn is_attached(&self) -> bool {
        self.surface.is_some()
    }

    /// Creates the console with a screen and clears it (host tests: the grid
    /// goes through the stack, which is large there).
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
        self.cursor_drawn = None;
        if let Some(s) = self.surface.as_mut() {
            let h = s.height();
            s.fill_rows(0, h, self.bg);
        }
    }

    /// Character in a cell (for tests and debugging).
    pub fn cell(&self, col: usize, row: usize) -> u8 {
        self.cells[row * self.cols + col]
    }

    /// Toggles the cursor: a `_` in the cell where the next write will go.
    /// The kernel calls it periodically; any write hides it before painting,
    /// so cell contents never change.
    pub fn blink(&mut self) {
        if self.cursor_drawn.is_some() {
            self.hide_cursor();
            return;
        }
        // While text is flowing nothing is painted: the cursor only makes sense
        // on an idle console (and painting over moving text is costly).
        if core::mem::take(&mut self.wrote_since_blink) {
            return;
        }
        if self.surface.is_none() || self.cols == 0 {
            return;
        }
        let col = self.cursor_col.min(self.cols - 1);
        let row = self.cursor_row;
        self.draw_glyph(col, row, b'_');
        self.cursor_drawn = Some((col, row));
    }

    fn hide_cursor(&mut self) {
        if let Some((col, row)) = self.cursor_drawn.take() {
            self.draw_cell(col, row);
        }
    }

    pub fn write_char(&mut self, ch: char) {
        self.hide_cursor();
        self.wrote_since_blink = true;
        match ch {
            '\n' => self.newline(),
            '\r' => self.cursor_col = 0,
            // Backspace: the cursor moves back one cell; erasing is up to the
            // writer (the shell sends "\x08 \x08").
            // At the start of a row it goes to the end of the previous one,
            // so a line longer than the screen can be edited back.
            '\u{8}' => {
                if self.cursor_col > 0 {
                    self.cursor_col = self.cursor_col.min(self.cols) - 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = self.cols.saturating_sub(1);
                }
            }
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
        let b = self.cells[row * self.cols + col];
        self.draw_glyph(col, row, b);
    }

    fn draw_glyph(&mut self, col: usize, row: usize, b: u8) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
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
        // Terminus '|': central column lit in the middle rows.
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
        assert!(lit > 0, "the glyph cannot be empty");
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
        // Five lines + final newline in 4 rows: L2, L3, L4 and an empty one remain.
        assert_eq!(c.cell(1, 0), b'2');
        assert_eq!(c.cell(1, 2), b'4');
        assert_eq!(c.cell(0, 3), b' ');
        assert_eq!(c.cursor(), (0, 3));
        // What is painted matches the cells: row 0 shows 'L'.
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
    fn backspace_moves_cursor_back_without_drawing() {
        let mut c = C::new(Mem::new(64, 32));
        write!(c, "ab\u{8} \u{8}").unwrap();
        assert_eq!(c.cell(0, 0), b'a');
        assert_eq!(c.cell(1, 0), b' ');
        assert_eq!(c.cursor_col, 1);
        // In column 0 of the first row it stays put.
        write!(c, "\u{8}\u{8}x").unwrap();
        assert_eq!(c.cell(0, 0), b'x');
    }

    #[test]
    fn backspace_wraps_to_the_previous_row() {
        // 64 px wide = 8 columns: 9 characters wrap onto the second row.
        let mut c = C::new(Mem::new(64, 32));
        write!(c, "abcdefghi").unwrap();
        assert_eq!(c.cursor(), (1, 1));
        write!(c, "\u{8}\u{8}").unwrap();
        assert_eq!(c.cursor(), (7, 0), "back to the last column of row 0");
        write!(c, "H").unwrap();
        assert_eq!(c.cell(7, 0), b'H');
        // A full row leaves the cursor pending at the end: backspace lands on
        // the last cell, not one before it.
        let mut c = C::new(Mem::new(64, 32));
        write!(c, "abcdefgh\u{8}X").unwrap();
        assert_eq!(c.cell(7, 0), b'X');
    }

    #[test]
    fn cursor_blinks_without_touching_cells() {
        let mut c = C::new(Mem::new(64, 32));
        write!(c, "a").unwrap();
        // The first blink after writing only notes that the console was
        // active; the next one, now idle, paints.
        c.blink();
        let bottom = &GLYPHS[(b'_' - FIRST_CHAR) as usize];
        let (dy, dx) = bottom
            .iter()
            .enumerate()
            .find_map(|(dy, bits)| {
                (0..GLYPH_WIDTH)
                    .find(|dx| bits & (0x80 >> dx) != 0)
                    .map(|dx| (dy, dx))
            })
            .expect("'_' has pixels");
        let (x, y) = (GLYPH_WIDTH + dx, dy);
        assert_eq!(c.surface().unwrap().at(x, y), C::DEFAULT_BG);
        c.blink();
        assert_eq!(
            c.surface().unwrap().at(x, y),
            C::DEFAULT_FG,
            "cursor painted"
        );
        assert_eq!(c.cell(1, 0), b' ', "the cell does not change");
        c.blink();
        assert_eq!(
            c.surface().unwrap().at(x, y),
            C::DEFAULT_BG,
            "cursor erased"
        );
        // Writing with the cursor painted hides it first: the cell shows 'b'.
        c.blink();
        write!(c, "b").unwrap();
        assert_eq!(c.cursor_drawn, None);
        assert_eq!(c.cell(1, 0), b'b');
        let glyph_b = &GLYPHS[(b'b' - FIRST_CHAR) as usize];
        let on = glyph_b[dy] & (0x80 >> dx) != 0;
        assert_eq!(
            c.surface().unwrap().at(x, y),
            if on { C::DEFAULT_FG } else { C::DEFAULT_BG }
        );
        // Without a screen it does nothing.
        let mut e = C::empty();
        e.blink();
        assert_eq!(e.cursor(), (0, 0));
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
        // Without a screen it neither draws nor fails.
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
