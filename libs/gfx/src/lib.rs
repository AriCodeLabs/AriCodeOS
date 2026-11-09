// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! 2D graphics (Phase 26): 32 bpp `0x00RRGGBB` canvases over caller
//! buffers, clipping, text with the console font, a cursor and the
//! compositor desktop model (windows with z-order, focus and
//! hit testing), plus the damage tracking that lets the compositor
//! recompose and flush only what changed ([`Scene`], [`Damage`]). No
//! `alloc`: everything is tested on the host over `Vec<u32>`.

#![no_std]

pub mod icons;
pub mod resize;
use icons::Icon;
pub use resize::{Cursor, Grips, Placing};

use aricode_fb_console::char_glyph;
use aricode_fb_console::font::GLYPH_WIDTH;

/// Color `0x00RRGGBB`.
pub type Color = u32;

/// One channel of a screen pixel: where it is and how wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Channel {
    shift: u32,
    bits: u32,
}

impl Channel {
    fn from_mask(mask: u32) -> Option<Self> {
        if mask == 0 {
            return None;
        }
        let shift = mask.trailing_zeros();
        let run = mask >> shift;
        // Contiguous, and of a width a `u8` can be scaled to.
        let bits = run.count_ones();
        (run & run.wrapping_add(1) == 0 && bits <= 16).then_some(Self { shift, bits })
    }

    const fn place(self, v: u32) -> u32 {
        ((v << self.bits) >> 8) << self.shift
    }
}

/// A screen whose firmware describes its pixels by bit masks (UEFI
/// `PixelBitMask`). Turns a [`Color`] into that pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelMasks {
    red: Channel,
    green: Channel,
    blue: Channel,
}

impl PixelMasks {
    /// `None` if a mask is empty, has holes or is wider than 16 bits.
    pub fn new(red: u32, green: u32, blue: u32) -> Option<Self> {
        Some(Self {
            red: Channel::from_mask(red)?,
            green: Channel::from_mask(green)?,
            blue: Channel::from_mask(blue)?,
        })
    }

    pub const fn encode(&self, c: Color) -> u32 {
        self.red.place((c >> 16) & 0xFF)
            | self.green.place((c >> 8) & 0xFF)
            | self.blue.place(c & 0xFF)
    }
}

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
    /// One pixel, if inside the clip.
    pub fn set(&mut self, x: i32, y: i32, color: Color) {
        if self.clip.contains(x, y) {
            self.pixels[y as usize * self.stride as usize + x as usize] = color;
        }
    }

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

    /// A window's body: `r`, with rounded corners over what is below.
    pub fn round_rect(&mut self, r: &Rect, color: Color) {
        let rows = RADIUS.min(r.h as i32 / 2);
        for row in 0..rows {
            let inset = corner_inset(row as usize);
            for y in [r.y + row, r.bottom() - 1 - row] {
                let span = Rect::new(r.x + inset, y, (r.w as i32 - 2 * inset).max(0) as u32, 1);
                self.fill_rect(&span, color);
                // Of a very narrow window, only its own columns.
                for col in 0..inset.min(r.w as i32 / 2) {
                    let cover = CORNER[row as usize][col as usize] as u32;
                    self.blend_pixel(r.x + col, y, color, cover);
                    self.blend_pixel(r.right() - 1 - col, y, color, cover);
                }
            }
        }
        let middle = (r.h as i32 - 2 * rows).max(0) as u32;
        self.fill_rect(&Rect::new(r.x, r.y + rows, r.w, middle), color);
    }

    /// `r` with corners of any `radius`, edges smoothed by 4x4 samples: what controls
    /// are drawn with. Windows have their own, cheaper [`round_rect`](Self::round_rect).
    pub fn rounded(&mut self, r: &Rect, radius: i32, color: Color) {
        if !r.intersects(&self.clip) {
            return;
        }
        let radius = radius.clamp(0, r.w.min(r.h) as i32 / 2);
        let (w, h) = (r.w as i32, r.h as i32);
        self.fill_rect(
            &Rect::new(r.x, r.y + radius, r.w, (h - 2 * radius) as u32),
            color,
        );
        let across = (w - 2 * radius) as u32;
        self.fill_rect(&Rect::new(r.x + radius, r.y, across, radius as u32), color);
        self.fill_rect(
            &Rect::new(r.x + radius, r.bottom() - radius, across, radius as u32),
            color,
        );
        // Eighths of a pixel from the corner circle's centre.
        let q = radius * 8;
        for y in 0..radius {
            for x in 0..radius {
                let mut inside = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let dx = q - (x * 8 + sx * 2 + 1);
                        let dy = q - (y * 8 + sy * 2 + 1);
                        if dx * dx + dy * dy <= q * q {
                            inside += 1;
                        }
                    }
                }
                if inside == 0 {
                    continue;
                }
                let (right, bottom) = (r.right() - 1 - x, r.bottom() - 1 - y);
                for (px, py) in [
                    (r.x + x, r.y + y),
                    (right, r.y + y),
                    (r.x + x, bottom),
                    (right, bottom),
                ] {
                    self.blend_pixel(px, py, color, inside * 16);
                }
            }
        }
    }

    /// Darkens or tints what is in the clip: `amount` of 256 of `color` over it.
    pub fn shade(&mut self, color: Color, amount: u32) {
        let r = self.clip;
        for y in r.y..r.bottom() {
            let row = y as usize * self.stride as usize;
            for px in &mut self.pixels[row + r.x as usize..row + r.right() as usize] {
                *px = blend(*px, color, amount);
            }
        }
    }

    /// A filled circle within `r`, edges smoothed by 4x4 samples.
    pub fn circle(&mut self, r: &Rect, color: Color) {
        if !r.intersects(&self.clip) {
            return;
        }
        let d = r.w.min(r.h) as i32;
        let q = d * 4;
        for y in 0..d {
            for x in 0..d {
                let mut inside = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let dx = x * 8 + sx * 2 + 1 - q;
                        let dy = y * 8 + sy * 2 + 1 - q;
                        if dx * dx + dy * dy <= q * q {
                            inside += 1;
                        }
                    }
                }
                self.blend_pixel(r.x + x, r.y + y, color, inside * 16);
            }
        }
    }

    /// The soft shadow around a frame, `strength` of 256 at its edge, darkening what
    /// is below. Inside the frame nothing is painted: it is about to cover that.
    pub fn window_shadow(&mut self, frame: &Rect, strength: u32) {
        let outer = Rect::new(
            frame.x - SHADOW,
            frame.y - SHADOW,
            frame.w + 2 * SHADOW as u32,
            frame.h + 2 * SHADOW as u32,
        )
        .intersect(&self.clip);
        // The corners' centres: past them on a side, only one axis counts.
        let (x0, x1) = (frame.x + RADIUS, frame.right() - 1 - RADIUS);
        let (y0, y1) = (frame.y + RADIUS, frame.bottom() - 1 - RADIUS);
        for y in outer.y..outer.bottom() {
            let dy = (y0 - y).max(y - y1).max(0) as usize;
            if dy >= SHADOW_SIDE {
                continue;
            }
            // Nothing to darken under the frame, except in its corners' notches.
            let (hole_from, hole_to) = if y < frame.y || y >= frame.bottom() {
                (0, 0)
            } else if dy > 0 {
                (x0, x1 + 1)
            } else {
                (frame.x, frame.right())
            };
            let row = y as usize * self.stride as usize;
            let mut shade = |from: i32, to: i32| {
                for x in from..to {
                    let dx = (x0 - x).max(x - x1).max(0) as usize;
                    if dx < SHADOW_SIDE {
                        let fall = SHADOW_FALL[dy][dx] as u32;
                        if fall != 0 {
                            let px = &mut self.pixels[row + x as usize];
                            *px = blend(*px, 0, (fall * strength) >> 8);
                        }
                    }
                }
            };
            if hole_from >= hole_to {
                shade(outer.x, outer.right());
            } else {
                shade(outer.x, hole_from.min(outer.right()));
                shade(hole_to.max(outer.x), outer.right());
            }
        }
    }

    #[inline]
    fn blend_pixel(&mut self, x: i32, y: i32, color: Color, amount: u32) {
        if amount != 0 && self.clip.contains(x, y) {
            let px = &mut self.pixels[y as usize * self.stride as usize + x as usize];
            *px = if amount >= 256 {
                color
            } else {
                blend(*px, color, amount)
            };
        }
    }

    /// Blends `color` through a `w`x`h` coverage mask at (x, y), clipped: what text
    /// is drawn with.
    pub fn blend_mask(&mut self, x: i32, y: i32, w: u32, h: u32, mask: &[u8], color: Color) {
        let area = Rect::new(x, y, w, h).intersect(&self.clip);
        for py in area.y..area.bottom() {
            let row = (py - y) as usize * w as usize;
            let out = py as usize * self.stride as usize;
            for px in area.x..area.right() {
                let cover = mask[row + (px - x) as usize] as u32;
                if cover != 0 {
                    let p = &mut self.pixels[out + px as usize];
                    // 255 of 255 is the color itself.
                    *p = blend(*p, color, cover + (cover >> 7));
                }
            }
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
        // One cell per character (ADR 0076); what the font cannot show is
        // `?`.
        for (i, ch) in s.chars().enumerate() {
            let glyph = char_glyph(ch);
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

    /// Draws `shape` with its point at `(x, y)`, only inside [`Cursor::rect`]: the
    /// arrow is 12x19 (`#` black, `.` white) with its point at the top left; a resize
    /// cursor is white outlined in black, centred on the point.
    pub fn cursor(&mut self, shape: Cursor, x: i32, y: i32) {
        let mut put = |px: i32, py: i32, color: Color| {
            if self.clip.contains(px, py) {
                self.pixels[py as usize * self.stride as usize + px as usize] = color;
            }
        };
        if shape == Cursor::Arrow {
            for (row, line) in CURSOR.iter().enumerate() {
                for (col, cell) in line.bytes().enumerate() {
                    let color = match cell {
                        b'#' => rgb(0, 0, 0),
                        b'.' => rgb(255, 255, 255),
                        _ => continue,
                    };
                    put(x + col as i32, y + row as i32, color);
                }
            }
            return;
        }
        let area = shape.rect(x, y);
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let (dx, dy) = (px - x, py - y);
                if shape.fills(dx, dy) {
                    put(px, py, rgb(255, 255, 255));
                } else if [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|&(ox, oy)| shape.fills(dx + ox, dy + oy))
                {
                    put(px, py, rgb(0, 0, 0));
                }
            }
        }
    }
}

/// The arrow's size: what [`Cursor::rect`] gives for it.
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

/// Title bar height of a window (ADR 0078).
pub const TITLE_H: u32 = 28;
/// The frame has no border: the title bar, and the content's own edges.
pub const BORDER: u32 = 0;
/// The corners of a window (ADR 0078: 10 px everywhere).
pub const RADIUS: i32 = 10;
/// The three buttons of the title bar: 12 px circles, 20 px apart.
pub const CIRCLE: u32 = 12;
pub const CIRCLE_STEP: i32 = 20;
const CIRCLE_MARGIN: i32 = 8;
/// How far a window's shadow reaches.
pub const SHADOW: i32 = 24;
/// A program's menus at most, and the items of each.
pub const MAX_MENUS: usize = 6;
pub const MAX_MENU_ITEMS: usize = 10;
const MENU_LABEL: usize = 24;

