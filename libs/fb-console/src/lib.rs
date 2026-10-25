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
//! Scrolling does not read from the framebuffer (reading VRAM is slow, on
//! real hardware above all): the console keeps the characters it holds
//! (`cells`) and the characters the screen shows (`drawn`), and a redraw
//! paints only the cells where they differ, a glyph per surface call
//! ([`Surface::glyph`]). Writes only change `cells` and mark rows dirty;
//! the drawing happens once per write (`fmt::Write::write_str`) or in
//! bounded steps ([`Console::redraw`]), so a write of many lines scrolls
//! many times but paints once, and a caller that holds a lock with
//! interrupts off can release it between rows (incident 0031: a full
//! repaint per newline, a `put` per pixel, with interrupts off, lost timer
//! ticks and made test timing depend on the kernel's layout).

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
    /// Draws one glyph cell at `(x, y)`: `rows` holds one byte per pixel
    /// row, 8 pixels wide, bit 7 the leftmost; set bits take `fg`, clear
    /// ones `bg`. Pixels outside the surface are clipped. The console
    /// draws only through this; the default goes through `put`, and a
    /// real surface overrides it to encode the colours once, check bounds
    /// once per row and write the row in a tight loop (incident 0031: a
    /// `put` per pixel spent most of the console's time in call overhead).
    fn glyph(&mut self, x: usize, y: usize, rows: &[u8], fg: Rgb, bg: Rgb) {
        let cols = self.width().saturating_sub(x).min(8);
        let lines = self.height().saturating_sub(y).min(rows.len());
        for (dy, bits) in rows[..lines].iter().enumerate() {
            for dx in 0..cols {
                let on = bits & (0x80 >> dx) != 0;
                self.put(x + dx, y + dy, if on { fg } else { bg });
            }
        }
    }
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

