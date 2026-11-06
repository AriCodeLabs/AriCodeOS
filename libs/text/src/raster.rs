// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Coverage of a filled outline, by signed area: each edge adds to the cells it
//! crosses how much of them lies to its right, and a running sum along each row gives
//! the coverage. This is how font-rs and fontdue do it, and it needs no sorting, no
//! recursion and nothing on the stack. A host test compares it with zeno, pixel by
//! pixel, which stays as the reference and is not shipped: zeno keeps 24 KB of working
//! data on the stack, in a program that has 64 KB.

use alloc::vec;
use alloc::vec::Vec;

/// Line segments, in pixels with y down, from flattening an outline.
#[derive(Default)]
pub struct Outline {
    lines: Vec<[f32; 4]>,
    start: (f32, f32),
    at: (f32, f32),
    pub min: (f32, f32),
    pub max: (f32, f32),
}

/// How far a curve may stray from its lines, in pixels.
const TOLERANCE: f32 = 0.1;

fn sqrt(v: f32) -> f32 {
    // Newton's method from a power of two: enough for a count of segments.
    let mut x = if v > 1.0 { v } else { 1.0 };
    for _ in 0..8 {
        x = (x + v / x) * 0.5;
    }
    x
}

const fn floor(v: f32) -> i32 {
    let t = v as i32;
    if (t as f32) > v {
        t - 1
    } else {
        t
    }
}

const fn ceil(v: f32) -> i32 {
    let t = v as i32;
    if (t as f32) < v {
        t + 1
    } else {
        t
    }
}

const fn abs(v: f32) -> f32 {
    if v < 0.0 {
        -v
    } else {
        v
    }
}

impl Outline {
    pub fn new() -> Self {
        Self {
            min: (f32::MAX, f32::MAX),
            max: (f32::MIN, f32::MIN),
            ..Self::default()
        }
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = (x, y);
        self.at = (x, y);
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        let (ax, ay) = self.at;
        self.lines.push([ax, ay, x, y]);
        self.min = (self.min.0.min(ax).min(x), self.min.1.min(ay).min(y));
        self.max = (self.max.0.max(ax).max(x), self.max.1.max(ay).max(y));
        self.at = (x, y);
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (ax, ay) = self.at;
        // A quadratic strays by a quarter of its control point's distance from the
        // chord's middle, and by a quarter of that with each split.
        let (dx, dy) = (ax - 2.0 * cx + x, ay - 2.0 * cy + y);
        let stray = (dx * dx + dy * dy) / 16.0;
        let n = ceil(sqrt(sqrt(stray) / TOLERANCE)).clamp(1, 32);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.line_to(
                u * u * ax + 2.0 * u * t * cx + t * t * x,
                u * u * ay + 2.0 * u * t * cy + t * t * y,
            );
        }
    }

    pub fn curve_to(&mut self, c0: (f32, f32), c1: (f32, f32), x: f32, y: f32) {
        let (ax, ay) = self.at;
        let d0 = abs(ax - 2.0 * c0.0 + c1.0) + abs(ay - 2.0 * c0.1 + c1.1);
        let d1 = abs(c0.0 - 2.0 * c1.0 + x) + abs(c0.1 - 2.0 * c1.1 + y);
        let n = ceil(sqrt(0.75 * d0.max(d1) / TOLERANCE)).clamp(1, 32);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.line_to(
                a * ax + b * c0.0 + c * c1.0 + d * x,
                a * ay + b * c0.1 + c * c1.1 + d * y,
            );
        }
    }

    /// Back to the contour's start, if it is not there.
    pub fn close(&mut self) {
        if self.at != self.start {
            let (x, y) = self.start;
            self.line_to(x, y);
        }
    }

    /// Coverage in 0..=255, row by row, with where its top-left pixel falls, moved
    /// right by `shift`. An empty outline gives an empty mask.
    pub fn coverage(&mut self, shift: f32, limit: u32) -> (Vec<u8>, i32, i32, u32, u32) {
        self.close();
        if self.lines.is_empty() {
            return (Vec::new(), 0, 0, 0, 0);
        }
        let (left, top) = (floor(self.min.0 + shift), floor(self.min.1));
        let w = (ceil(self.max.0 + shift) - left).clamp(0, limit as i32) as usize;
        let h = (ceil(self.max.1) - top).clamp(0, limit as i32) as usize;
        if w == 0 || h == 0 {
            return (Vec::new(), left, top, 0, 0);
        }
        // Two more columns: what an edge on the last pixel's right side leaves.
        let row = w + 2;
        let mut area = vec![0f32; row * h];
        for line in &self.lines {
            let from = (line[0] + shift - left as f32, line[1] - top as f32);
            let to = (line[2] + shift - left as f32, line[3] - top as f32);
            add_line(&mut area, row, h, from, to);
        }
        let mut out = Vec::with_capacity(w * h);
        for cells in area.chunks_exact(row) {
            let mut sum = 0.0;
            for cell in &cells[..w] {
                sum += *cell;
                out.push((abs(sum).min(1.0) * 255.0 + 0.5) as u8);
            }
        }
        (out, left, top, w as u32, h as u32)
    }
}

