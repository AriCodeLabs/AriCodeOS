// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! 2D graphics (Phase 26): 32 bpp `0x00RRGGBB` canvases over caller
//! buffers, clipping, text with the console font, a cursor and the
//! compositor desktop model (windows with z-order, focus and
//! hit testing), plus the damage tracking that lets the compositor
//! recompose and flush only what changed ([`Scene`], [`Damage`]). No
//! `alloc`: everything is tested on the host over `Vec<u32>`.

#![no_std]

use aricode_fb_console::font::{FIRST_CHAR, GLYPHS, GLYPH_WIDTH, LAST_CHAR};

/// Color `0x00RRGGBB`.
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r <= x || b <= y {
            Rect::new(0, 0, 0, 0)
        } else {
            Rect::new(x, y, (r - x) as u32, (b - y) as u32)
        }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }
    /// Smallest rectangle holding both (an empty one adds nothing).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let r = self.right().max(o.right());
        let b = self.bottom().max(o.bottom());
        Rect::new(x, y, (r - x) as u32, (b - y) as u32)
    }
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
}

/// Canvas over a borrowed buffer: `stride` pixels per row. Drawing is
/// clipped to the canvas and to its clip rectangle (the whole canvas
/// unless narrowed with [`Canvas::clipped`]); reading is not.
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    clip: Rect,
}

impl<'a> Canvas<'a> {
    /// `None` if the buffer does not cover `height` rows of `stride` pixels.
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32, stride: u32) -> Option<Self> {
        if stride < width || (pixels.len() as u64) < stride as u64 * height as u64 {
            return None;
        }
        Some(Self {
            pixels,
            width,
            height,
            stride,
            clip: Rect::new(0, 0, width, height),
        })
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// The area drawing may touch.
    pub fn clip(&self) -> Rect {
        self.clip
    }

    /// The same pixels, drawing only inside `r` and the current clip.
    pub fn clipped(&mut self, r: &Rect) -> Canvas<'_> {
        Canvas {
            pixels: &mut *self.pixels,
            width: self.width,
            height: self.height,
            stride: self.stride,
            clip: self.clip.intersect(r),
        }
    }

    pub fn get(&self, x: i32, y: i32) -> Option<Color> {
        if !self.bounds().contains(x, y) {
            return None;
        }
        Some(self.pixels[y as usize * self.stride as usize + x as usize])
    }

    /// Fills the clip rectangle.
    pub fn fill(&mut self, color: Color) {
        let c = self.clip;
        self.fill_rect(&c, color);
    }

    pub fn fill_rect(&mut self, r: &Rect, color: Color) {
        let r = r.intersect(&self.clip);
        for y in r.y..r.bottom() {
            let row = y as usize * self.stride as usize;
            self.pixels[row + r.x as usize..row + r.right() as usize].fill(color);
        }
    }

    /// Copies `src` with its top-left corner at `(x, y)`, clipping.
    pub fn blit(&mut self, src: &Canvas, x: i32, y: i32) {
        let dst_rect = Rect::new(x, y, src.width, src.height).intersect(&self.clip);
        for dy in 0..dst_rect.h as i32 {
            let sy = (dst_rect.y + dy - y) as usize;
            let sx = (dst_rect.x - x) as usize;
            let s = sy * src.stride as usize + sx;
            let d = (dst_rect.y + dy) as usize * self.stride as usize + dst_rect.x as usize;
            let n = dst_rect.w as usize;
            self.pixels[d..d + n].copy_from_slice(&src.pixels[s..s + n]);
        }
    }

    /// Text with the console 8x16 font; `None` as background leaves the
    /// pixel unchanged.
    pub fn text(&mut self, x: i32, y: i32, s: &str, fg: Color, bg: Option<Color>) {
        for (i, ch) in s.bytes().enumerate() {
            let glyph = if (FIRST_CHAR..=LAST_CHAR).contains(&ch) {
                &GLYPHS[(ch - FIRST_CHAR) as usize]
            } else {
                &GLYPHS[(b'?' - FIRST_CHAR) as usize]
            };
            let gx = x + (i * GLYPH_WIDTH) as i32;
            let cell = Rect::new(gx, y, GLYPH_WIDTH as u32, glyph.len() as u32);
            if !cell.intersects(&self.clip) {
                continue;
            }
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..GLYPH_WIDTH {
                    let on = bits & (0x80 >> col) != 0;
                    let (px, py) = (gx + col as i32, y + row as i32);
                    if !self.clip.contains(px, py) {
                        continue;
                    }
                    if on {
                        self.pixels[py as usize * self.stride as usize + px as usize] = fg;
                    } else if let Some(bg) = bg {
                        self.pixels[py as usize * self.stride as usize + px as usize] = bg;
                    }
                }
            }
        }
    }

    /// Draws the cursor (12x19 arrow: `#` = black, `.` = white) at `(x, y)`.
    pub fn cursor(&mut self, x: i32, y: i32) {
        for (row, line) in CURSOR.iter().enumerate() {
            for (col, cell) in line.bytes().enumerate() {
                let color = match cell {
                    b'#' => rgb(0, 0, 0),
                    b'.' => rgb(255, 255, 255),
                    _ => continue,
                };
                let (px, py) = (x + col as i32, y + row as i32);
                if self.clip.contains(px, py) {
                    self.pixels[py as usize * self.stride as usize + px as usize] = color;
                }
            }
        }
    }
}