#[derive(Debug, Clone, Copy)]
struct MenuItem {
    label: [u8; MENU_LABEL],
    len: u8,
    id: u32,
}

impl MenuItem {
    const EMPTY: Self = Self {
        label: [0; MENU_LABEL],
        len: 0,
        id: 0,
    };

    fn set(&mut self, label: &str, id: u32) {
        let n = label.floor_char_boundary(MENU_LABEL);
        self.label = [0; MENU_LABEL];
        self.label[..n].copy_from_slice(&label.as_bytes()[..n]);
        self.len = n as u8;
        self.id = id;
    }

    fn text(&self) -> &str {
        core::str::from_utf8(&self.label[..self.len as usize]).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy)]
struct Menu {
    title: MenuItem,
    items: [MenuItem; MAX_MENU_ITEMS],
    count: u8,
}

/// The menus a program gave for the bar, within fixed bounds: text and numbers, which
/// the compositor draws itself, so a hung program's menus still open.
#[derive(Debug, Clone)]
pub struct Menus {
    menus: [Menu; MAX_MENUS],
    count: u8,
}

impl Default for Menus {
    fn default() -> Self {
        let menu = Menu {
            title: MenuItem::EMPTY,
            items: [MenuItem::EMPTY; MAX_MENU_ITEMS],
            count: 0,
        };
        Self {
            menus: [menu; MAX_MENUS],
            count: 0,
        }
    }
}

impl Menus {
    /// Menu `menu` is named `label` when `id` is 0, and gets an item otherwise. A label
    /// may hold a tab: what follows goes at the row's right end. Menus come in order.
    /// `false` past what fits, or for a menu not named yet.
    pub fn add(&mut self, menu: usize, label: &str, id: u32) -> bool {
        if id == 0 {
            if menu != self.count as usize || menu >= MAX_MENUS {
                return false;
            }
            self.menus[menu].title.set(label, 0);
            self.count += 1;
            return true;
        }
        if menu >= self.count as usize {
            return false;
        }
        let m = &mut self.menus[menu];
        if m.count as usize >= MAX_MENU_ITEMS {
            return false;
        }
        m.items[m.count as usize].set(label, id);
        m.count += 1;
        true
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn count(&self) -> usize {
        self.count as usize
    }

    pub fn items(&self, menu: usize) -> usize {
        self.menus.get(menu).map_or(0, |m| m.count as usize)
    }

    /// The number a chosen item tells its program, and whether it is the Paste item, for
    /// which the owner's click opens the clipboard as Ctrl+V does ([`PasteGrant`]).
    pub fn chosen(&self, menu: usize, item: usize) -> Option<(u32, bool)> {
        let m = self.menus.get(menu)?;
        if item >= m.count as usize {
            return None;
        }
        let it = &m.items[item];
        let name = it.text().split('\t').next().unwrap_or("");
        Some((it.id, name == "Paste"))
    }
}

/// The menu on show, and the row the pointer or the arrows are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuOpen {
    pub menu: usize,
    pub item: Option<usize>,
}

/// What is under the pointer, as far as menus go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    Title(usize),
    Item(usize, usize),
    None,
}

/// A running program's place in the dock: its window and rectangle.
pub type Dock = [Option<(u32, Rect)>; MAX_WINDOWS];
/// The longest text the bar shows at its right end, in bytes.
pub const STATUS_MAX: usize = 48;

/// A minute since the Unix epoch, already in the owner's time, as the bar shows it:
/// `Fri 6 Nov  23:40`, in `out`. How many bytes.
pub fn clock_text(minute: i64, out: &mut [u8; STATUS_MAX]) -> usize {
    const DAYS: [&[u8; 3]; 7] = [b"Thu", b"Fri", b"Sat", b"Sun", b"Mon", b"Tue", b"Wed"];
    const MONTHS: [&[u8; 3]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    let days = minute.div_euclid(24 * 60);
    let in_day = minute.rem_euclid(24 * 60);
    // Civil from days, proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let mut n = 0;
    let mut put = |bytes: &[u8]| {
        out[n..n + bytes.len()].copy_from_slice(bytes);
        n += bytes.len();
    };
    put(DAYS[days.rem_euclid(7) as usize]);
    put(b" ");
    if day >= 10 {
        put(&[b'0' + (day / 10) as u8]);
    }
    put(&[b'0' + (day % 10) as u8, b' ']);
    put(MONTHS[(month - 1) as usize]);
    let (h, m) = (in_day / 60, in_day % 60);
    put(b"  ");
    put(&[
        b'0' + (h / 10) as u8,
        b'0' + (h % 10) as u8,
        b':',
        b'0' + (m / 10) as u8,
        b'0' + (m % 10) as u8,
    ]);
    n
}

/// How near the screen's edge a dragged title bar snaps.
pub const SNAP_MARGIN: i32 = 8;
pub const MAX_WINDOWS: usize = 8;

/// What can be changed about how windows look, without touching code that uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub desktop: Color,
    pub window: Color,
    pub title_active: Color,
    pub title_inactive: Color,
    pub close: Color,
    pub minimize: Color,
    pub zoom: Color,
    /// The three circles of a window without the focus.
    pub circle_off: Color,
    /// The shadow's strength at the frame, of 256, with and without the focus.
    pub shadow_active: u32,
    pub shadow_inactive: u32,
    /// The bar at the top of the screen.
    pub bar: Color,
    pub bar_ink: Color,
    pub bar_h: u32,
    /// How far text stays from the bar's ends.
    pub bar_margin: i32,
    /// The dock at the bottom: its background, a running program's place in it, and the
    /// one with the keys.
    pub dock: Color,
    pub dock_active: Color,
    pub dock_ink: Color,
    /// A minimized program's title in the dock.
    pub dock_quiet_ink: Color,
    pub dock_item: (u32, u32),
    pub dock_icon: u32,
    pub dock_icon_top: i32,
    /// Maximum name bubble size, and its gap above the dock.
    pub dock_hint_size: (u32, u32),
    pub dock_hint_gap: i32,
    pub dock_indicator_h: u32,
    pub dock_gap: i32,
    /// Inside the dock, around its places.
    pub dock_pad: i32,
    /// From the screen's edge to the dock.
    pub dock_margin: i32,
    /// A menu's rows, around them, between titles in the bar, and the row under the keys.
    pub menu_row_h: i32,
    pub menu_pad: i32,
    pub menu_gap: i32,
    pub menu_text_pad: i32,
    pub menu_accent: Color,
    pub menu_accent_ink: Color,
    /// A notice, under the bar at the right.
    pub notice_size: (u32, u32),
    pub notice_margin: i32,
}

impl Theme {
    /// The light theme of ADR 0078.
    pub const LIGHT: Self = Self {
        desktop: rgb(0x8E, 0xA9, 0xC4),
        window: rgb(0xEC, 0xEC, 0xEC),
        title_active: rgb(0x26, 0x26, 0x26),
        title_inactive: rgb(0x80, 0x80, 0x80),
        close: rgb(0xFF, 0x5F, 0x57),
        minimize: rgb(0xFE, 0xBC, 0x2E),
        zoom: rgb(0x28, 0xC8, 0x40),
        circle_off: rgb(0xCC, 0xCC, 0xCC),
        shadow_active: 77,
        shadow_inactive: 38,
        bar: rgb(0xF4, 0xF4, 0xF4),
        bar_ink: rgb(0x26, 0x26, 0x26),
        bar_h: 24,
        bar_margin: 12,
        dock: rgb(0xE4, 0xE4, 0xE4),
        dock_active: rgb(0xFF, 0xFF, 0xFF),
        dock_ink: rgb(0x26, 0x26, 0x26),
        dock_quiet_ink: rgb(0x80, 0x80, 0x80),
        dock_item: (44, 40),
        dock_icon: 28,
        dock_icon_top: 3,
        dock_hint_size: (240, 28),
        dock_hint_gap: 8,
        dock_indicator_h: 3,
        dock_gap: 4,
        dock_pad: 4,
        dock_margin: 6,
        menu_row_h: 22,
        menu_pad: 4,
        menu_gap: 16,
        menu_text_pad: 12,
        menu_accent: rgb(0x1E, 0x6E, 0xF4),
        menu_accent_ink: rgb(0xFF, 0xFF, 0xFF),
        notice_size: (380, 36),
        notice_margin: 12,
    };

    /// The dark theme of ADR 0078: the frame on #323232, over a darker desktop.
    pub const DARK: Self = Self {
        desktop: rgb(0x2A, 0x35, 0x41),
        window: rgb(0x32, 0x32, 0x32),
        title_active: rgb(0xE6, 0xE6, 0xE6),
        title_inactive: rgb(0x8C, 0x8C, 0x8C),
        circle_off: rgb(0x55, 0x55, 0x55),
        shadow_active: 110,
        shadow_inactive: 60,
        bar: rgb(0x24, 0x24, 0x24),
        bar_ink: rgb(0xE6, 0xE6, 0xE6),
        dock: rgb(0x3A, 0x3A, 0x3A),
        dock_active: rgb(0x55, 0x55, 0x55),
        dock_ink: rgb(0xE6, 0xE6, 0xE6),
        dock_quiet_ink: rgb(0x8C, 0x8C, 0x8C),
        ..Self::LIGHT
    };