/// Adds one edge's signed area to the cells of each row it crosses.
fn add_line(area: &mut [f32], row: usize, height: usize, from: (f32, f32), to: (f32, f32)) {
    if from.1 == to.1 {
        return;
    }
    let (dir, p0, p1) = if from.1 < to.1 {
        (1.0, from, to)
    } else {
        (-1.0, to, from)
    };
    let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
    let first = floor(p0.1).max(0);
    let last = ceil(p1.1).min(height as i32);
    let width = (row - 2) as f32;
    // Where the edge is when it enters the visible rows.
    let mut x = p0.0;
    if p0.1 < 0.0 {
        x -= p0.1 * dxdy;
    }
    for y in first..last {
        let start = y as usize * row;
        let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
        let next = x + dxdy * dy;
        let d = dy * dir;
        let (x0, x1) = if x < next { (x, next) } else { (next, x) };
        let (x0, x1) = (x0.clamp(0.0, width), x1.clamp(0.0, width));
        let (c0, f0) = (floor(x0), x0 - floor(x0) as f32);
        let c1 = ceil(x1);
        if c1 <= c0 + 1 {
            // Within one column: what lies to its right.
            let middle = 0.5 * (f0 + (x1 - c0 as f32));
            area[start + c0 as usize] += d - d * middle;
            area[start + c0 as usize + 1] += d * middle;
        } else {
            let s = 1.0 / (x1 - x0);
            let f1 = x1 - (c1 - 1) as f32;
            let a0 = 0.5 * s * (1.0 - f0) * (1.0 - f0);
            let am = 0.5 * s * f1 * f1;
            area[start + c0 as usize] += d * a0;
            if c1 == c0 + 2 {
                area[start + c0 as usize + 1] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - f0);
                area[start + c0 as usize + 1] += d * (a1 - a0);
                for c in c0 + 2..c1 - 1 {
                    area[start + c as usize] += d * s;
                }
                let a2 = a1 + (c1 - c0 - 3) as f32 * s;
                area[start + c1 as usize - 1] += d * (1.0 - a2 - am);
            }
            area[start + c1 as usize] += d * am;
        }
        x = next;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn square(x: f32, y: f32, side: f32) -> Outline {
        let mut o = Outline::new();
        o.move_to(x, y);
        o.line_to(x + side, y);
        o.line_to(x + side, y + side);
        o.line_to(x, y + side);
        o.close();
        o
    }

    #[test]
    fn a_square_on_whole_pixels_is_all_or_nothing() {
        let (mask, left, top, w, h) = square(2.0, -5.0, 3.0).coverage(0.0, 256);
        assert_eq!((left, top, w, h), (2, -5, 3, 3));
        assert!(mask.iter().all(|&c| c == 255), "{mask:?}");
    }

    #[test]
    fn a_square_off_the_grid_covers_in_proportion() {
        let (mask, _, _, w, h) = square(0.5, 0.0, 2.0).coverage(0.0, 256);
        assert_eq!((w, h), (3, 2));
        assert_eq!(&mask[..3], &[128, 255, 128]);
        // A shift of a quarter pixel moves it just as much.
        let (mask, left, _, w, _) = square(0.0, 0.0, 1.0).coverage(0.25, 256);
        assert_eq!((left, w), (0, 2));
        assert_eq!(&mask[..2], &[191, 64]);
    }

    #[test]
    fn nothing_breaks_on_odd_outlines() {
        assert!(Outline::new().coverage(0.0, 256).0.is_empty());
        let mut o = Outline::new();
        o.move_to(1.0, 1.0);
        o.line_to(1.0, 1.0);
        assert!(o.coverage(0.0, 256).0.is_empty());
        // Past the limit, it is cut, not an allocation of the whole thing.
        let (mask, _, _, w, h) = square(0.0, 0.0, 1e7).coverage(0.0, 64);
        assert_eq!((w, h, mask.len()), (64, 64, 64 * 64));
        let mut o = Outline::new();
        o.move_to(-1e6, -1e6);
        o.curve_to((1e6, 0.0), (0.0, 1e6), 1e6, 1e6);
        o.quad_to(0.0, 1e5, -3.0, 5.0);
        o.coverage(0.25, 64);
    }
}
