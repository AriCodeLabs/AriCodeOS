// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! 2D graphics (Phase 26): 32 bpp `0x00RRGGBB` canvases over caller
//! buffers, clipping, text with the console font, a cursor and the
//! compositor desktop model (windows with z-order, focus and
//! hit testing). No `alloc`: everything is tested on the host over `Vec<u32>`.

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
}

/// Canvas over a borrowed buffer: `stride` pixels per row.
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
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
        })
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn get(&self, x: i32, y: i32) -> Option<Color> {
        if !self.bounds().contains(x, y) {
            return None;
        }
        Some(self.pixels[y as usize * self.stride as usize + x as usize])
    }

    pub fn fill(&mut self, color: Color) {
        let b = self.bounds();
        self.fill_rect(&b, color);
    }

    pub fn fill_rect(&mut self, r: &Rect, color: Color) {
        let r = r.intersect(&self.bounds());
        for y in r.y..r.bottom() {
            let row = y as usize * self.stride as usize;
            self.pixels[row + r.x as usize..row + r.right() as usize].fill(color);
        }
    }

    /// Copies `src` with its top-left corner at `(x, y)`, clipping.
    pub fn blit(&mut self, src: &Canvas, x: i32, y: i32) {
        let dst_rect = Rect::new(x, y, src.width, src.height).intersect(&self.bounds());
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
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..GLYPH_WIDTH {
                    let on = bits & (0x80 >> col) != 0;
                    let (px, py) = (gx + col as i32, y + row as i32);
                    if !self.bounds().contains(px, py) {
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
                if self.bounds().contains(px, py) {
                    self.pixels[py as usize * self.stride as usize + px as usize] = color;
                }
            }
        }
    }
}

pub const CURSOR_W: u32 = 12;
pub const CURSOR_H: u32 = 19;
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

    /// Paints the whole desktop into `out`: background, frames, contents and the
    /// cursor. `draw_content(window, canvas)` paints the content of each
    /// window (usually a `blit` of its buffer at `content.x/y`).
    pub fn compose(&self, out: &mut Canvas, mut draw_content: impl FnMut(&Window, &mut Canvas)) {
        out.fill(DESKTOP_BG);
        for w in self.iter() {
            let frame = w.frame();
            let title = if self.focus == Some(w.id) {
                TITLE_ACTIVE
            } else {
                TITLE_INACTIVE
            };
            out.fill_rect(&frame, title);
            out.text(frame.x + 6, frame.y + 2, w.title_str(), TITLE_TEXT, None);
            out.fill_rect(&w.content, rgb(0, 0, 0));
            draw_content(w, out);
        }
        out.cursor(self.pointer.0, self.pointer.1);
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
}