pub const CURSOR_W: u32 = 12;
pub const CURSOR_H: u32 = 19;

/// The pixels [`Canvas::cursor`] may paint for a cursor at `(x, y)`.
pub const fn cursor_rect(x: i32, y: i32) -> Rect {
    Rect::new(x, y, CURSOR_W, CURSOR_H)
}
const CURSOR: [&str; CURSOR_H as usize] = [
    "#           ",
    "##          ",
    "#.#         ",
    "#..#        ",
    "#...#       ",
    "#....#      ",
    "#.....#     ",
    "#......#    ",
    "#.......#   ",
    "#........#  ",
    "#.....#####",
    "#..#..#     ",
    "#.# #..#    ",
    "##  #..#    ",
    "#    #..#   ",
    "     #..#   ",
    "      #..#  ",
    "      #..#  ",
    "       ##   ",
];

/// Title bar height and border thickness of a window.
pub const TITLE_H: u32 = 20;
pub const BORDER: u32 = 2;
pub const MAX_WINDOWS: usize = 8;

/// Desktop colors.
pub const DESKTOP_BG: Color = rgb(0x1e, 0x29, 0x3b);
pub const TITLE_ACTIVE: Color = rgb(0x3b, 0x82, 0xf6);
pub const TITLE_INACTIVE: Color = rgb(0x64, 0x74, 0x8b);
pub const TITLE_TEXT: Color = rgb(0xff, 0xff, 0xff);

/// Desktop window: the client paints the content in its own
/// buffer; only its geometry is kept here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub id: u32,
    /// Content rectangle (without frame) on screen.
    pub content: Rect,
    pub title: [u8; 24],
    pub title_len: u8,
}

impl Window {
    pub fn title_str(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("?")
    }
    /// Full frame: border and title bar.
    pub fn frame(&self) -> Rect {
        Rect::new(
            self.content.x - BORDER as i32,
            self.content.y - (TITLE_H + BORDER) as i32,
            self.content.w + 2 * BORDER,
            self.content.h + TITLE_H + 2 * BORDER,
        )
    }
}

/// What lies under a point of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Desktop,
    /// Window and coordinates relative to the content (negative on the frame).
    Window {
        id: u32,
        x: i32,
        y: i32,
    },
}

/// Desktop: up to `MAX_WINDOWS` windows in z-order (the last one on top),
/// focus and pointer position.
pub struct Desktop {
    pub screen: Rect,
    windows: [Option<Window>; MAX_WINDOWS],
    /// Z-order: indices into `windows`, from back to front.
    order: [u8; MAX_WINDOWS],
    count: usize,
    pub focus: Option<u32>,
    pub pointer: (i32, i32),
    next_id: u32,
}

impl Desktop {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            screen: Rect::new(0, 0, width, height),
            windows: [None; MAX_WINDOWS],
            order: [0; MAX_WINDOWS],
            count: 0,
            focus: None,
            pointer: (width as i32 / 2, height as i32 / 2),
            next_id: 1,
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Creates a cascaded window (each one 40 px further down and right)
    /// and brings it to the front. `None` if there is no room.
    pub fn create(&mut self, w: u32, h: u32, title: &str) -> Option<u32> {
        if self.count == MAX_WINDOWS || w == 0 || h == 0 {
            return None;
        }
        let slot = self.windows.iter().position(Option::is_none)?;
        let id = self.next_id;
        self.next_id += 1;
        let step = 40 * self.count as i32;
        let x = (40 + step).min((self.screen.w as i32 - w as i32).max(0));
        let y =
            (40 + TITLE_H as i32 + step).min((self.screen.h as i32 - h as i32).max(TITLE_H as i32));
        let mut t = [0u8; 24];
        let n = title.len().min(24);
        t[..n].copy_from_slice(&title.as_bytes()[..n]);
        self.windows[slot] = Some(Window {
            id,
            content: Rect::new(x, y, w, h),
            title: t,
            title_len: n as u8,
        });
        self.order[self.count] = slot as u8;
        self.count += 1;
        Some(id)
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let Some(pos) = (0..self.count)
            .position(|i| self.windows[self.order[i] as usize].is_some_and(|w| w.id == id))
        else {
            return false;
        };
        let slot = self.order[pos] as usize;
        self.windows[slot] = None;
        self.order.copy_within(pos + 1..self.count, pos);
        self.count -= 1;
        if self.focus == Some(id) {
            self.focus = None;
        }
        true
    }

    pub fn get(&self, id: u32) -> Option<&Window> {
        self.windows.iter().flatten().find(|w| w.id == id)
    }