    pub const fn of(dark: bool) -> Self {
        if dark {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::LIGHT
    }
}

/// How many pixels a rounded corner takes from a row, `row` from its top: those it
/// does not cover at all or only in part.
const fn corner_inset(row: usize) -> i32 {
    let mut col = 0;
    while col < RADIUS as usize && CORNER[row][col] < 256 {
        col += 1;
    }
    col as i32
}

/// Mixes `amount`/256 of `over` onto `under`.
const fn blend(under: Color, over: Color, amount: u32) -> Color {
    let keep = 256 - amount;
    let rb = ((under & 0xFF00FF) * keep + (over & 0xFF00FF) * amount) >> 8;
    let g = ((under & 0xFF00) * keep + (over & 0xFF00) * amount) >> 8;
    (rb & 0xFF00FF) | (g & 0xFF00)
}

const fn isqrt(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = v;
    let mut y = v.div_ceil(2);
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// How much of each pixel a circle of `RADIUS` covers in a corner, of 256,
/// counting 4x4 samples: rows from the corner's top, columns from its left.
const fn corner_cover() -> [[u16; RADIUS as usize]; RADIUS as usize] {
    let mut table = [[0u16; RADIUS as usize]; RADIUS as usize];
    let r = RADIUS * 8;
    let mut y = 0;
    while y < RADIUS {
        let mut x = 0;
        while x < RADIUS {
            let mut inside = 0;
            let mut sy = 0;
            while sy < 4 {
                let mut sx = 0;
                while sx < 4 {
                    let dx = r - (x * 8 + sx * 2 + 1);
                    let dy = r - (y * 8 + sy * 2 + 1);
                    if dx * dx + dy * dy <= r * r {
                        inside += 1;
                    }
                    sx += 1;
                }
                sy += 1;
            }
            table[y as usize][x as usize] = inside * 16;
            x += 1;
        }
        y += 1;
    }
    table
}

const CORNER: [[u16; RADIUS as usize]; RADIUS as usize] = corner_cover();

const SHADOW_SIDE: usize = (RADIUS + SHADOW) as usize;

/// A window shadow's strength by distance from its rounded edge, of 256: blurred
/// once, here, and used in slices. Rows and columns from the corner's centre.
const fn shadow_table() -> [[u8; SHADOW_SIDE]; SHADOW_SIDE] {
    let mut table = [[0u8; SHADOW_SIDE]; SHADOW_SIDE];
    let mut y = 0;
    while y < SHADOW_SIDE {
        let mut x = 0;
        while x < SHADOW_SIDE {
            let d = isqrt((x * x + y * y) as u32) as i32 - RADIUS;
            if d < SHADOW {
                // A soft fall: (1 - d/S) squared.
                let left = if d < 0 { SHADOW } else { SHADOW - d } as u32;
                table[y][x] = (255 * left * left / (SHADOW * SHADOW) as u32) as u8;
            }
            x += 1;
        }
        y += 1;
    }
    table
}

static SHADOW_FALL: [[u8; SHADOW_SIDE]; SHADOW_SIDE] = shadow_table();

/// A side of a window, for resizing: bits, so a corner is two.
pub mod edge {
    pub const LEFT: u8 = 1;
    pub const RIGHT: u8 = 2;
    pub const TOP: u8 = 4;
    pub const BOTTOM: u8 = 8;
}

/// Where a title bar dragged to the screen's edge puts a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Full,
}

/// Which snap the pointer asks for at (x, y), on a screen of that size: a side is a
/// half, a corner a quarter, the top all of it.
pub fn snap_at(x: i32, y: i32, width: u32, height: u32) -> Option<Snap> {
    let near_left = x < SNAP_MARGIN;
    let near_right = x >= width as i32 - SNAP_MARGIN;
    let near_top = y < SNAP_MARGIN;
    let near_bottom = y >= height as i32 - SNAP_MARGIN;
    match (near_left, near_right, near_top, near_bottom) {
        (true, _, true, _) => Some(Snap::TopLeft),
        (_, true, true, _) => Some(Snap::TopRight),
        (true, _, _, true) => Some(Snap::BottomLeft),
        (_, true, _, true) => Some(Snap::BottomRight),
        (true, ..) => Some(Snap::Left),
        (_, true, ..) => Some(Snap::Right),
        (_, _, true, _) => Some(Snap::Full),
        _ => None,
    }
}

/// The content rectangle a snap gives in the area windows may fill.
pub fn snap_content(snap: Snap, work: &Rect) -> Rect {
    let (width, height) = (work.w, work.h);
    let (half_w, half_h) = (width / 2, height / 2);
    let (x, y, w, h) = match snap {
        Snap::Left => (0, 0, half_w, height),
        Snap::Right => (half_w, 0, width - half_w, height),
        Snap::TopLeft => (0, 0, half_w, half_h),
        Snap::TopRight => (half_w, 0, width - half_w, half_h),
        Snap::BottomLeft => (0, half_h, half_w, height - half_h),
        Snap::BottomRight => (half_w, half_h, width - half_w, height - half_h),
        Snap::Full => (0, 0, width, height),
    };
    Rect::new(
        work.x + x as i32,
        work.y + (y + TITLE_H) as i32,
        w,
        h.saturating_sub(TITLE_H),
    )
}

/// What the title of a window whose program does not answer ends with.
pub const HUNG_MARK: &str = " (not responding)";

/// Desktop window: the client paints the content in its own
/// buffer; only its geometry is kept here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub id: u32,
    /// Content rectangle (without frame) on screen.
    pub content: Rect,
    pub title: [u8; 24],
    pub title_len: u8,
    /// Its program did not take a request to close (ADR 0077): the title
    /// says so, and the next click on the close box ends its session.
    pub hung: bool,
    /// Minimized: not shown, until Alt+Tab brings it back.
    pub hidden: bool,
    /// Its program answers `Configure`: its edges resize it and zoom works.
    pub resizable: bool,
    /// Where it was before the zoom button filled the screen.
    pub restore: Option<Rect>,
    /// The program's name from its signed manifest, as init told it. What the bar and the
    /// dock show: its title is whatever the program says.
    program: [u8; MENU_LABEL],
    program_len: u8,
}

impl Window {
    pub fn title_str(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("?")
    }
    /// The close box: a square at the right end of the title bar. A
    /// window too narrow to keep some title beside it has none (audit of
    /// ADR 0077, L7).
    pub fn close_box(&self) -> Option<Rect> {
        self.circle(0)
    }

    /// The three buttons, left to right: close, minimize, zoom. A window too
    /// narrow for them has none, and its title takes the room.
    fn circle(&self, index: i32) -> Option<Rect> {
        let f = self.frame();
        if (f.w as i32) < 2 * CIRCLE_MARGIN + 2 * CIRCLE_STEP + CIRCLE as i32 {
            return None;
        }
        Some(Rect::new(
            f.x + CIRCLE_MARGIN + index * CIRCLE_STEP,
            f.y + (TITLE_H - CIRCLE) as i32 / 2,
            CIRCLE,
            CIRCLE,
        ))
    }

    pub fn minimize_box(&self) -> Option<Rect> {
        self.circle(1)
    }

    pub fn zoom_box(&self) -> Option<Rect> {
        self.circle(2)
    }

    /// What the window can touch on the screen: its frame and its shadow.
    pub fn outer(&self) -> Rect {
        let f = self.frame();
        Rect::new(
            f.x - SHADOW,
            f.y - SHADOW,
            f.w + 2 * SHADOW as u32,
            f.h + 2 * SHADOW as u32,
        )
    }

    /// What the bar and the dock call it: its program's name if init told it, as a
    /// name reads, or else its own title, for what no manifest names.
    pub fn panel_name(&self) -> &str {
        if self.program_len == 0 {
            return self.title_str();
        }
        Icon::application_name(self.program_name())
    }