/// Value of a `drawn` cell whose pixels match no character (the cursor
/// is painted there, or the screen was lent out): never a valid cell, so
/// the next redraw paints it.
const UNKNOWN: u8 = 0;

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
    /// Rows `dirty_from..dirty_to` may hold cells whose `drawn` differs
    /// from `cells`; empty when `dirty_from >= dirty_to`.
    dirty_from: usize,
    dirty_to: usize,
    fg: Rgb,
    bg: Rgb,
    /// The text the console holds, row by row (`cols` per row).
    cells: [u8; MAX_CELLS],
    /// What the screen shows in each cell (`UNKNOWN` when it is no
    /// character): a RAM shadow, so a redraw never reads the framebuffer.
    drawn: [u8; MAX_CELLS],
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
            dirty_from: 0,
            dirty_to: 0,
            fg: Self::DEFAULT_FG,
            bg: Self::DEFAULT_BG,
            cells: [b' '; MAX_CELLS],
            drawn: [b' '; MAX_CELLS],
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

    /// Lends the screen out: the console keeps its text but draws nothing
    /// until `reattach`, so whoever holds the screen meanwhile (a user
    /// driver under test) is not painted over by a scroll.
    pub fn detach(&mut self) -> Option<S> {
        self.cursor_drawn = None;
        self.surface.take()
    }

    /// Takes a lent screen back. Nothing is known of what the borrower left
    /// on it, so every cell is marked as needing paint; the text kept
    /// meanwhile (lines written while detached included) is drawn by the
    /// next [`redraw`](Self::redraw), which the caller may do in steps.
    pub fn reattach(&mut self, surface: S) {
        self.surface = Some(surface);
        self.invalidate();
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
    /// Changes the colours; the cells already on screen are repainted with
    /// them by the next redraw.
    pub fn set_colors(&mut self, fg: Rgb, bg: Rgb) {
        if (fg, bg) != (self.fg, self.bg) {
            self.fg = fg;
            self.bg = bg;
            self.invalidate();
        }
    }
    pub fn surface(&self) -> Option<&S> {
        self.surface.as_ref()
    }

    pub fn clear(&mut self) {
        let used = self.cols * self.rows;
        self.cells[..used].fill(b' ');
        self.cursor_col = 0;
        self.cursor_row = 0;
        self.cursor_drawn = None;
        self.dirty_from = 0;
        self.dirty_to = 0;
        if let Some(s) = self.surface.as_mut() {
            let h = s.height();
            s.fill_rows(0, h, self.bg);
            // A blank cell is a space drawn in `bg`.
            self.drawn[..used].fill(b' ');
        } else {
            self.drawn[..used].fill(UNKNOWN);
        }
    }

    /// Character in a cell (for tests and debugging).
    pub fn cell(&self, col: usize, row: usize) -> u8 {
        self.cells[row * self.cols + col]
    }

    /// Whether some cell still differs from what the screen shows (the
    /// console is attached and a redraw would paint something).
    pub fn needs_redraw(&self) -> bool {
        self.surface.is_some() && self.dirty_from < self.dirty_to
    }

    /// Paints the dirty rows, at most `max_rows` of them, top first, and
    /// returns `true` once nothing is left to paint. Only cells whose
    /// character differs from what the screen shows are drawn, so a scroll
    /// costs the cells that changed, not the whole screen. Without a
    /// screen it drops the pending work (`reattach` repaints everything)
    /// and returns `true`. Callers that hold a lock with interrupts off
    /// pass a small `max_rows` and release the lock between calls
    /// (incident 0031).
    pub fn redraw(&mut self, max_rows: usize) -> bool {
        if self.surface.is_none() {
            self.dirty_from = 0;
            self.dirty_to = 0;
            return true;
        }
        let mut budget = max_rows;
        while self.dirty_from < self.dirty_to {
            if budget == 0 {
                return false;
            }
            budget -= 1;
            let row = self.dirty_from;
            self.dirty_from += 1;
            self.redraw_row(row);
        }
        self.dirty_from = 0;
        self.dirty_to = 0;
        true
    }

    /// Writes text without drawing it: the cells change and their rows are
    /// marked dirty for the next [`redraw`](Self::redraw).
    pub fn write_deferred(&mut self, args: core::fmt::Arguments) {
        struct Deferred<'a, S: Surface>(&'a mut Console<S>);
        impl<S: Surface> core::fmt::Write for Deferred<'_, S> {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                for ch in s.chars() {
                    self.0.queue_char(ch);
                }
                Ok(())
            }
        }
        let _ = core::fmt::Write::write_fmt(&mut Deferred(self), args);
    }

    /// Toggles the cursor: a `_` in the cell where the next write will go.
    /// The kernel calls it periodically; any write hides it before painting,
    /// so cell contents never change.
    pub fn blink(&mut self) {
        if self.cursor_drawn.is_some() {
            self.hide_cursor();
            self.redraw(usize::MAX);
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
        // Text still waiting to be painted goes first: the cursor belongs
        // after it.
        self.redraw(usize::MAX);
        let col = self.cursor_col.min(self.cols - 1);
        let row = self.cursor_row;
        self.draw_glyph(col, row, b'_');
        self.drawn[row * self.cols + col] = UNKNOWN;
        self.cursor_drawn = Some((col, row));
    }

    /// The cell under the cursor shows `_`, which `drawn` records as
    /// `UNKNOWN`: marking its row dirty is enough for a redraw to restore it.
    fn hide_cursor(&mut self) {
        if let Some((_, row)) = self.cursor_drawn.take() {
            self.mark_dirty(row, row + 1);
        }
    }

    /// Writes one character and draws what changed.
    pub fn write_char(&mut self, ch: char) {
        self.queue_char(ch);
        self.redraw(usize::MAX);
    }

    fn queue_char(&mut self, ch: char) {
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
        self.mark_dirty(row, row + 1);
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

    /// Moves the text up one row. Only the cells change: every row is
    /// marked dirty and the redraw paints the cells whose character moved
    /// in or out (blank areas stay untouched).
    fn scroll_up(&mut self) {
        let cols = self.cols;
        let used = cols * self.rows;
        self.cells.copy_within(cols..used, 0);
        self.cells[used - cols..used].fill(b' ');
        self.mark_dirty(0, self.rows);
    }

    fn mark_dirty(&mut self, from: usize, to: usize) {
        if self.dirty_from >= self.dirty_to {
            self.dirty_from = from;
            self.dirty_to = to;
        } else {
            self.dirty_from = self.dirty_from.min(from);
            self.dirty_to = self.dirty_to.max(to);
        }
    }

    /// Forgets what the screen shows: the next redraw paints every cell.
    fn invalidate(&mut self) {
        let used = self.cols * self.rows;
        self.drawn[..used].fill(UNKNOWN);
        self.mark_dirty(0, self.rows);
    }

    fn redraw_row(&mut self, row: usize) {
        let start = row * self.cols;
        for col in 0..self.cols {
            let b = self.cells[start + col];
            if self.drawn[start + col] != b {
                self.draw_glyph(col, row, b);
                self.drawn[start + col] = b;
            }
        }
    }

    fn draw_glyph(&mut self, col: usize, row: usize, b: u8) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let glyph = &GLYPHS[(b - FIRST_CHAR) as usize];
        surface.glyph(
            col * GLYPH_WIDTH,
            row * GLYPH_HEIGHT,
            glyph,
            self.fg,
            self.bg,
        );
    }
}