    /// Windows from back to front.
    pub fn iter(&self) -> impl Iterator<Item = &Window> + '_ {
        self.order[..self.count]
            .iter()
            .filter_map(move |&i| self.windows[i as usize].as_ref())
    }

    /// Brings `id` to the front and gives it focus.
    pub fn raise(&mut self, id: u32) {
        if let Some(pos) = (0..self.count)
            .position(|i| self.windows[self.order[i] as usize].is_some_and(|w| w.id == id))
        {
            let slot = self.order[pos];
            self.order.copy_within(pos + 1..self.count, pos);
            self.order[self.count - 1] = slot;
            self.focus = Some(id);
        }
    }

    /// Topmost window under `(x, y)` (frame included).
    pub fn hit(&self, x: i32, y: i32) -> Hit {
        for w in self.iter().collect_rev() {
            if w.frame().contains(x, y) {
                return Hit::Window {
                    id: w.id,
                    x: x - w.content.x,
                    y: y - w.content.y,
                };
            }
        }
        Hit::Desktop
    }

    /// Moves the pointer by a relative offset, clamped to the screen.
    pub fn move_pointer(&mut self, dx: i32, dy: i32) {
        let (x, y) = self.pointer;
        self.pointer = (
            (x + dx).clamp(0, self.screen.w as i32 - 1),
            (y + dy).clamp(0, self.screen.h as i32 - 1),
        );
    }

    /// Moves the content of `id` to `(x, y)`, keeping the top-left corner
    /// of its frame on screen. `false` if there is no such window.
    pub fn move_window(&mut self, id: u32, x: i32, y: i32) -> bool {
        let Some(w) = self.windows.iter_mut().flatten().find(|w| w.id == id) else {
            return false;
        };
        let (left, top) = (BORDER as i32, (TITLE_H + BORDER) as i32);
        w.content.x = x.clamp(left, (self.screen.w as i32 - 1 + left).max(left));
        w.content.y = y.clamp(top, (self.screen.h as i32 - 1 + top).max(top));
        true
    }

    /// What the composition depends on, besides the window contents.
    pub fn scene(&self) -> Scene {
        let mut windows = [None; MAX_WINDOWS];
        for (slot, w) in windows.iter_mut().zip(self.iter()) {
            *slot = Some(*w);
        }
        Scene {
            windows,
            count: self.count,
            focus: self.focus,
            pointer: self.pointer,
        }
    }

    /// Paints the whole desktop into `out`: background, frames, contents and the
    /// cursor. `draw_content(window, canvas)` paints the content of each
    /// window (usually a `blit` of its buffer at `content.x/y`); its canvas
    /// is clipped to the content rectangle.
    pub fn compose(&self, out: &mut Canvas, draw_content: impl FnMut(&Window, &mut Canvas)) {
        let all = out.bounds();
        self.compose_region(out, &all, draw_content);
    }

    /// Like [`Desktop::compose`], but repaints only the pixels inside
    /// `region`: the rest of `out` is left as it was, so composing each
    /// rectangle of a [`Damage`] over the previous frame gives the same
    /// pixels as composing the whole screen. Each window draws only inside
    /// its frame (title text included) and the cursor only inside
    /// [`cursor_rect`], which is what [`Scene::damage`] relies on.
    pub fn compose_region(
        &self,
        out: &mut Canvas,
        region: &Rect,
        mut draw_content: impl FnMut(&Window, &mut Canvas),
    ) {
        let mut out = out.clipped(region);
        let clip = out.clip();
        if clip.is_empty() {
            return;
        }
        out.fill(DESKTOP_BG);
        for w in self.iter() {
            let frame = w.frame();
            if !frame.intersects(&clip) {
                continue;
            }
            let title = if self.focus == Some(w.id) {
                TITLE_ACTIVE
            } else {
                TITLE_INACTIVE
            };
            out.fill_rect(&frame, title);
            out.clipped(&frame)
                .text(frame.x + 6, frame.y + 2, w.title_str(), TITLE_TEXT, None);
            out.fill_rect(&w.content, rgb(0, 0, 0));
            draw_content(w, &mut out.clipped(&w.content));
        }
        out.cursor(self.pointer.0, self.pointer.1);
    }
}

/// Snapshot of what a composed frame depends on apart from the window
/// contents: the windows (geometry and title) from back to front, the
/// focus and the pointer. The compositor keeps the scene it last put on
/// screen and diffs the current one against it ([`Scene::damage`]), so no
/// change to the desktop can be forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scene {
    windows: [Option<Window>; MAX_WINDOWS],
    count: usize,
    focus: Option<u32>,
    pointer: (i32, i32),
}