    /// Identity supplied by init, never inferred from the window title.
    pub fn program_name(&self) -> &str {
        core::str::from_utf8(&self.program[..self.program_len as usize]).unwrap_or("")
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

/// What draws the title bars' text: the console font by default, a real one from a
/// caller that has it (`aricode-text`, in the compositor).
pub trait Labels {
    fn width(&mut self, text: &str) -> i32;
    /// Draws `text` inside `canvas`'s clip, its line centred on `middle`, from `x`.
    fn draw(&mut self, canvas: &mut Canvas, text: &str, x: i32, middle: i32, color: Color);
}

/// The console font, for what has nothing better.
pub struct CellLabels;

impl Labels for CellLabels {
    fn width(&mut self, text: &str) -> i32 {
        (text.chars().count() * GLYPH_WIDTH) as i32
    }
    fn draw(&mut self, canvas: &mut Canvas, text: &str, x: i32, middle: i32, color: Color) {
        canvas.text(x, middle - 8, text, color, None);
    }
}

/// What lies under a point of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Desktop,
    /// A notice on show: a click takes it away.
    Notice,
    /// The bar at the top, or the dock around its places: nothing below takes the click.
    Panel,
    /// A running program's place in the dock.
    Dock {
        id: u32,
    },
    /// Window and coordinates relative to the content (negative on the frame).
    Window {
        id: u32,
        x: i32,
        y: i32,
    },
    /// The close box of a window's title bar (ADR 0077).
    Close {
        id: u32,
    },
    Minimize {
        id: u32,
    },
    Zoom {
        id: u32,
    },
    /// The rest of the title bar: dragging it moves the window.
    TitleBar {
        id: u32,
    },
    /// Where a resizable window's [`edge`]s are grabbed: the band around it, or a
    /// rounded corner's notch ([`Grips::edges_at`]).
    Edge {
        id: u32,
        edges: u8,
    },
}

/// How long after its session was made a program's first window may
/// still take the focus: the time to verify and start a large package.
pub const FOCUS_GRACE_MS: u64 = 5_000;

/// Whether a new window takes the focus (ADR 0074 phase C): only the first
/// window of a session made in the last `FOCUS_GRACE_MS` (the program the
/// owner just opened), and only if no key was pressed since that session
/// was made (the owner has moved on and is typing elsewhere). Otherwise a
/// program could open a window whenever it liked and take the keys meant
/// for another, a password typed in a terminal among them (audit of phase
/// C, M2).
pub fn new_window_takes_focus(
    first_of_session: bool,
    since_session_ms: u64,
    key_since_session: bool,
) -> bool {
    first_of_session && since_session_ms <= FOCUS_GRACE_MS && !key_since_session
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
    /// The pointer's picture: the compositor says what the pointer would do.
    pub cursor: Cursor,
    /// Where resizable windows are grabbed, and their smallest size.
    pub grips: Grips,
    next_id: u32,
    status: [u8; STATUS_MAX],
    status_len: u8,
    notice: [u8; STATUS_MAX],
    notice_len: u8,
    pub theme: Theme,
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
            cursor: Cursor::Arrow,
            grips: Grips::DEFAULT,
            next_id: 1,
            status: [0; STATUS_MAX],
            status_len: 0,
            notice: [0; STATUS_MAX],
            notice_len: 0,
            theme: Theme::LIGHT,
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
        let top = (self.theme.bar_h + TITLE_H) as i32;
        let y = (40 + TITLE_H as i32 + step).min((self.screen.h as i32 - h as i32).max(top));
        let mut t = [0u8; 24];
        let n = title.len().min(24);
        t[..n].copy_from_slice(&title.as_bytes()[..n]);
        self.windows[slot] = Some(Window {
            id,
            content: Rect::new(x, y, w, h),
            title: t,
            title_len: n as u8,
            hung: false,
            hidden: false,
            resizable: false,
            restore: None,
            program: [0; MENU_LABEL],
            program_len: 0,
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

    /// Marks `id` as a window whose program does not answer, or not.
    pub fn set_hung(&mut self, id: u32, hung: bool) {
        if let Some(w) = self.windows.iter_mut().flatten().find(|w| w.id == id) {
            w.hung = hung;
        }
    }

    /// The front window, if any.
    pub fn front(&self) -> Option<u32> {
        self.iter().filter(|w| !w.hidden).last().map(|w| w.id)
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
        if self.notice_len > 0 && self.notice_box().contains(x, y) {
            return Hit::Notice;
        }
        let (dock, places) = self.dock();
        if let Some((id, _)) = places.iter().flatten().find(|(_, p)| p.contains(x, y)) {
            return Hit::Dock { id: *id };
        }
        if dock.contains(x, y) || self.bar().contains(x, y) {
            return Hit::Panel;
        }
        for w in self.iter().collect_rev() {
            if w.hidden {
                continue;
            }
            let f = w.frame();
            if w.resizable {
                let edges = self.grips.edges_at(&f, x, y);
                if edges != 0 {
                    return Hit::Edge { id: w.id, edges };
                }
            }
            if !f.contains(x, y) {
                continue;
            }
            let inside = |b: Option<Rect>| b.is_some_and(|b| b.contains(x, y));
            if inside(w.close_box()) {
                return Hit::Close { id: w.id };
            }
            if inside(w.minimize_box()) {
                return Hit::Minimize { id: w.id };
            }
            if inside(w.zoom_box()) {
                return Hit::Zoom { id: w.id };
            }
            if !w.content.contains(x, y) {
                return Hit::TitleBar { id: w.id };
            }
            return Hit::Window {
                id: w.id,
                x: x - w.content.x,
                y: y - w.content.y,
            };
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
        let left = BORDER as i32;
        let title = (TITLE_H + BORDER) as i32;
        let top = self.theme.bar_h as i32 + title;
        w.content.x = x.clamp(left, (self.screen.w as i32 - 1 + left).max(left));
        w.content.y = y.clamp(top, (self.screen.h as i32 - 1 + title).max(top));
        true
    }

    pub fn set_resizable(&mut self, id: u32) {
        if let Some(w) = self.windows.iter_mut().flatten().find(|w| w.id == id) {
            w.resizable = true;
        }
    }

    /// Where a frame may be dragged to by its edges: the screen under the bar.
    pub fn drag_bounds(&self) -> Rect {
        let bar = self.theme.bar_h.min(self.screen.h);
        Rect::new(0, bar as i32, self.screen.w, self.screen.h - bar)
    }

    /// The bar at the top of the screen.
    pub fn bar(&self) -> Rect {
        Rect::new(0, 0, self.screen.w, self.theme.bar_h)
    }

    /// Where windows may go when they are fitted to something: the screen, less the bar
    /// and the dock.
    pub fn work(&self) -> Rect {
        let t = &self.theme;
        let dock = t.dock_item.1 as i32 + 2 * t.dock_pad + 2 * t.dock_margin;
        let h = (self.screen.h as i32 - t.bar_h as i32 - dock).max(0);
        Rect::new(0, t.bar_h as i32, self.screen.w, h as u32)
    }

    /// The dock, and each running program's place in it: centred, in the order they
    /// opened, narrower when many would not fit. Empty with nothing running.
    pub fn dock(&self) -> (Rect, Dock) {
        let t = &self.theme;
        let mut places = [None; MAX_WINDOWS];
        let n = self.count as i32;
        if n == 0 {
            return (Rect::new(0, 0, 0, 0), places);
        }
        let around = 2 * (t.dock_pad + t.dock_margin);
        let room = self.screen.w as i32 - around - (n - 1) * t.dock_gap;
        let w = (room / n).clamp(1, t.dock_item.0 as i32);
        let all = n * w + (n - 1) * t.dock_gap;
        let y = self.screen.h as i32 - t.dock_margin - t.dock_pad - t.dock_item.1 as i32;
        let mut x = (self.screen.w as i32 - all) / 2;
        let open = self.windows.iter().flatten();
        for (slot, window) in places.iter_mut().zip(open) {
            *slot = Some((window.id, Rect::new(x, y, w as u32, t.dock_item.1)));
            x += w + t.dock_gap;
        }
        let outer = Rect::new(
            (self.screen.w as i32 - all) / 2 - t.dock_pad,
            y - t.dock_pad,
            (all + 2 * t.dock_pad) as u32,
            t.dock_item.1 + 2 * t.dock_pad as u32,
        );
        (outer, places)
    }

    /// The hovered program and the maximum area its name may paint. Geometry is
    /// independent of the font so the scene can invalidate the old and new bubble.
    fn dock_hint(&self) -> Option<(u32, Rect)> {
        let (dock, places) = self.dock();
        let (id, place) = places
            .into_iter()
            .flatten()
            .find(|(_, r)| r.contains(self.pointer.0, self.pointer.1))?;
        let (w, h) = self.theme.dock_hint_size;
        let w = w.min(self.screen.w);
        let x = (place.x + place.w as i32 / 2 - w as i32 / 2).clamp(0, (self.screen.w - w) as i32);
        let y = dock.y - self.theme.dock_hint_gap - h as i32;
        // On a very short display there is no room above the dock.
        (y >= self.bar().bottom()).then_some((id, Rect::new(x, y, w, h)))
    }

    /// Where a menu's title is in the bar: after the focused window's name.
    fn menu_titles(&self, menus: &Menus, labels: &mut impl Labels) -> [Rect; MAX_MENUS] {
        let t = &self.theme;
        let mut rects = [Rect::new(0, 0, 0, 0); MAX_MENUS];
        let name = self
            .focus
            .and_then(|id| self.get(id))
            .map_or(0, |w| labels.width(w.panel_name()));
        let mut x = t.bar_margin + name + t.menu_gap / 2;
        // A title that would reach the clock is not shown, and cannot be clicked: the
        // bar's right end says what only the system says.
        let limit = self.screen.w as i32 - t.bar_margin * 2 - labels.width(self.status_str());
        for (i, rect) in rects.iter_mut().enumerate().take(menus.count()) {
            let w = labels.width(menus.menus[i].title.text()) + t.menu_gap;
            if x + w > limit {
                break;
            }
            *rect = Rect::new(x, 0, w as u32, t.bar_h);
            x += w;
        }
        rects
    }

    /// The rectangle of an open menu, under its title.
    pub fn menu_box(&self, menus: &Menus, labels: &mut impl Labels, menu: usize) -> Rect {
        let t = &self.theme;
        let Some(m) = menus.menus.get(menu).filter(|_| menu < menus.count()) else {
            return Rect::new(0, 0, 0, 0);
        };
        let mut wide = 0;
        for it in &m.items[..m.count as usize] {
            let (name, keys) = it.text().split_once('\t').unwrap_or((it.text(), ""));
            let both = labels.width(name) + labels.width(keys);
            let apart = if keys.is_empty() { 0 } else { t.menu_gap * 2 };
            wide = wide.max(both + apart);
        }
        let w = wide + 2 * (t.menu_pad + t.menu_text_pad);
        let h = m.count as i32 * t.menu_row_h + 2 * t.menu_pad;
        let x = self.menu_titles(menus, labels)[menu].x.max(t.bar_margin);
        // On the screen, also for the rightmost title on a small one.
        let x = x.min(self.screen.w as i32 - w).max(0);
        Rect::new(x, t.bar_h as i32, w as u32, h as u32)
    }

    fn menu_row(&self, area: &Rect, item: usize) -> Rect {
        let t = &self.theme;
        Rect::new(
            area.x + t.menu_pad,
            area.y + t.menu_pad + item as i32 * t.menu_row_h,
            (area.w as i32 - 2 * t.menu_pad).max(0) as u32,
            t.menu_row_h as u32,
        )
    }

    /// What the pointer is on, with the open menu, if any, over everything else.
    pub fn menu_at(
        &self,
        menus: &Menus,
        labels: &mut impl Labels,
        open: Option<usize>,
        x: i32,
        y: i32,
    ) -> MenuHit {
        if let Some(menu) = open {
            let area = self.menu_box(menus, labels, menu);
            if area.contains(x, y) {
                return (0..menus.items(menu))
                    .find(|&i| self.menu_row(&area, i).contains(x, y))
                    .map_or(MenuHit::None, |i| MenuHit::Item(menu, i));
            }
        }
        let titles = self.menu_titles(menus, labels);
        titles[..menus.count()]
            .iter()
            .position(|r| r.contains(x, y))
            .map_or(MenuHit::None, MenuHit::Title)
    }

    /// The titles in the bar and the open menu, over everything but the pointer.
    fn compose_menus(
        &self,
        out: &mut Canvas,
        labels: &mut impl Labels,
        menus: &Menus,
        open: Option<MenuOpen>,
    ) {
        let t = &self.theme;
        let titles = self.menu_titles(menus, labels);
        let middle = t.bar_h as i32 / 2;
        for (i, rect) in titles[..menus.count()].iter().enumerate() {
            if rect.is_empty() {
                continue;
            }
            let on = open.is_some_and(|o| o.menu == i);
            if on {
                out.fill_rect(rect, t.menu_accent);
            }
            let ink = if on { t.menu_accent_ink } else { t.bar_ink };
            let x = rect.x + t.menu_gap / 2;
            let mut inside = out.clipped(rect);
            labels.draw(&mut inside, menus.menus[i].title.text(), x, middle, ink);
        }
        let Some(open) = open else { return };
        let area = self.menu_box(menus, labels, open.menu);
        out.window_shadow(&area, t.shadow_active);
        out.rounded(&area, RADIUS - t.menu_pad, t.bar);
        let m = &menus.menus[open.menu];
        for (i, it) in m.items[..m.count as usize].iter().enumerate() {
            let row = self.menu_row(&area, i);
            let on = open.item == Some(i);
            if on {
                out.rounded(&row, t.menu_pad, t.menu_accent);
            }
            let ink = if on { t.menu_accent_ink } else { t.bar_ink };
            let (name, keys) = it.text().split_once('\t').unwrap_or((it.text(), ""));
            let mid = row.y + row.h as i32 / 2;
            let mut inside = out.clipped(&row);
            labels.draw(&mut inside, name, row.x + t.menu_text_pad, mid, ink);
            let right = row.right() - t.menu_text_pad - labels.width(keys);
            labels.draw(&mut inside, keys, right, mid, ink);
        }
    }

    /// The window's program, from its manifest: see [`Window::panel_name`].
    pub fn set_program(&mut self, id: u32, name: &[u8]) {
        if let Some(w) = self.windows.iter_mut().flatten().find(|w| w.id == id) {
            let n = name.len().min(MENU_LABEL);
            w.program[..n].copy_from_slice(&name[..n]);
            w.program_len = n as u8;
        }
    }

    /// Shows a notice, or takes it away with an empty text. The compositor's own: no
    /// program writes here, so what one says can be believed.
    pub fn set_notice(&mut self, text: &str) {
        let n = text.floor_char_boundary(self.notice.len());
        self.notice = [0; STATUS_MAX];
        self.notice[..n].copy_from_slice(&text.as_bytes()[..n]);
        self.notice_len = n as u8;
    }

    /// Where a notice goes: under the bar, at the right, and on a small screen.
    fn notice_box(&self) -> Rect {
        let t = &self.theme;
        let (w, h) = t.notice_size;
        let w = w.min(self.screen.w);
        let x = (self.screen.w as i32 - w as i32 - t.notice_margin).max(0);
        Rect::new(x, t.bar_h as i32 + t.notice_margin, w, h)
    }

    /// What the bar says at its right end: the clock.
    pub fn set_status(&mut self, text: &str) {
        let n = text.floor_char_boundary(self.status.len());
        self.status = [0; STATUS_MAX];
        self.status[..n].copy_from_slice(&text.as_bytes()[..n]);
        self.status_len = n as u8;
    }

    fn status_str(&self) -> &str {
        core::str::from_utf8(&self.status[..self.status_len as usize]).unwrap_or("")
    }

    /// Hides or shows a window. A hidden one is neither drawn nor hit, and takes
    /// no focus.
    pub fn set_hidden(&mut self, id: u32, hidden: bool) {
        if let Some(w) = self.windows.iter_mut().flatten().find(|w| w.id == id) {
            w.hidden = hidden;
        }
        if hidden && self.focus == Some(id) {
            self.focus = None;
        }
    }

    /// Remembers where a window was, or forgets it, for the zoom button.
    pub fn set_restore(&mut self, id: u32, at: Option<Rect>) {
        if let Some(w) = self.windows.iter_mut().flatten().find(|w| w.id == id) {
            w.restore = at;
        }
    }

    /// Gives a window's content a new size. Its corner stays unless that would
    /// put its right side or its close box off the screen.
    pub fn resize(&mut self, id: u32, w: u32, h: u32) -> bool {
        let (screen_w, screen_h) = (self.screen.w as i32, self.screen.h as i32);
        let bar_h = self.theme.bar_h;
        match self.windows.iter_mut().flatten().find(|win| win.id == id) {
            Some(win) if w != 0 && h != 0 => {
                win.content.w = w;
                win.content.h = h;
                let left = BORDER as i32;
                // Under the bar its close box would be out of reach (ADR 0077).
                let top = (bar_h + TITLE_H + BORDER) as i32;
                let fit =
                    |room: i32, side: u32, low: i32| (room - side as i32 - BORDER as i32).max(low);
                win.content.x = win.content.x.min(fit(screen_w, w, left));
                win.content.y = win.content.y.min(fit(screen_h, h, top));
                true
            }
            _ => false,
        }
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
            cursor: self.cursor,
            bar: self.bar(),
            dock: self.dock().0,
            dock_hint: self.dock_hint(),
            status: self.status,
            notice: self.notice,
            notice_at: self.notice_box(),
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
    /// [`Cursor::rect`], which is what [`Scene::damage`] relies on.
    pub fn compose_region(
        &self,
        out: &mut Canvas,
        region: &Rect,
        draw_content: impl FnMut(&Window, &mut Canvas),
    ) {
        self.compose_labeled(out, region, &mut CellLabels, None, draw_content);
    }

    /// [`Desktop::compose_region`], with the title bars' text from `labels`.
    pub fn compose_labeled(
        &self,
        out: &mut Canvas,
        region: &Rect,
        labels: &mut impl Labels,
        menus: Option<(&Menus, Option<MenuOpen>)>,
        mut draw_content: impl FnMut(&Window, &mut Canvas),
    ) {
        let mut out = out.clipped(region);
        let clip = out.clip();
        if clip.is_empty() {
            return;
        }
        out.fill(self.theme.desktop);
        for w in self.iter() {
            if w.hidden || !w.outer().intersects(&clip) {
                continue;
            }
            let frame = w.frame();
            let active = self.focus == Some(w.id);
            let strength = if active {
                self.theme.shadow_active
            } else {
                self.theme.shadow_inactive
            };
            out.window_shadow(&frame, strength);
            out.round_rect(&frame, self.theme.window);
            // The title, centred, stops short of the buttons.
            let color = if active {
                self.theme.title_active
            } else {
                self.theme.title_inactive
            };
            let buttons = w.zoom_box().map_or(frame.x, |z| z.right());
            let mark = if w.hung { labels.width(HUNG_MARK) } else { 0 };
            let name = labels.width(w.title_str());
            let room = Rect::new(
                buttons,
                frame.y,
                (frame.right() - buttons).max(0) as u32,
                TITLE_H,
            );
            let x = (frame.x + (frame.w as i32 - name - mark) / 2).max(buttons + 4);
            let middle = frame.y + TITLE_H as i32 / 2;
            let mut titled = out.clipped(&room);
            labels.draw(&mut titled, w.title_str(), x, middle, color);
            if w.hung {
                labels.draw(&mut titled, HUNG_MARK, x + name, middle, color);
            }
            let circles = [
                (w.close_box(), self.theme.close),
                (w.minimize_box(), self.theme.minimize),
                (w.zoom_box(), self.theme.zoom),
            ];
            for (at, on) in circles {
                if let Some(b) = at {
                    out.circle(&b, if active { on } else { self.theme.circle_off });
                }
            }
            // The content, inside the frame's rounded bottom corners: row by row
            // there, whole above. The frame's own colour edges them.
            let c = w.content;
            let square = (c.h as i32 - RADIUS).max(0);
            draw_content(
                w,
                &mut out.clipped(&Rect::new(c.x, c.y, c.w, square as u32)),
            );
            for row in square..c.h as i32 {
                let from_bottom = (c.h as i32 - 1 - row).min(RADIUS - 1);
                let inset = corner_inset(from_bottom as usize);
                let line = Rect::new(
                    c.x + inset,
                    c.y + row,
                    (c.w as i32 - 2 * inset).max(0) as u32,
                    1,
                );
                draw_content(w, &mut out.clipped(&line));
            }
        }
        self.compose_panels(&mut out, labels, &clip);
        if let Some((menus, open)) = menus {
            self.compose_menus(&mut out, labels, menus, open);
        }
        out.cursor(self.cursor, self.pointer.0, self.pointer.1);
    }

    /// The bar and the dock, over the windows.
    fn compose_panels(&self, out: &mut Canvas, labels: &mut impl Labels, clip: &Rect) {
        let t = &self.theme;
        let bar = self.bar();
        if bar.intersects(clip) {
            out.fill_rect(&bar, t.bar);
            let middle = bar.y + bar.h as i32 / 2;
            let status = self.status_str();
            let at = bar.right() - t.bar_margin - labels.width(status);
            labels.draw(out, status, at, middle, t.bar_ink);
            if let Some(w) = self.focus.and_then(|id| self.get(id)) {
                // Short of the clock: a long title never covers the time.
                let room = Rect::new(0, 0, (at - t.bar_margin).max(0) as u32, bar.h);
                let mut inside = out.clipped(&room);
                labels.draw(&mut inside, w.panel_name(), t.bar_margin, middle, t.bar_ink);
            }
        }
        if self.notice_len > 0 {
            let place = self.notice_box();
            out.rounded(&place, RADIUS, t.bar);
            let text = core::str::from_utf8(&self.notice[..self.notice_len as usize]).unwrap_or("");
            let mut inside = out.clipped(&place);
            let middle = place.y + place.h as i32 / 2;
            labels.draw(&mut inside, text, place.x + t.bar_margin, middle, t.bar_ink);
        }
        if let Some((id, bounds)) = self.dock_hint() {
            if let Some(w) = self.get(id).filter(|_| bounds.intersects(clip)) {
                let pad = t.bar_margin;
                let width = (labels.width(w.panel_name()) + 2 * pad).max(0) as u32;
                let width = width.min(bounds.w);
                let bubble = Rect::new(
                    bounds.x + (bounds.w - width) as i32 / 2,
                    bounds.y,
                    width,
                    bounds.h,
                );
                out.rounded(&bubble, RADIUS, t.bar);
                let room = Rect::new(
                    bubble.x + pad,
                    bubble.y,
                    bubble.w.saturating_sub((2 * pad) as u32),
                    bubble.h,
                );
                draw_fitted_label(
                    &mut out.clipped(&room),
                    labels,
                    w.panel_name(),
                    &room,
                    bubble.y + bubble.h as i32 / 2,
                    t.bar_ink,
                );
            }
        }
        let (dock, places) = self.dock();
        if !dock.intersects(clip) {
            return;
        }
        out.rounded(&dock, RADIUS, t.dock);
        for (id, place) in places.iter().flatten() {
            let Some(w) = self.get(*id) else { continue };
            if self.focus == Some(*id) {
                out.rounded(place, RADIUS, t.dock_active);
            }
            let ink = if w.hidden {
                t.dock_quiet_ink
            } else {
                t.dock_ink
            };
            let mut inside = out.clipped(place);
            let size = t.dock_icon.min(place.w.saturating_sub(8));
            Icon::application(w.program_name()).draw(
                &mut inside,
                Rect::new(
                    place.x + (place.w - size) as i32 / 2,
                    place.y + t.dock_icon_top,
                    size,
                    size,
                ),
            );
            // A line for the active window, a dot for another visible window,
            // and an outlined dot for a minimized one: colour is not the only cue.
            let width = if self.focus == Some(*id) { 16 } else { 5 };
            let mark = Rect::new(
                place.x + (place.w as i32 - width) / 2,
                place.bottom() - t.dock_indicator_h as i32 - 3,
                width as u32,
                t.dock_indicator_h,
            );
            inside.rounded(&mark, 2, ink);
            if w.hidden && mark.w > 2 && mark.h > 2 {
                inside.fill_rect(
                    &Rect::new(mark.x + 1, mark.y + 1, mark.w - 2, mark.h - 2),
                    t.dock,
                );
            }
        }
    }
}

/// Centres a hover label and ends a shortened UTF-8 name with an ellipsis.
fn draw_fitted_label(
    out: &mut Canvas,
    labels: &mut impl Labels,
    name: &str,
    room: &Rect,
    middle: i32,
    ink: Color,
) {
    let width = labels.width(name);
    if width <= room.w as i32 {
        labels.draw(out, name, room.x + (room.w as i32 - width) / 2, middle, ink);
        return;
    }
    let dots = labels.width("…");
    let mut end = name.len();
    while end > 0 && labels.width(&name[..end]) + dots > room.w as i32 {
        end = name.floor_char_boundary(end - 1);
    }
    labels.draw(out, &name[..end], room.x, middle, ink);
    let x = room.x + labels.width(&name[..end]);
    labels.draw(out, "…", x, middle, ink);
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
    cursor: Cursor,
    bar: Rect,
    dock: Rect,
    dock_hint: Option<(u32, Rect)>,
    status: [u8; STATUS_MAX],
    notice: [u8; STATUS_MAX],
    notice_at: Rect,
}

impl Scene {
    fn iter(&self) -> impl Iterator<Item = &Window> + '_ {
        self.windows[..self.count].iter().flatten()
    }

    fn position(&self, id: u32) -> Option<usize> {
        self.iter().position(|w| w.id == id)
    }

    /// Adds to `damage` every screen area whose composed pixels may differ
    /// between `self` and `newer`: both cursors if the pointer moved or
    /// changed its picture; the old and new frame of a window that appeared, went away,
    /// moved, was retitled or gained or lost the focus; and, for two windows
    /// whose stacking order swapped, the area where they overlap. Content
    /// changes are not part of a scene: the compositor adds the content
    /// rectangle of a window its client presented.
    pub fn damage(&self, newer: &Scene, damage: &mut Damage) {
        if (self.pointer, self.cursor) != (newer.pointer, newer.cursor) {
            damage.add(&self.cursor.rect(self.pointer.0, self.pointer.1));
            damage.add(&newer.cursor.rect(newer.pointer.0, newer.pointer.1));
        }
        // The bar names the focused window and the dock shows each one: a window that
        // came, went, changed its title or hid changes them.
        let same_for_panels = self.count == newer.count
            && self.focus == newer.focus
            && self.iter().all(|old| {
                newer.iter().any(|new| {
                    new.id == old.id
                        && new.title == old.title
                        && new.hidden == old.hidden
                        && new.program == old.program
                        && new.program_len == old.program_len
                })
            });
        if !same_for_panels || self.status != newer.status {
            damage.add(&self.bar);
            damage.add(&newer.bar);
        }
        if !same_for_panels {
            damage.add(&self.dock);
            damage.add(&newer.dock);
        }
        if !same_for_panels || self.dock_hint != newer.dock_hint {
            for (_, area) in [self.dock_hint, newer.dock_hint].into_iter().flatten() {
                damage.add(&area);
            }
        }
        if self.notice != newer.notice {
            damage.add(&self.notice_at);
            damage.add(&newer.notice_at);
        }
        let focused = |s: &Scene, id: u32| s.focus == Some(id);
        for old in self.iter() {
            match newer.iter().find(|w| w.id == old.id) {
                None => damage.add(&old.outer()),
                Some(new) => {
                    if new != old || focused(self, old.id) != focused(newer, old.id) {
                        damage.add(&old.outer());
                        damage.add(&new.outer());
                    }
                }
            }
        }
        for new in newer.iter() {
            if self.position(new.id).is_none() {
                damage.add(&new.outer());
            }
        }
        for (i, a) in self.iter().enumerate() {
            for b in self.iter().skip(i + 1) {
                let (Some(na), Some(nb)) = (newer.position(a.id), newer.position(b.id)) else {
                    continue;
                };
                if na > nb {
                    damage.add(&a.outer().intersect(&b.outer()));
                    let (wa, wb) = (newer.windows[na], newer.windows[nb]);
                    if let (Some(wa), Some(wb)) = (wa, wb) {
                        damage.add(&wa.outer().intersect(&wb.outer()));
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

/// Who may read the clipboard now: what the owner copied may be a password, so a program
/// reads it only with the focus and only when the owner pressed Ctrl+V in it, once for
/// each press. No program can watch the clipboard from behind, nor take it unasked.
#[derive(Debug, Default, Clone, Copy)]
pub struct PasteGrant(Option<u32>);

/// The byte a window gets for Ctrl+V.
const CTRL_V: u8 = 0x16;

impl PasteGrant {
    /// A key went to the focused window as these bytes; `held` when it repeats on its
    /// own, which is not a press. A key with no bytes (a release, a modifier) changes
    /// nothing.
    pub fn on_key(&mut self, focus: u32, bytes: &[u8], held: bool) {
        if bytes.is_empty() {
            return;
        }
        self.0 = (bytes == [CTRL_V] && !held).then_some(focus);
    }

    /// The owner chose Paste in this window's menu: as good as pressing Ctrl+V there.
    pub fn on_menu(&mut self, focus: u32) {
        self.0 = Some(focus);
    }

    /// The focus moved.
    pub fn on_focus_change(&mut self) {
        self.0 = None;
    }

    /// Whether `window` may read now; this uses up the press.
    pub fn take(&mut self, window: u32, focus: Option<u32>) -> bool {
        let granted = self.0 == Some(window) && focus == Some(window);
        if granted {
            self.0 = None;
        }
        granted
    }
}

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

    #[test]
    fn the_dock_shows_each_program_and_panels_take_the_clicks() {
        let mut d = Desktop::new(800, 600);
        let (empty, places) = d.dock();
        assert!(empty.is_empty(), "nothing running, nothing shown");
        assert_eq!(places, [None; MAX_WINDOWS]);
        let a = d.create(100, 100, "A").unwrap();
        let (dock, places) = d.dock();
        let (id, place) = places[0].unwrap();
        assert_eq!(id, a);
        let t = Theme::LIGHT;
        assert_eq!(dock.bottom(), 600 - t.dock_margin);
        assert_eq!(dock.x + dock.right(), 800, "centred");
        assert_eq!(d.hit(place.x + 1, place.y + 1), Hit::Dock { id: a });
        assert_eq!(d.hit(dock.x + 1, dock.y + 1), Hit::Panel);
        assert_eq!(d.hit(300, 3), Hit::Panel, "the bar");
        // A window under them does not take a click on them.
        d.move_window(a, 0, 500);
        assert_eq!(d.hit(place.x + 1, place.y + 1), Hit::Dock { id: a });
        // Hiding keeps its place, and marks the dock as changed.
        let before = d.scene();
        d.set_hidden(a, true);
        assert_eq!(d.dock().1[0], Some((a, place)));
        let mut damage = Damage::new(Rect::new(0, 0, 800, 600));
        before.damage(&d.scene(), &mut damage);
        assert!(damage.rects().iter().any(|r| r.intersects(&place)));
        // Many at once stay on the screen and apart.
        for _ in 0..7 {
            d.create(100, 100, "x").unwrap();
        }
        let (_, places) = d.dock();
        let mut last_right = 0;
        for (_, p) in places.into_iter().flatten() {
            assert!(p.x >= last_right && p.right() <= 800, "{p:?}");
            last_right = p.right();
        }
        assert_eq!(places.iter().flatten().count(), 8);
    }

    #[test]
    fn icons_use_the_manifest_and_identity_changes_repaint_panels() {
        let mut d = Desktop::new(800, 600);
        let id = d.create(100, 100, "terminal").unwrap();
        assert_eq!(
            Icon::application(d.get(id).unwrap().program_name()),
            Icon::Application
        );
        let before = d.scene();
        d.set_program(id, b"files");
        let w = d.get(id).unwrap();
        assert_eq!(w.panel_name(), "Files");
        assert_eq!(Icon::application(w.program_name()), Icon::Files);
        let mut damage = Damage::new(Rect::new(0, 0, 800, 600));
        before.damage(&d.scene(), &mut damage);
        assert!(damage.rects().iter().any(|r| r.intersects(&d.dock().0)));
        assert!(damage.rects().iter().any(|r| r.intersects(&d.bar())));
    }

    #[test]
    fn dock_ellipsis_keeps_utf8_and_fits_the_available_width() {
        struct Recorded(Vec<std::string::String>);
        impl Labels for Recorded {
            fn width(&mut self, text: &str) -> i32 {
                text.chars().count() as i32 * 8
            }
            fn draw(&mut self, _: &mut Canvas, text: &str, _: i32, _: i32, _: Color) {
                self.0.push(text.into());
            }
        }
        let mut labels = Recorded(Vec::new());
        let mut buf = Vec::new();
        let mut out = canvas(&mut buf, 40, 20);
        draw_fitted_label(
            &mut out,
            &mut labels,
            "ééééé",
            &Rect::new(0, 0, 32, 20),
            10,
            0,
        );
        assert_eq!(labels.0, ["ééé", "…"]);
    }

    #[test]
    fn windows_fit_between_the_bar_and_the_dock() {
        let mut d = Desktop::new(1280, 800);
        let work = d.work();
        assert_eq!((work.y, work.h), (24, 800 - 24 - 60));
        let full = snap_content(Snap::Full, &work);
        assert_eq!((full.x, full.y, full.w, full.h), (0, 24 + 28, 1280, 688));
        let low = snap_content(Snap::BottomRight, &work);
        assert_eq!(low.bottom(), work.bottom());
        // A title bar never goes under the bar.
        let a = d.create(100, 100, "A").unwrap();
        d.move_window(a, 10, 0);
        assert_eq!(d.get(a).unwrap().frame().y, 24);
    }

    #[test]
    fn the_clock_reads_as_a_calendar_does() {
        let text = |minute| {
            let mut out = [0; STATUS_MAX];
            let n = clock_text(minute, &mut out);
            std::string::String::from_utf8(out[..n].to_vec()).unwrap()
        };
        assert_eq!(text(0), "Thu 1 Jan  00:00");
        // 2026-11-06 23:40 UTC.
        assert_eq!(text(29_900_140), "Fri 6 Nov  23:40");
        // A leap day, and the last minute of a day.
        assert_eq!(text(951_782_400 / 60 + 1439), "Tue 29 Feb  23:59");
        // Before the epoch still reads.
        assert_eq!(text(-1), "Wed 31 Dec  23:59");
    }

    fn sample_menus() -> Menus {
        let mut m = Menus::default();
        assert!(m.add(0, "File", 0));
        assert!(m.add(0, "Open\tCtrl+O", 1));
        assert!(m.add(0, "Save\tCtrl+S", 2));
        assert!(m.add(1, "Edit", 0));
        assert!(m.add(1, "Paste\tCtrl+V", 3));
        assert!(m.add(1, "Pasted things", 4));
        m
    }

    #[test]
    fn menus_keep_within_their_bounds() {
        let mut m = Menus::default();
        assert!(!m.add(0, "Item before its menu", 1));
        assert!(!m.add(1, "A menu out of order", 0));
        assert!(m.add(0, "File", 0));
        for i in 0..MAX_MENU_ITEMS {
            assert!(m.add(0, "x", i as u32 + 1));
        }
        assert!(!m.add(0, "one too many", 99));
        for i in 1..MAX_MENUS {
            assert!(m.add(i, "m", 0));
        }
        assert!(!m.add(MAX_MENUS, "m", 0));
        // A label too long is cut between characters.
        let mut long = Menus::default();
        assert!(long.add(0, &"é".repeat(30), 0));
        assert!(long.menus[0].title.text().chars().all(|c| c == 'é'));
    }

    #[test]
    fn only_the_item_named_paste_opens_the_clipboard() {
        let m = sample_menus();
        assert_eq!(m.chosen(0, 0), Some((1, false)));
        assert_eq!(m.chosen(1, 0), Some((3, true)));
        assert_eq!(
            m.chosen(1, 1),
            Some((4, false)),
            "a name that only starts so"
        );
        assert_eq!(m.chosen(1, 2), None);
        assert_eq!(m.chosen(5, 0), None);
    }

    #[test]
    fn menus_are_found_where_they_are_drawn() {
        let mut d = Desktop::new(800, 600);
        d.create(100, 100, "App").unwrap();
        let m = sample_menus();
        let mut l = CellLabels;
        let titles = d.menu_titles(&m, &mut l);
        let file = titles[0];
        let edit = titles[1];
        assert!(file.right() <= edit.x);
        assert_eq!(
            d.menu_at(&m, &mut l, None, file.x + 1, 5),
            MenuHit::Title(0)
        );
        assert_eq!(
            d.menu_at(&m, &mut l, None, edit.x + 1, 5),
            MenuHit::Title(1)
        );
        assert_eq!(d.menu_at(&m, &mut l, None, 700, 5), MenuHit::None);
        let area = d.menu_box(&m, &mut l, 0);
        assert_eq!(area.y, d.bar().h as i32);
        let row = d.menu_row(&area, 1);
        let (x, y) = (row.x + 2, row.y + 2);
        assert_eq!(d.menu_at(&m, &mut l, Some(0), x, y), MenuHit::Item(0, 1));
        assert_eq!(d.menu_at(&m, &mut l, None, x, y), MenuHit::None, "closed");
        // On a small screen the menu stays on it.
        let mut small = Desktop::new(120, 100);
        small.create(10, 10, "App").unwrap();
        let area = small.menu_box(&m, &mut l, 1);
        assert!(area.x >= 0 && (area.right() <= 120 || area.x == 0));
        // Drawing it stays inside what damage covers: the bar and the box.
        let mut buf = Vec::new();
        let mut out = canvas(&mut buf, 800, 600);
        d.compose_labeled(
            &mut out,
            &Rect::new(0, 0, 800, 600),
            &mut l,
            Some((
                &m,
                Some(MenuOpen {
                    menu: 0,
                    item: Some(1),
                }),
            )),
            |_, c| c.fill(0xffffff),
        );
        let row = d.menu_row(&d.menu_box(&m, &mut l, 0), 1);
        assert_eq!(
            out.get(row.x + row.w as i32 - 2, row.y + 2),
            Some(d.theme.menu_accent)
        );
    }

    #[test]
    fn a_notice_shows_marks_its_place_and_a_click_finds_it() {
        let mut d = Desktop::new(800, 600);
        let at = d.notice_box();
        assert_eq!(d.hit(at.x + 1, at.y + 1), Hit::Desktop, "none on show");
        let before = d.scene();
        d.set_notice("Hot");
        assert_eq!(d.hit(at.x + 1, at.y + 1), Hit::Notice);
        let mut damage = Damage::new(Rect::new(0, 0, 800, 600));
        before.damage(&d.scene(), &mut damage);
        assert_eq!(damage.rects(), [at]);
        // On a screen narrower than a notice it stays on the screen.
        let small = Desktop::new(200, 100);
        assert_eq!(small.notice_box().x, 0);
        assert_eq!(small.notice_box().w, 200);
    }

    #[test]
    fn menu_titles_never_reach_the_clock() {
        let mut d = Desktop::new(300, 200);
        d.create(10, 10, "App").unwrap();
        d.set_status("Sat 7 Nov  00:30");
        let mut m = Menus::default();
        for i in 0..MAX_MENUS {
            assert!(m.add(i, "A rather long menu title", 0));
        }
        let mut l = CellLabels;
        let limit = 300 - 2 * d.theme.bar_margin - l.width("Sat 7 Nov  00:30");
        for r in d.menu_titles(&m, &mut l) {
            assert!(r.is_empty() || r.right() <= limit, "{r:?}");
        }
        // One that is not shown cannot be clicked.
        assert_eq!(d.menu_at(&m, &mut l, None, 299, 5), MenuHit::None);
    }

    #[test]
    fn a_window_cannot_size_its_title_bar_under_the_bar() {
        let mut d = Desktop::new(800, 600);
        let a = d.create(100, 100, "A").unwrap();
        assert!(d.resize(a, 800, 600));
        let t = d.theme;
        assert_eq!(d.get(a).unwrap().frame().y, t.bar_h as i32);
        let close = d.get(a).unwrap().close_box().unwrap();
        let (x, y) = (close.x + 2, close.y + 2);
        assert_eq!(d.hit(x, y), Hit::Close { id: a });
    }

    #[test]
    fn the_panels_name_a_program_by_its_manifest_not_its_title() {
        let mut d = Desktop::new(800, 600);
        let a = d.create(100, 100, "Settings").unwrap();
        assert_eq!(
            d.get(a).unwrap().panel_name(),
            "Settings",
            "no manifest known"
        );
        d.set_program(a, b"pista");
        assert_eq!(d.get(a).unwrap().panel_name(), "Pista");
        assert_eq!(d.get(a).unwrap().title_str(), "Settings");
    }

    #[test]
    fn a_new_status_marks_the_bar_only() {
        let mut d = Desktop::new(800, 600);
        let before = d.scene();
        d.set_status("12:00");
        let mut damage = Damage::new(Rect::new(0, 0, 800, 600));
        before.damage(&d.scene(), &mut damage);
        assert_eq!(damage.rects(), [d.bar()]);
        // Too long is cut between characters.
        d.set_status(&"é".repeat(40));
        assert!(d.status_str().chars().all(|c| c == 'é'));
    }

    #[test]
    fn only_a_press_in_the_focused_window_opens_the_clipboard() {
        let mut g = PasteGrant::default();
        // Nothing pressed: nobody reads.
        assert!(!g.take(1, Some(1)));
        // A press in window 1 is for window 1, once.
        g.on_key(1, &[CTRL_V], false);
        assert!(!g.take(2, Some(1)), "another window");
        assert!(!g.take(1, Some(2)), "it has lost the focus");
        assert!(g.take(1, Some(1)));
        assert!(!g.take(1, Some(1)), "one read for one press");
        // Letting the keys go keeps it until it is read; another key ends it.
        g.on_key(1, &[CTRL_V], false);
        g.on_key(1, &[], false);
        assert!(g.take(1, Some(1)));
        // Holding the key is one press, not one for each repeat.
        g.on_key(1, &[CTRL_V], true);
        assert!(!g.take(1, Some(1)));
        g.on_key(1, &[CTRL_V], false);
        g.on_key(1, b"x", false);
        assert!(!g.take(1, Some(1)));
        // Moving away ends it, even coming back.
        g.on_key(1, &[CTRL_V], false);
        g.on_focus_change();
        assert!(!g.take(1, Some(1)));
        // Choosing Paste in its menu opens it, once.
        g.on_menu(1);
        assert!(g.take(1, Some(1)));
        assert!(!g.take(1, Some(1)));
    }

    #[test]
    fn the_frame_has_three_circles_on_its_left_and_a_title_bar() {
        let mut d = Desktop::new(800, 600);
        let id = d.create(300, 200, "w").unwrap();
        let w = *d.get(id).unwrap();
        let f = w.frame();
        assert_eq!((f.w, f.h), (300, 200 + TITLE_H));
        let at = |b: Rect| (b.x + 6, b.y + 6);
        let (close, min, zoom) = (
            w.close_box().unwrap(),
            w.minimize_box().unwrap(),
            w.zoom_box().unwrap(),
        );
        assert_eq!(min.x - close.x, CIRCLE_STEP);
        assert_eq!(zoom.x - min.x, CIRCLE_STEP);
        assert_eq!(d.hit(at(close).0, at(close).1), Hit::Close { id });
        assert_eq!(d.hit(at(min).0, at(min).1), Hit::Minimize { id });
        assert_eq!(d.hit(at(zoom).0, at(zoom).1), Hit::Zoom { id });
        assert_eq!(d.hit(f.x + 150, f.y + 4), Hit::TitleBar { id });
        assert_eq!(
            d.hit(f.x + 10, w.content.y + 5),
            Hit::Window { id, x: 10, y: 5 }
        );
    }

    #[test]
    fn only_a_window_that_answers_has_edges() {
        let mut d = Desktop::new(800, 600);
        let id = d.create(300, 200, "w").unwrap();
        let f = d.get(id).unwrap().frame();
        let right = (f.right() + 2, f.y + 50);
        assert_eq!(d.hit(right.0, right.1), Hit::Desktop);
        d.set_resizable(id);
        let edges = edge::RIGHT;
        assert_eq!(d.hit(right.0, right.1), Hit::Edge { id, edges });
        let edges = edge::LEFT | edge::TOP;
        assert_eq!(d.hit(f.x - 2, f.y - 2), Hit::Edge { id, edges });
        assert_eq!(d.hit(f.right() + d.grips.band, f.y + 50), Hit::Desktop);
        // The notch of a rounded corner grabs it; the same pixel of a window that
        // cannot be resized is still its own.
        let corner = (f.right() - 1, f.bottom() - 1);
        let edges = edge::RIGHT | edge::BOTTOM;
        assert_eq!(d.hit(corner.0, corner.1), Hit::Edge { id, edges });
        let fixed = d.create(300, 200, "fixed").unwrap();
        let f = d.get(fixed).unwrap().frame();
        let (x, y) = (f.right() - 1, f.bottom() - 1);
        assert!(matches!(d.hit(x, y), Hit::Window { id, .. } if id == fixed));
    }

    #[test]
    fn the_drag_bounds_are_the_screen_under_the_bar() {
        let d = Desktop::new(800, 600);
        let bar = Theme::LIGHT.bar_h;
        assert_eq!(d.drag_bounds(), Rect::new(0, bar as i32, 800, 600 - bar));
        assert_eq!(Desktop::new(800, 10).drag_bounds().h, 0, "a screen all bar");
    }

    #[test]
    fn a_hidden_window_is_not_there() {
        let mut d = Desktop::new(800, 600);
        let (a, b) = (
            d.create(300, 200, "a").unwrap(),
            d.create(300, 200, "b").unwrap(),
        );
        d.raise(b);
        let f = d.get(b).unwrap().frame();
        let probe = (f.x + 20, f.y + 40);
        assert!(matches!(d.hit(probe.0, probe.1), Hit::Window { id, .. } if id == b));
        d.set_hidden(b, true);
        assert_eq!(d.focus, None);
        assert_eq!(d.front(), Some(a));
        assert!(!matches!(d.hit(probe.0, probe.1), Hit::Window { id, .. } if id == b));
    }

    #[test]
    fn the_screens_edges_snap_to_halves_and_quarters() {
        let (w, h) = (1366, 768);
        assert_eq!(snap_at(0, 300, w, h), Some(Snap::Left));
        assert_eq!(snap_at(1365, 300, w, h), Some(Snap::Right));
        assert_eq!(snap_at(0, 0, w, h), Some(Snap::TopLeft));
        assert_eq!(snap_at(1365, 767, w, h), Some(Snap::BottomRight));
        assert_eq!(snap_at(600, 0, w, h), Some(Snap::Full));
        assert_eq!(snap_at(600, 300, w, h), None);
        assert_eq!(snap_at(600, 767, w, h), None, "the bottom alone is no snap");
        // The content leaves room for its title bar, and quarters tile.
        let screen = Rect::new(0, 0, w, h);
        let left = snap_content(Snap::Left, &screen);
        let right = snap_content(Snap::Right, &screen);
        assert_eq!(left, Rect::new(0, TITLE_H as i32, 683, 768 - TITLE_H));
        assert_eq!(left.right(), right.x);
        assert_eq!(right.right(), 1366);
        let low = snap_content(Snap::BottomLeft, &screen);
        assert_eq!(low.y, 384 + TITLE_H as i32);
        assert_eq!(low.bottom(), 768);
    }

    /// To look at a frame: `GFX_DUMP=file cargo test -p aricode-gfx dump -- --ignored`.
    #[test]
    #[ignore]
    fn dump_desktop() {
        let path = std::env::var("GFX_DUMP").expect("GFX_DUMP");
        let (w, h) = (800, 500);
        let mut d = Desktop::new(w, h);
        let a = d.create(420, 260, "Inactive window").unwrap();
        let b = d.create(420, 260, "The window in use").unwrap();
        d.raise(a);
        d.raise(b);
        d.pointer = (640, 300);
        let mut px = std::vec![0u32; (w * h) as usize];
        let mut out = Canvas::new(&mut px, w, h, w).unwrap();
        d.compose(&mut out, |_, c| c.fill(0xFFFFFF));
        let bytes: Vec<u8> = px
            .iter()
            .flat_map(|p| p.to_be_bytes()[1..].to_vec())
            .collect();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn corners_are_rounded_and_the_shadow_stays_outside() {
        let mut d = Desktop::new(200, 200);
        let id = d.create(100, 60, "w").unwrap();
        d.raise(id);
        let mut px = std::vec![0u32; 200 * 200];
        let mut out = Canvas::new(&mut px, 200, 200, 200).unwrap();
        d.compose(&mut out, |_, _| {});
        let f = d.get(id).unwrap().frame();
        let t = d.theme;
        // The corner's pixel is not the window; a little inside it is.
        assert_ne!(out.get(f.x, f.y), Some(t.window));
        assert_eq!(out.get(f.x + 12, f.y + 1), Some(t.window));
        assert_eq!(out.get(f.x + 1, f.y + 12), Some(t.window));
        // Just outside the frame is the desktop, darker; far away it is not.
        let near = out.get(f.x - 1, f.y + 40).unwrap();
        assert_ne!(near, t.desktop);
        assert_eq!(near, blend(t.desktop, 0, near_strength(&t)));
        assert_eq!(out.get(f.x - SHADOW - 1, f.y + 40), Some(t.desktop));
    }

    /// The shadow's strength one pixel outside a straight edge.
    fn near_strength(t: &Theme) -> u32 {
        (SHADOW_FALL[0][RADIUS as usize + 1] as u32 * t.shadow_active) >> 8
    }

    #[test]
    fn a_resized_window_keeps_its_corner() {
        let mut d = Desktop::new(800, 600);
        let id = d.create(100, 50, "w").unwrap();
        let corner = (d.get(id).unwrap().content.x, d.get(id).unwrap().content.y);
        assert!(d.resize(id, 300, 200));
        let c = d.get(id).unwrap().content;
        assert_eq!((c.x, c.y, c.w, c.h), (corner.0, corner.1, 300, 200));
        assert!(!d.resize(id, 0, 200));
        // Too far right and down for its new size: it moves back on screen.
        assert!(d.resize(id, 790, 590));
        let c = d.get(id).unwrap().content;
        // Its right side at the screen's; the title bar stays below the top.
        assert_eq!((c.x, c.y), (10, Theme::LIGHT.bar_h as i32 + TITLE_H as i32));
        let c = Rect::new(100, 100, 0, 0);
        let moved = d.move_window(id, c.x, c.y) && d.resize(id, 750, 500);
        assert!(moved);
        let c = d.get(id).unwrap().content;
        assert!(c.right() + BORDER as i32 <= 800 && c.bottom() + BORDER as i32 <= 600);
        assert!(!d.resize(id + 1, 10, 10));
    }

    #[test]
    fn pixel_masks_place_each_channel() {
        let c = rgb(0x12, 0x34, 0x56);
        // The two layouts UEFI names, written as masks.
        let bgr = PixelMasks::new(0xFF_0000, 0xFF00, 0xFF).unwrap();
        assert_eq!(bgr.encode(c), c);
        let rgb_order = PixelMasks::new(0xFF, 0xFF00, 0xFF_0000).unwrap();
        assert_eq!(rgb_order.encode(c), 0x56_3412);
        // 10 bits per channel: full scale reaches the top of the field.
        let deep = PixelMasks::new(0x3FF0_0000, 0xF_FC00, 0x3FF).unwrap();
        assert_eq!(deep.encode(rgb(0xFF, 0, 0)), 0xFF << 2 << 20);
        assert_eq!(deep.encode(rgb(0, 0, 0x80)), 0x80 << 2);
    }

    #[test]
    fn pixel_masks_refuse_what_they_cannot_place() {
        assert_eq!(PixelMasks::new(0, 0xFF00, 0xFF), None, "empty");
        assert_eq!(PixelMasks::new(0xF0F0, 0xFF00, 0xFF), None, "with holes");
        assert_eq!(PixelMasks::new(0x1_FFFF, 0xFF00, 0xFF), None, "too wide");
    }

    #[test]
    fn set_is_clipped() {
        let mut px = vec![0u32; 4 * 4];
        let mut c = Canvas::new(&mut px, 4, 4, 4).unwrap();
        c.set(1, 2, 7);
        c.set(-1, 0, 9);
        c.set(4, 0, 9);
        let mut inner = c.clipped(&Rect::new(0, 0, 2, 2));
        inner.set(3, 3, 9);
        assert_eq!(c.get(1, 2), Some(7));
        assert_eq!(px.iter().filter(|&&p| p == 9).count(), 0);
    }

    #[test]
    fn only_a_just_opened_program_takes_the_focus() {
        assert!(new_window_takes_focus(true, 0, false));
        assert!(new_window_takes_focus(true, FOCUS_GRACE_MS, false));
        assert!(!new_window_takes_focus(false, 10, false), "a second window");
        assert!(
            !new_window_takes_focus(true, FOCUS_GRACE_MS + 1, false),
            "too late"
        );
        assert!(
            !new_window_takes_focus(true, 10, true),
            "the owner typed since"
        );
    }
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
        c.cursor(Cursor::Arrow, 35, 15);
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
        assert_eq!(wa.content, Rect::new(40, 68, 200, 100));
        assert_eq!(wb.content, Rect::new(80, 108, 200, 100));
        // B is on top in the overlapping area.
        assert_eq!(
            d.hit(100, 120),
            Hit::Window {
                id: b,
                x: 20,
                y: 12
            }
        );
        assert_eq!(
            d.hit(50, 78),
            Hit::Window {
                id: a,
                x: 10,
                y: 10
            }
        );
        assert_eq!(d.hit(600, 400), Hit::Desktop);
        // A's title bar is the desktop's to answer, not its program's.
        assert_eq!(d.hit(150, 45), Hit::TitleBar { id: a });
        // The close box, first of the buttons on the left of A's title bar.
        let close = d.get(a).unwrap().close_box().unwrap();
        let frame = d.get(a).unwrap().frame();
        assert_eq!(close.x, frame.x + 8);
        assert_eq!(d.hit(close.x + 1, close.y + 1), Hit::Close { id: a });
        assert_ne!(d.hit(close.x - 1, close.y + 1), Hit::Close { id: a });
        // A window too narrow for the buttons has none.
        let thin = d.create(20, 50, "t").unwrap();
        assert!(d.get(thin).unwrap().close_box().is_none());
        d.remove(thin);
        d.raise(a);
        assert_eq!(d.focus, Some(a));
        assert_eq!(
            d.hit(100, 120),
            Hit::Window {
                id: a,
                x: 60,
                y: 52
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
            screen.get(wb.content.x + 70, wb.content.y - 5),
            Some(Theme::LIGHT.window)
        );
        assert_eq!(
            screen.get(wa.content.x + 1, wa.content.y - 5),
            Some(Theme::LIGHT.window)
        );
        let right = screen.width as i32 - 1;
        assert_eq!(screen.get(right, 30), Some(Theme::LIGHT.desktop));
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
            v.cursor(Cursor::Arrow, 20, 12);
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
        presented: Vec<u32>,
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
                presented: Vec::new(),
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

        /// As the compositor: what a program presented is repainted where its
        /// window is when the frame is made, not where it was then.
        fn present(&mut self, id: u32) {
            self.presented.push(id);
        }

        /// One composed frame, as the compositor does it; returns the
        /// pixels flushed.
        fn frame(&mut self) -> u64 {
            let scene = self.desktop.scene();
            self.shown.damage(&scene, &mut self.damage);
            self.shown = scene;
            for id in core::mem::take(&mut self.presented) {
                if let Some(w) = self.desktop.get(id) {
                    self.damage.add(&w.content);
                }
            }
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
                _ => {
                    let shapes = [
                        Cursor::Arrow,
                        Cursor::LeftRight,
                        Cursor::UpDown,
                        Cursor::Falling,
                        Cursor::Rising,
                    ];
                    self.desktop.cursor = shapes[rng.below(shapes.len() as u32) as usize];
                }
            }
        }
    }

    #[test]
    fn dock_icons_repaint_after_identity_focus_hide_and_slot_reuse() {
        for theme in [Theme::LIGHT, Theme::DARK] {
            let mut m = Model::new(800, 600);
            m.desktop.theme = theme;
            m.damage = Damage::full(m.desktop.screen);
            let a = m.create(180, 100, "terminal", 0xffffff);
            let b = m.create(180, 100, "files", 0xeeeeee);
            m.frame();
            for name in [
                "terminal", "files", "editor", "settings", "pista", "launcher", "other",
            ] {
                m.desktop.set_program(a, name.as_bytes());
                m.desktop.raise(a);
                m.frame();
                assert!(m.back == m.full(), "identity {name}");
                m.desktop.set_hidden(a, true);
                m.desktop.raise(b);
                m.frame();
                assert!(m.back == m.full(), "hidden {name}");
                m.desktop.set_hidden(a, false);
                m.desktop.raise(a);
                m.frame();
                assert!(m.back == m.full(), "restored {name}");
            }
            // The count stays the same across this frame, but an icon and slot change.
            m.desktop.remove(a);
            m.contents.retain(|(id, _)| *id != a);
            let c = m.create(180, 100, "new", 0xdddddd);
            m.desktop.set_program(c, b"files");
            m.frame();
            assert!(m.back == m.full(), "slot reuse");
            assert_eq!(m.frame(), 0, "a quiet desktop costs no repaint");
        }
    }

    #[test]
    fn compact_dock_hover_is_bounded_and_repaints_when_entering_switching_and_leaving() {
        let mut m = Model::new(800, 600);
        for name in [
            "launcher", "terminal", "files", "editor", "settings", "pista", "other", "last",
        ] {
            let id = m.create(100, 80, name, 0xffffff);
            m.desktop.set_program(id, name.as_bytes());
        }
        let (dock, places) = m.desktop.dock();
        assert_eq!((dock.w, dock.h), (388, 48));
        m.frame();
        for (id, place) in places.into_iter().flatten() {
            m.desktop.pointer = (place.x + place.w as i32 / 2, place.y + 10);
            let (hint_id, hint) = m.desktop.dock_hint().unwrap();
            assert_eq!(hint_id, id);
            assert!(hint.x >= 0 && hint.right() <= 800 && hint.bottom() < dock.y);
            assert_eq!(
                m.desktop.hit(m.desktop.pointer.0, m.desktop.pointer.1),
                Hit::Dock { id }
            );
            m.frame();
            assert!(m.back == m.full(), "hover {id}");
            m.desktop.set_program(id, b"renamed_app");
            m.frame();
            assert!(m.back == m.full(), "identity while hovered {id}");
        }
        m.desktop.pointer = (10, 10);
        assert_eq!(m.desktop.dock_hint(), None);
        m.frame();
        assert!(m.back == m.full(), "leaving erases the bubble");
        assert_eq!(m.frame(), 0);
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
        let outer = d.get(id).unwrap().outer();
        d.pointer = (190, 90);
        let mut buf = Vec::new();
        let mut out = canvas(&mut buf, 200, 100);
        d.compose(&mut out, |_, c| c.fill(0xffffff));
        for y in 0..100 {
            for x in 0..200 {
                let panels = d.bar().contains(x, y) || d.dock().0.contains(x, y);
                if !panels && !outer.contains(x, y) && !Cursor::Arrow.rect(190, 90).contains(x, y) {
                    assert_eq!(out.get(x, y), Some(Theme::LIGHT.desktop), "({x},{y})");
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
            (0, Theme::LIGHT.bar_h as i32)
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
            let outer = m.desktop.get(a).unwrap().outer();
            assert_eq!(moved, (outer.w as u64 + 10) * (outer.h as u64 + 10));
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