/// Writes text and draws it once at the end: a string of many lines
/// scrolls the cells many times but paints the screen once.
impl<S: Surface> core::fmt::Write for Console<S> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.chars() {
            self.queue_char(ch);
        }
        self.redraw(usize::MAX);
        Ok(())
    }

    /// A formatted write is queued whole and painted once.
    fn write_fmt(&mut self, args: core::fmt::Arguments) -> core::fmt::Result {
        self.write_deferred(args);
        self.redraw(usize::MAX);
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
        /// `put` calls so far: a glyph is 128 of them.
        puts: usize,
    }
    impl Mem {
        fn new(w: usize, h: usize) -> Self {
            Self {
                w,
                h,
                px: vec![Rgb(0, 0, 0); w * h],
                puts: 0,
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
            assert!(x < self.w && y < self.h, "put outside the surface");
            self.px[y * self.w + x] = c;
            self.puts += 1;
        }
    }

    const GLYPH_PUTS: usize = GLYPH_WIDTH * GLYPH_HEIGHT;

    /// Glyphs drawn since the last call.
    fn glyphs_drawn(c: &mut C) -> usize {
        let s = c.surface.as_mut().unwrap();
        let n = s.puts / GLYPH_PUTS;
        s.puts = 0;
        n
    }

    /// The screen shows exactly the cells, in the console's colours
    /// (the cursor, if drawn, excepted).
    fn assert_screen_matches_cells(c: &C) {
        let s = c.surface().unwrap();
        for row in 0..c.rows() {
            for col in 0..c.cols() {
                if c.cursor_drawn == Some((col, row)) {
                    continue;
                }
                let glyph = &GLYPHS[(c.cell(col, row) - FIRST_CHAR) as usize];
                for (dy, bits) in glyph.iter().enumerate() {
                    for dx in 0..GLYPH_WIDTH {
                        let on = bits & (0x80 >> dx) != 0;
                        let expect = if on { c.fg } else { c.bg };
                        let (x, y) = (col * GLYPH_WIDTH + dx, row * GLYPH_HEIGHT + dy);
                        assert_eq!(s.at(x, y), expect, "cell ({col},{row}) pixel ({dx},{dy})");
                    }
                }
            }
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

    /// A detached console keeps its text and draws nothing; reattaching
    /// draws all of it again over what the borrower painted.
    #[test]
    fn detached_console_leaves_the_screen_alone_until_reattached() {
        let mut c = C::new(Mem::new(640, 480));
        c.write_str("antes\n").unwrap();
        let mut screen = c.detach().expect("attached");
        assert!(!c.is_attached());
        let painted = Rgb(0xFF, 0x88, 0x00);
        screen.fill_rows(0, 480, painted);
        // Enough lines to scroll: nothing reaches the borrowed screen.
        for _ in 0..40 {
            c.write_str("durante\n").unwrap();
        }
        assert_eq!(screen.at(0, 0), painted);
        assert_eq!(screen.at(639, 479), painted);
        c.reattach(screen);
        assert!(c.is_attached());
        assert!(c.needs_redraw(), "nothing is drawn until the redraw");
        assert_eq!(c.surface().unwrap().at(639, 479), painted);
        assert!(c.redraw(usize::MAX));
        let s = c.surface().unwrap();
        assert_eq!(s.at(639, 479), C::DEFAULT_BG, "kept cells drawn again");
        assert_eq!(c.cell(0, 0), b'd', "the lines written meanwhile are kept");
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

    /// The default `glyph` clips at the surface's edges instead of writing
    /// outside it (a surface narrower or shorter than a cell).
    #[test]
    fn default_glyph_clips_to_the_surface() {
        let mut s = Mem::new(12, 10);
        let rows = [0xFFu8; GLYPH_HEIGHT];
        s.glyph(8, 4, &rows, Rgb(1, 1, 1), Rgb(2, 2, 2));
        assert_eq!(s.puts, 4 * 6, "4 columns x 6 rows inside");
        assert_eq!(s.at(11, 9), Rgb(1, 1, 1));
        assert_eq!(s.at(7, 9), Rgb(0, 0, 0));
        s.glyph(12, 0, &rows, Rgb(1, 1, 1), Rgb(2, 2, 2));
        s.glyph(0, 10, &rows, Rgb(1, 1, 1), Rgb(2, 2, 2));
        assert_eq!(s.puts, 4 * 6, "fully outside draws nothing");
    }

    /// Incident 0031: a scroll repaints only the cells whose character
    /// changed, never the whole screen.
    #[test]
    fn scroll_paints_only_changed_cells() {
        let mut c = C::new(Mem::new(64, 64)); // 8 x 4
        glyphs_drawn(&mut c); // the clear
        for _ in 0..3 {
            c.write_str("ab\n").unwrap();
        }
        assert_eq!(glyphs_drawn(&mut c), 6);
        // Rows 0-2 hold "ab" before and after the scroll: nothing to paint
        // there; row 3 got "ab" and lost it within the same write.
        c.write_str("ab\n").unwrap();
        assert_eq!(glyphs_drawn(&mut c), 0);
        assert_screen_matches_cells(&c);
        // A different line: only its cells and the row it leaves blank.
        c.write_str("xyz").unwrap();
        assert_eq!(glyphs_drawn(&mut c), 3);
        c.write_str("\n").unwrap();
        // Row 2 "ab" -> "xyz" (3 cells), row 3 "xyz" -> blank (3 cells).
        assert_eq!(glyphs_drawn(&mut c), 6);
        assert_screen_matches_cells(&c);
    }

    /// A write of many lines scrolls many times but paints once: at most
    /// one glyph per cell on screen.
    #[test]
    fn one_paint_per_write() {
        let mut c = C::new(Mem::new(640, 480)); // 80 x 30
        glyphs_drawn(&mut c); // the clear
        let mut text = std::string::String::new();
        for i in 0..200 {
            text.push_str(&std::format!("line {i:03} of a long listing\n"));
        }
        c.write_str(&text).unwrap();
        assert!(glyphs_drawn(&mut c) <= 80 * 30);
        assert_eq!(c.cell(5, 0), b'1', "line 171 at the top");
        assert_screen_matches_cells(&c);
        // `write_fmt` too: queued whole, painted once.
        write!(c, "{}", text).unwrap();
        assert!(glyphs_drawn(&mut c) <= 80 * 30);
        assert_screen_matches_cells(&c);
    }

    /// Deferred writes paint nothing until `redraw`, which goes a bounded
    /// number of rows per call (the kernel releases its lock between
    /// calls).
    #[test]
    fn deferred_write_and_redraw_in_steps() {
        let mut c = C::new(Mem::new(64, 64)); // 8 x 4
        glyphs_drawn(&mut c); // the clear
        c.write_deferred(format_args!("a\nb\nc\nd\ne"));
        assert_eq!(glyphs_drawn(&mut c), 0);
        assert!(c.needs_redraw());
        let mut calls = 1;
        while !c.redraw(1) {
            calls += 1;
        }
        assert_eq!(calls, 4, "four rows, one per call");
        assert!(!c.needs_redraw());
        assert_eq!(glyphs_drawn(&mut c), 4);
        assert_screen_matches_cells(&c);
        // A scroll in the middle of a stepped redraw restarts from the top.
        c.write_deferred(format_args!("\nf"));
        assert!(!c.redraw(1));
        c.write_deferred(format_args!("\ng"));
        assert!(c.redraw(usize::MAX));
        assert_screen_matches_cells(&c);
        assert_eq!(c.cell(0, 3), b'g');
    }

    #[test]
    fn new_colors_repaint_every_cell() {
        let mut c = C::new(Mem::new(64, 32)); // 8 x 2
        c.write_str("hi").unwrap();
        glyphs_drawn(&mut c);
        c.set_colors(Rgb(0xFF, 0, 0), Rgb(0, 0, 0xFF));
        assert!(c.needs_redraw());
        c.redraw(usize::MAX);
        assert_eq!(glyphs_drawn(&mut c), 16);
        assert_screen_matches_cells(&c);
    }

    /// Whatever the mix of writes, scrolls, cursor blinks, stepped redraws
    /// and lending the screen out, once redrawn the screen shows exactly
    /// the cells (the RAM shadow never drifts from the pixels).
    #[test]
    fn screen_always_converges_to_the_cells() {
        let mut c = C::new(Mem::new(80, 64)); // 10 x 4
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces = [
            "a",
            "bc",
            "\n",
            "\t",
            "\u{8}",
            "\r",
            "long line wraps",
            "x\ny\n",
            "\u{8} \u{8}",
        ];
        for step in 0..3000 {
            let r = next();
            let piece = pieces[(r >> 8) as usize % pieces.len()];
            match r % 10 {
                0 => c.blink(),
                1 => {
                    let s = c.detach().unwrap();
                    c.write_str(piece).unwrap();
                    c.reattach(s);
                }
                2 => {
                    c.write_deferred(format_args!("{}", piece));
                    c.redraw(1 + (r >> 16) as usize % 3);
                }
                _ => c.write_str(piece).unwrap(),
            }
            if step % 7 == 0 {
                c.redraw(usize::MAX);
                assert_screen_matches_cells(&c);
            }
        }
    }
}