impl Scene {
    fn iter(&self) -> impl Iterator<Item = &Window> + '_ {
        self.windows[..self.count].iter().flatten()
    }

    fn position(&self, id: u32) -> Option<usize> {
        self.iter().position(|w| w.id == id)
    }

    /// Adds to `damage` every screen area whose composed pixels may differ
    /// between `self` and `newer`: both cursor positions if the pointer
    /// moved; the old and new frame of a window that appeared, went away,
    /// moved, was retitled or gained or lost the focus; and, for two windows
    /// whose stacking order swapped, the area where they overlap. Content
    /// changes are not part of a scene: the compositor adds the content
    /// rectangle of a window its client presented.
    pub fn damage(&self, newer: &Scene, damage: &mut Damage) {
        if self.pointer != newer.pointer {
            damage.add(&cursor_rect(self.pointer.0, self.pointer.1));
            damage.add(&cursor_rect(newer.pointer.0, newer.pointer.1));
        }
        let focused = |s: &Scene, id: u32| s.focus == Some(id);
        for old in self.iter() {
            match newer.iter().find(|w| w.id == old.id) {
                None => damage.add(&old.frame()),
                Some(new) => {
                    if new != old || focused(self, old.id) != focused(newer, old.id) {
                        damage.add(&old.frame());
                        damage.add(&new.frame());
                    }
                }
            }
        }
        for new in newer.iter() {
            if self.position(new.id).is_none() {
                damage.add(&new.frame());
            }
        }
        for (i, a) in self.iter().enumerate() {
            for b in self.iter().skip(i + 1) {
                let (Some(na), Some(nb)) = (newer.position(a.id), newer.position(b.id)) else {
                    continue;
                };
                if na > nb {
                    damage.add(&a.frame().intersect(&b.frame()));
                    let (wa, wb) = (newer.windows[na], newer.windows[nb]);
                    if let (Some(wa), Some(wb)) = (wa, wb) {
                        damage.add(&wa.frame().intersect(&wb.frame()));
                    }
                }
            }
        }
    }
}

/// How many rectangles a [`Damage`] keeps before merging the closest ones.
pub const MAX_DAMAGE: usize = 8;

/// Damaged rectangles covering more than this fraction of the screen
/// (numerator, denominator) become the whole screen.
pub const FULL_SCREEN_FRACTION: (u64, u64) = (1, 2);

/// The screen areas to recompose and flush: at most [`MAX_DAMAGE`]
/// disjoint rectangles inside the screen. A rectangle that overlaps one
/// already kept is merged with it (their bounding box); when the set is
/// full, the new one is merged with the rectangle whose bounding box grows
/// least; past [`FULL_SCREEN_FRACTION`] of the screen the set becomes the
/// whole screen. Merging only ever adds pixels, so the set always covers
/// everything added since the last [`Damage::clear`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    screen: Rect,
    rects: [Rect; MAX_DAMAGE],
    count: usize,
}

impl Damage {
    pub const fn new(screen: Rect) -> Self {
        Self {
            screen,
            rects: [Rect::new(0, 0, 0, 0); MAX_DAMAGE],
            count: 0,
        }
    }

    /// The whole screen damaged (the first frame).
    pub const fn full(screen: Rect) -> Self {
        let mut d = Self::new(screen);
        d.rects[0] = screen;
        d.count = 1;
        d
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects[..self.count]
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Pixels covered (the rectangles are disjoint).
    pub fn pixels(&self) -> u64 {
        self.rects().iter().map(Rect::area).sum()
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    fn remove(&mut self, i: usize) {
        self.rects.copy_within(i + 1..self.count, i);
        self.count -= 1;
    }

    pub fn add(&mut self, r: &Rect) {
        let mut r = r.intersect(&self.screen);
        if r.is_empty() || self.rects() == [self.screen] {
            return;
        }
        loop {
            if let Some(i) = self.rects().iter().position(|o| o.intersects(&r)) {
                r = r.union(&self.rects[i]);
                self.remove(i);
            } else if self.count == MAX_DAMAGE {
                let grows = |o: &Rect| r.union(o).area() - o.area();
                let mut best = 0;
                for i in 1..self.count {
                    if grows(&self.rects[i]) < grows(&self.rects[best]) {
                        best = i;
                    }
                }
                r = r.union(&self.rects[best]);
                self.remove(best);
            } else {
                break;
            }
        }
        self.rects[self.count] = r;
        self.count += 1;
        let (num, den) = FULL_SCREEN_FRACTION;
        if self.pixels() * den > self.screen.area() * num {
            *self = Self::full(self.screen);
        }
    }
}

/// Collects up to `MAX_WINDOWS` references in reverse order without `alloc`.
trait CollectRev<'a> {
    fn collect_rev(self) -> RevIter<'a>;
}

impl<'a, I: Iterator<Item = &'a Window>> CollectRev<'a> for I {
    fn collect_rev(self) -> RevIter<'a> {
        let mut items: [Option<&'a Window>; MAX_WINDOWS] = [None; MAX_WINDOWS];
        let mut n = 0;
        for w in self.take(MAX_WINDOWS) {
            items[n] = Some(w);
            n += 1;
        }
        RevIter { items, n }
    }
}

struct RevIter<'a> {
    items: [Option<&'a Window>; MAX_WINDOWS],
    n: usize,
}

impl<'a> Iterator for RevIter<'a> {
    type Item = &'a Window;
    fn next(&mut self) -> Option<&'a Window> {
        if self.n == 0 {
            return None;
        }
        self.n -= 1;
        self.items[self.n]
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn canvas(buf: &mut Vec<u32>, w: u32, h: u32) -> Canvas<'_> {
        buf.resize((w * h) as usize, 0);
        Canvas::new(buf, w, h, w).unwrap()
    }

    #[test]
    fn rect_math() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
        assert!(a.contains(9, 9) && !a.contains(10, 9) && !a.contains(-1, 0));
    }

    #[test]
    fn fill_blit_and_clip() {
        let mut buf = Vec::new();
        let mut c = canvas(&mut buf, 8, 8);
        c.fill(rgb(1, 2, 3));
        c.fill_rect(&Rect::new(-2, 6, 4, 10), rgb(9, 9, 9)); // clipped to (0..2, 6..8)
        assert_eq!(c.get(1, 7), Some(rgb(9, 9, 9)));
        assert_eq!(c.get(2, 7), Some(rgb(1, 2, 3)));
        assert_eq!(c.get(8, 0), None);
        let mut sbuf = vec![rgb(7, 7, 7); 4];
        let src = Canvas::new(&mut sbuf, 2, 2, 2).unwrap();
        c.blit(&src, 7, 7); // only pixel (7,7) falls inside
        assert_eq!(c.get(7, 7), Some(rgb(7, 7, 7)));
        assert_eq!(c.get(6, 7), Some(rgb(1, 2, 3)));
        c.blit(&src, -1, -1); // only (0,0)
        assert_eq!(c.get(0, 0), Some(rgb(7, 7, 7)));
        assert!(
            Canvas::new(&mut [0u32; 4], 4, 2, 4).is_none(),
            "short buffer"
        );
        assert!(
            Canvas::new(&mut [0u32; 8], 4, 2, 2).is_none(),
            "stride smaller than width"
        );
    }

    #[test]
    fn text_and_cursor_draw_something_inside_bounds() {
        let mut buf = Vec::new();
        let mut c = canvas(&mut buf, 40, 20);
        c.fill(0);
        c.text(0, 0, "Hi", rgb(255, 255, 255), None);
        let lit = buf.iter().filter(|&&p| p != 0).count();
        assert!(lit > 10 && lit < 2 * 8 * 16);
        // Partially outside text does not panic.
        let mut c = canvas(&mut buf, 40, 20);
        c.text(36, 10, "xyz", 1, Some(2));
        c.cursor(35, 15);
        assert_eq!(c.get(35, 15), Some(rgb(0, 0, 0)));
    }

    #[test]
    fn desktop_windows_focus_and_hit_test() {
        let mut d = Desktop::new(640, 480);
        let a = d.create(200, 100, "A").unwrap();
        let b = d.create(200, 100, "B").unwrap();
        assert_eq!(d.count(), 2);
        let wa = *d.get(a).unwrap();
        let wb = *d.get(b).unwrap();
        assert_eq!(wa.content, Rect::new(40, 60, 200, 100));
        assert_eq!(wb.content, Rect::new(80, 100, 200, 100));
        // B is on top in the overlapping area.
        assert_eq!(
            d.hit(100, 120),
            Hit::Window {
                id: b,
                x: 20,
                y: 20
            }
        );
        assert_eq!(
            d.hit(50, 70),
            Hit::Window {
                id: a,
                x: 10,
                y: 10
            }
        );
        assert_eq!(d.hit(600, 400), Hit::Desktop);
        // Click on A's title bar: negative coordinates, and A is raised.
        assert_eq!(
            d.hit(50, 45),
            Hit::Window {
                id: a,
                x: 10,
                y: -15
            }
        );
        d.raise(a);
        assert_eq!(d.focus, Some(a));
        assert_eq!(
            d.hit(100, 120),
            Hit::Window {
                id: a,
                x: 60,
                y: 60
            }
        );
        let order: Vec<u32> = d.iter().map(|w| w.id).collect();
        assert_eq!(order, vec![b, a]);
        assert!(d.remove(b));
        assert!(!d.remove(b));
        assert_eq!(d.count(), 1);
        d.move_pointer(-1000, 5);
        assert_eq!(d.pointer, (0, 245));
        d.move_pointer(5000, 5000);
        assert_eq!(d.pointer, (639, 479));
    }

    #[test]
    fn compose_two_windows_and_cursor() {
        let mut d = Desktop::new(320, 240);
        let a = d.create(100, 60, "A").unwrap();
        let b = d.create(100, 60, "B").unwrap();
        d.raise(b);
        d.pointer = (5, 5);
        let mut ca = vec![rgb(255, 0, 0); 100 * 60];
        let mut cb = vec![rgb(0, 0, 255); 100 * 60];
        let mut out = Vec::new();
        let mut screen = canvas(&mut out, 320, 240);
        d.compose(&mut screen, |w, out| {
            let buf = if w.id == a { &mut ca } else { &mut cb };
            let src = Canvas::new(buf, 100, 60, 100).unwrap();
            out.blit(&src, w.content.x, w.content.y);
        });
        let wa = *d.get(a).unwrap();
        let wb = *d.get(b).unwrap();
        assert_eq!(
            screen.get(wa.content.x + 1, wa.content.y + 1),
            Some(rgb(255, 0, 0))
        );
        assert_eq!(
            screen.get(wb.content.x + 1, wb.content.y + 1),
            Some(rgb(0, 0, 255))
        );
        // B wins in the overlap.
        assert_eq!(
            screen.get(wb.content.x + 1, wb.content.y + 1),
            Some(rgb(0, 0, 255))
        );
        assert_eq!(
            screen.get(wa.content.x + 90, wa.content.y + 50),
            Some(rgb(0, 0, 255))
        );
        // B's title bar active, A's inactive; background outside.
        assert_eq!(
            screen.get(wb.content.x + 90, wb.content.y - 5),
            Some(TITLE_ACTIVE)
        );
        assert_eq!(
            screen.get(wa.content.x + 1, wa.content.y - 5),
            Some(TITLE_INACTIVE)
        );
        assert_eq!(screen.get(300, 230), Some(DESKTOP_BG));
        assert_eq!(screen.get(5, 5), Some(rgb(0, 0, 0)), "cursor tip");
    }

    #[test]
    fn rect_union_and_area() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 20, 10, 10);
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 30));
        assert_eq!(a.union(&Rect::new(50, 50, 0, 3)), a, "empty adds nothing");
        assert_eq!(Rect::new(9, 9, 0, 0).union(&b), b);
        assert_eq!(b.area(), 100);
        assert!(a.intersects(&Rect::new(9, 9, 5, 5)));
        assert!(
            !a.intersects(&Rect::new(10, 0, 5, 5)),
            "touching is not overlapping"
        );
    }

    #[test]
    fn a_clipped_canvas_draws_only_inside_its_clip() {
        let mut buf = Vec::new();
        let mut c = canvas(&mut buf, 40, 30);
        c.fill(1);
        let clip = Rect::new(10, 5, 12, 9);
        {
            let mut v = c.clipped(&clip);
            assert_eq!(v.clip(), clip);
            assert_eq!(
                v.clipped(&Rect::new(0, 0, 15, 8)).clip(),
                Rect::new(10, 5, 5, 3)
            );
            v.fill(2);
            v.fill_rect(&Rect::new(0, 0, 40, 30), 3);
            v.text(8, 4, "Clip", 4, Some(5));
            v.cursor(20, 12);
            let mut sbuf = vec![6; 30 * 30];
            let src = Canvas::new(&mut sbuf, 30, 30, 30).unwrap();
            v.blit(&src, 15, 0);
            assert_eq!(v.get(0, 0), Some(1), "reading is not clipped");
        }
        for y in 0..30 {
            for x in 0..40 {
                if !clip.contains(x, y) {
                    assert_eq!(c.get(x, y), Some(1), "({x},{y}) outside the clip");
                }
            }
        }
        assert_eq!(c.get(16, 6), Some(6), "blit inside the clip");
    }

    /// Every pixel of `r` is covered by some rectangle of `d`.
    fn covers(d: &Damage, r: &Rect) -> bool {
        let r = r.intersect(&d.screen);
        (r.y..r.bottom())
            .all(|y| (r.x..r.right()).all(|x| d.rects().iter().any(|o| o.contains(x, y))))
    }

    fn assert_disjoint(d: &Damage) {
        for (i, a) in d.rects().iter().enumerate() {
            assert!(
                !a.is_empty() && d.screen.intersect(a) == *a,
                "{a:?} inside the screen"
            );
            for b in &d.rects()[i + 1..] {
                assert!(!a.intersects(b), "{a:?} and {b:?} overlap");
            }
        }
    }

    #[test]
    fn damage_merges_overlaps_and_stays_bounded() {
        let screen = Rect::new(0, 0, 1000, 1000);
        let mut d = Damage::new(screen);
        assert!(d.is_empty());
        d.add(&Rect::new(10, 10, 12, 19));
        d.add(&Rect::new(11, 10, 12, 19));
        assert_eq!(
            d.rects(),
            [Rect::new(10, 10, 13, 19)],
            "two cursor positions merge"
        );
        d.add(&Rect::new(12, 12, 2, 2));
        assert_eq!(d.rects().len(), 1, "a contained rectangle adds nothing");
        d.add(&Rect::new(-5, 990, 20, 20));
        assert_eq!(
            d.rects()[1],
            Rect::new(0, 990, 15, 10),
            "clipped to the screen"
        );
        d.add(&Rect::new(2000, 0, 5, 5));
        assert_eq!(d.rects().len(), 2, "off-screen adds nothing");
        // Many small scattered rectangles: never more than MAX_DAMAGE.
        let mut added = vec![Rect::new(10, 10, 13, 19), Rect::new(0, 990, 15, 10)];
        for i in 0..40 {
            let r = Rect::new(100 + (i * 97) % 290, 100 + (i * 53) % 290, 5, 5);
            d.add(&r);
            added.push(r);
            assert!(d.rects().len() <= MAX_DAMAGE);
            assert_disjoint(&d);
        }
        assert!(
            added.iter().all(|r| covers(&d, r)),
            "merging never drops pixels"
        );
        assert!(d.pixels() < screen.area() / 2);
        // Past the threshold: the whole screen, and it stays so.
        d.add(&Rect::new(0, 0, 1000, 600));
        assert_eq!(d.rects(), [screen]);
        d.add(&Rect::new(1, 1, 1, 1));
        assert_eq!(d.rects(), [screen]);
        d.clear();
        assert!(d.is_empty() && d.pixels() == 0);
        assert_eq!(Damage::full(screen).rects(), [screen]);
    }

    /// Small deterministic generator for the randomized tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 32) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n.max(1)
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + self.below((hi - lo) as u32 + 1) as i32
        }
    }

    /// The compositor's job over host buffers: the desktop, one content
    /// buffer per window, the scene on screen, the pending damage, the
    /// back buffer and the framebuffer it flushes to.
    struct Model {
        desktop: Desktop,
        contents: Vec<(u32, Vec<u32>)>,
        shown: Scene,
        damage: Damage,
        back: Vec<u32>,
        fb: Vec<u32>,
        w: u32,
        h: u32,
    }

    impl Model {
        fn new(w: u32, h: u32) -> Self {
            let desktop = Desktop::new(w, h);
            let mut m = Self {
                shown: desktop.scene(),
                damage: Damage::full(desktop.screen),
                desktop,
                contents: Vec::new(),
                back: vec![0xdead; (w * h) as usize],
                fb: vec![0xbeef; (w * h) as usize],
                w,
                h,
            };
            m.frame();
            m
        }

        fn draw(&self, out: &mut Canvas, region: &Rect) {
            self.desktop.compose_region(out, region, |win, out| {
                let (_, buf) = self.contents.iter().find(|(id, _)| *id == win.id).unwrap();
                let mut buf = buf.clone();
                let src =
                    Canvas::new(&mut buf, win.content.w, win.content.h, win.content.w).unwrap();
                out.blit(&src, win.content.x, win.content.y);
            });
        }

        fn present(&mut self, id: u32) {
            let content = self.desktop.get(id).unwrap().content;
            self.damage.add(&content);
        }

        /// One composed frame, as the compositor does it; returns the
        /// pixels flushed.
        fn frame(&mut self) -> u64 {
            let scene = self.desktop.scene();
            self.shown.damage(&scene, &mut self.damage);
            self.shown = scene;
            let damage = self.damage;
            self.damage.clear();
            let mut back = core::mem::take(&mut self.back);
            {
                let mut out = Canvas::new(&mut back, self.w, self.h, self.w).unwrap();
                for r in damage.rects() {
                    self.draw(&mut out, r);
                }
            }
            for r in damage.rects() {
                for y in r.y..r.bottom() {
                    let row = (y as u32 * self.w) as usize;
                    let span = row + r.x as usize..row + r.right() as usize;
                    self.fb[span.clone()].copy_from_slice(&back[span]);
                }
            }
            self.back = back;
            damage.pixels()
        }

        fn full(&self) -> Vec<u32> {
            let mut buf = vec![0; (self.w * self.h) as usize];
            let mut out = Canvas::new(&mut buf, self.w, self.h, self.w).unwrap();
            let all = out.bounds();
            self.draw(&mut out, &all);
            buf
        }

        fn create(&mut self, w: u32, h: u32, title: &str, color: Color) -> u32 {
            let id = self.desktop.create(w, h, title).unwrap();
            self.contents.push((id, vec![color; (w * h) as usize]));
            id
        }

        fn random_step(&mut self, rng: &mut Rng) {
            let ids: Vec<u32> = self.desktop.iter().map(|w| w.id).collect();
            let pick = |rng: &mut Rng| ids[rng.below(ids.len() as u32) as usize];
            match rng.below(10) {
                0 if self.desktop.count() < MAX_WINDOWS => {
                    let title = &"A long window title that is cut"[..rng.below(32) as usize];
                    let (w, h) = (rng.range(1, 160) as u32, rng.range(1, 120) as u32);
                    let color = rng.next() & 0xff_ffff;
                    self.create(w, h, title, color);
                }
                1 if !ids.is_empty() => {
                    let id = pick(rng);
                    self.desktop.remove(id);
                    self.contents.retain(|(c, _)| *c != id);
                }
                2 if !ids.is_empty() => self.desktop.raise(pick(rng)),
                3 if !ids.is_empty() => {
                    let id = pick(rng);
                    let x = rng.range(-50, self.w as i32 + 50);
                    let y = rng.range(-50, self.h as i32 + 50);
                    self.desktop.move_window(id, x, y);
                }
                4 | 5 => {
                    let s = if rng.below(4) == 0 { 400 } else { 3 };
                    self.desktop
                        .move_pointer(rng.range(-s, s), rng.range(-s, s));
                }
                6 | 7 if !ids.is_empty() => {
                    let id = pick(rng);
                    let win = *self.desktop.get(id).unwrap();
                    let (_, buf) = self.contents.iter_mut().find(|(c, _)| *c == id).unwrap();
                    let mut c =
                        Canvas::new(buf, win.content.w, win.content.h, win.content.w).unwrap();
                    let r = Rect::new(
                        rng.range(-5, win.content.w as i32),
                        rng.range(-5, win.content.h as i32),
                        rng.below(80),
                        rng.below(60),
                    );
                    c.fill_rect(&r, rng.next() & 0xff_ffff);
                    self.present(id);
                }
                8 => self.desktop.focus = None,
                _ => {}
            }
        }
    }

    #[test]
    fn composing_only_the_damage_matches_full_composition() {
        for seed in 1..=24u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (w, h) = (rng.range(64, 360) as u32, rng.range(48, 260) as u32);
            let mut m = Model::new(w, h);
            assert!(
                m.back == m.full() && m.fb == m.back,
                "first frame, seed {seed}"
            );
            for step in 0..300 {
                // Several events per frame, as when updates are coalesced.
                for _ in 0..rng.range(1, 4) {
                    m.random_step(&mut rng);
                }
                let before = m.fb.clone();
                let flushed = m.frame();
                let full = m.full();
                assert!(
                    m.back == full,
                    "back buffer differs, seed {seed} step {step}"
                );
                assert!(m.fb == full, "framebuffer differs, seed {seed} step {step}");
                let changed = before.iter().zip(&full).filter(|(a, b)| a != b).count() as u64;
                assert!(flushed >= changed, "seed {seed} step {step}");
            }
        }
    }

    #[test]
    fn compose_region_leaves_the_rest_untouched() {
        let mut d = Desktop::new(200, 150);
        d.create(150, 100, "Wide title over the edge").unwrap();
        d.pointer = (60, 70);
        let mut buf = vec![7; 200 * 150];
        let mut out = Canvas::new(&mut buf, 200, 150, 200).unwrap();
        let region = Rect::new(50, 55, 30, 20);
        d.compose_region(&mut out, &region, |_, c| c.fill(0x123456));
        for y in 0..150 {
            for x in 0..200 {
                assert_eq!(region.contains(x, y), out.get(x, y) != Some(7), "({x},{y})");
            }
        }
    }

    #[test]
    fn window_drawing_stays_inside_its_frame() {
        // A narrow window with a long title: the title is cut at the frame.
        let mut d = Desktop::new(200, 100);
        let id = d.create(10, 10, "A very long title").unwrap();
        let frame = d.get(id).unwrap().frame();
        d.pointer = (190, 90);
        let mut buf = Vec::new();
        let mut out = canvas(&mut buf, 200, 100);
        d.compose(&mut out, |_, c| c.fill(0xffffff));
        for y in 0..100 {
            for x in 0..200 {
                if !frame.contains(x, y) && !cursor_rect(190, 90).contains(x, y) {
                    assert_eq!(out.get(x, y), Some(DESKTOP_BG), "({x},{y})");
                }
            }
        }
    }

    #[test]
    fn move_window_keeps_the_frame_corner_on_screen() {
        let mut d = Desktop::new(300, 200);
        let id = d.create(100, 50, "M").unwrap();
        assert!(d.move_window(id, 120, 90));
        assert_eq!(d.get(id).unwrap().content, Rect::new(120, 90, 100, 50));
        d.move_window(id, -500, -500);
        assert_eq!(
            (d.get(id).unwrap().frame().x, d.get(id).unwrap().frame().y),
            (0, 0)
        );
        d.move_window(id, 5000, 5000);
        assert_eq!(
            (d.get(id).unwrap().frame().x, d.get(id).unwrap().frame().y),
            (299, 199)
        );
        assert!(!d.move_window(id + 1, 0, 0));
    }

    /// Figures for the performance audit of 731c398, finding C: pixels and
    /// bytes flushed per frame for one event each, against the full-screen
    /// flip they replace (4 bytes per pixel).
    /// `cargo test -p aricode-gfx flushed -- --nocapture` prints them.
    #[test]
    fn flushed_pixels_per_event() {
        for (w, h) in [(1024u32, 768u32), (1920, 1080)] {
            let full = w as u64 * h as u64;
            // Empty desktop, the pointer moves one pixel (the audit's probe).
            let mut m = Model::new(w, h);
            let before = m.fb.clone();
            m.desktop.move_pointer(1, 0);
            let pointer = m.frame();
            let changed = before.iter().zip(&m.fb).filter(|(a, b)| a != b).count();
            assert_eq!(m.fb, m.full());
            assert_eq!(changed, 80, "pixels that really change");
            assert_eq!(
                pointer,
                ((CURSOR_W + 1) * CURSOR_H) as u64,
                "the two cursor positions"
            );
            assert!(
                pointer < 1000 && pointer * 1000 < full,
                "bounded cursor region"
            );
            // Two 400x300 windows; the top one presents new content.
            let a = m.create(400, 300, "Demo A", rgb(0xc0, 0x40, 0x40));
            let b = m.create(400, 300, "Demo B", rgb(0x40, 0x80, 0xc0));
            m.desktop.raise(b);
            m.frame();
            m.present(b);
            let update = m.frame();
            assert_eq!(update, 400 * 300);
            // The bottom window moves 10 pixels right and down.
            let c = m.desktop.get(a).unwrap().content;
            m.desktop.move_window(a, c.x + 10, c.y + 10);
            let moved = m.frame();
            let frame = m.desktop.get(a).unwrap().frame();
            assert_eq!(moved, (frame.w as u64 + 10) * (frame.h as u64 + 10));
            assert_eq!(m.fb, m.full());
            std::println!(
                "desktop={w}x{h} full_flip={full} px / {} B; pointer_1px={pointer} px / {} B \
                 ({changed} px change); window_update_400x300={update} px / {} B; \
                 window_move_10px={moved} px / {} B",
                full * 4,
                pointer * 4,
                update * 4,
                moved * 4
            );
        }
    }
}
