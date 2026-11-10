// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Filled triangles on a `0x00RRGGBB` buffer. Painter's order: whatever comes
//! later covers what came before for overlays. The 3D path interpolates reciprocal
//! depth, so hills, scenery and vehicles occlude correctly in any draw order.

/// A pixel buffer, row after row.
pub struct Target<'a> {
    pub depth: &'a mut [u32],
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

impl Target<'_> {
    /// Fills whole rows `from..to` (clipped) with `color`.
    pub fn fill_rows(&mut self, from: usize, to: usize, color: u32) {
        let to = to.min(self.height);
        if from < to {
            self.pixels[from * self.width..to * self.width].fill(color);
        }
    }

    /// Fills `[x0, x1)` of row `y`, clipped.
    #[inline]
    pub fn span(&mut self, y: i64, x0: i64, x1: i64, color: u32) {
        if y < 0 || y >= self.height as i64 {
            return;
        }
        let (x0, x1) = (x0.max(0), x1.min(self.width as i64));
        if x0 < x1 {
            let row = y as usize * self.width;
            self.pixels[row + x0 as usize..row + x1 as usize].fill(color);
        }
    }

    /// Perspective-correct reciprocal depth, interpolated in screen space.
    /// `z` is 2^24 / metres; larger values are nearer. Clearing to zero is sky.
    pub fn triangle_depth(&mut self, v: [(i64, i64, i64); 3], color: u32) {
        self.surface(v.map(|(x, y, z)| [x, y, z, 0, 0]), color, 0);
    }

    /// Perspective texture coordinates are world X/Z multiplied by reciprocal
    /// depth. Only terrain pays for texture sampling; solid objects use fills.
    pub fn surface(&mut self, mut v: [[i64; 5]; 3], color: u32, material: u8) {
        v.sort_unstable_by_key(|p| p[1]);
        let [top, mid, low] = v;
        let first = ((top[1] + 32767) >> 16).max(0);
        let last = ((low[1] + 32767) >> 16).min(self.height as i64);
        let along = |a: [i64; 5], b: [i64; 5], y: i64| {
            let dy = b[1] - a[1];
            let t = y - a[1];
            let mut out = [a[0], a[2], a[3], a[4]];
            if dy != 0 {
                for (j, index) in [0, 2, 3, 4].into_iter().enumerate() {
                    out[j] += ((b[index] - a[index]) as i128 * t as i128 / dy as i128) as i64;
                }
            }
            out
        };
        for row in first..last {
            let y = (row << 16) + 32768;
            let a = along(top, low, y);
            let b = if y < mid[1] {
                along(top, mid, y)
            } else {
                along(mid, low, y)
            };
            let (l, r) = if a[0] < b[0] { (a, b) } else { (b, a) };
            let x0 = ((l[0] + 32767) >> 16).max(0);
            let x1 = ((r[0] + 32767) >> 16).min(self.width as i64);
            if x0 >= x1 || r[0] == l[0] {
                continue;
            }
            let mut values = [0i64; 3];
            let mut slopes = [0i64; 3];
            let attrs = if material == 0 { 1 } else { 3 };
            for j in 0..attrs {
                slopes[j] =
                    ((((r[j + 1] - l[j + 1]) as i128) << 32) / (r[0] - l[0]) as i128) as i64;
                values[j] = (l[j + 1] << 16)
                    + ((slopes[j] as i128 * ((x0 << 16) + 32768 - l[0]) as i128) >> 16) as i64;
            }
            let start = row as usize * self.width + x0 as usize;
            let end = row as usize * self.width + x1 as usize;
            for index in start..end {
                let depth = (values[0] >> 16).max(1) as u32;
                if depth >= self.depth[index] {
                    self.depth[index] = depth;
                    let mut paint = color;
                    // Fade fine texture into the distance to avoid shimmer.
                    if material != 0 && depth > 150_000 {
                        let u = ((values[1] >> 12) / depth as i64) as i32;
                        let v = ((values[2] >> 12) / depth as i64) as i32;
                        let hash = (u.wrapping_mul(374761393) ^ v.wrapping_mul(668265263)) as u32;
                        let grain = ((hash ^ (hash >> 13)) & 15) as i32;
                        let variation = if material == 1 {
                            grain - 8
                        } else {
                            let patch = (((u >> 5) ^ (v >> 5)) & 1) * 10;
                            grain - 14 + patch
                        };
                        let strength = ((depth - 150_000) / 25_000).min(16) as i32;
                        let factor = (256 + variation * strength / 16) as u32;
                        let red = (((color >> 16) & 255) * factor / 256).min(255);
                        let green = (((color >> 8) & 255) * factor / 256).min(255);
                        let blue = ((color & 255) * factor / 256).min(255);
                        paint = (red << 16) | (green << 8) | blue;
                    }
                    self.pixels[index] = paint;
                }
                for j in 0..attrs {
                    values[j] += slopes[j];
                }
            }
        }
    }

    /// A triangle with vertices in pixels, Q16.16. A pixel belongs to it when
    /// its centre does, so triangles that share an edge leave no gap and no
    /// overlap on it. The arithmetic is in 64 bits: projected points far off
    /// screen stay exact.
    pub fn triangle(&mut self, a: (i64, i64), b: (i64, i64), c: (i64, i64), color: u32) {
        let mut v = [a, b, c];
        v.sort_unstable_by_key(|p| p.1);
        let [top, mid, low] = v;
        const HALF: i64 = 1 << 15;
        const SHIFT: u32 = 16;
        // The first row whose centre is at or below the top, within the target.
        let first = ((top.1 - HALF + 0xFFFF) >> SHIFT).max(0);
        let last = ((low.1 - HALF + 0xFFFF) >> SHIFT).min(self.height as i64);
        let along = |from: (i64, i64), to: (i64, i64), y: i64| -> i64 {
            let run = to.1 - from.1;
            if run == 0 {
                return from.0;
            }
            from.0 + ((to.0 - from.0) as i128 * (y - from.1) as i128 / run as i128) as i64
        };
        for row in first..last {
            let y = (row << SHIFT) + HALF;
            let long = along(top, low, y);
            let short = if y < mid.1 {
                along(top, mid, y)
            } else {
                along(mid, low, y)
            };
            let (l, r) = if long < short {
                (long, short)
            } else {
                (short, long)
            };
            let x0 = (l - HALF + 0xFFFF) >> SHIFT;
            let x1 = (r - HALF + 0xFFFF) >> SHIFT;
            self.span(row, x0, x1, color);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    const W: usize = 16;
    const H: usize = 16;

    fn p(x: f64, y: f64) -> (i64, i64) {
        ((x * 65536.0) as i64, (y * 65536.0) as i64)
    }

    fn draw(tris: &[[(i64, i64); 3]]) -> Vec<u32> {
        let mut pixels = vec![0; W * H];
        let mut depths = vec![0; W * H];
        let mut t = Target {
            depth: &mut depths,
            pixels: &mut pixels,
            width: W,
            height: H,
        };
        for tri in tris {
            t.triangle(tri[0], tri[1], tri[2], 1);
        }
        pixels
    }

    #[test]
    fn nearer_geometry_wins_in_either_draw_order() {
        for reverse in [false, true] {
            let mut pixels = vec![0; W * H];
            let mut depth = vec![0; W * H];
            let mut t = Target {
                pixels: &mut pixels,
                depth: &mut depth,
                width: W,
                height: H,
            };
            let near = [(0, 0, 200), (16 << 16, 0, 200), (0, 16 << 16, 200)];
            let far = [(0, 0, 100), (16 << 16, 0, 100), (0, 16 << 16, 100)];
            if reverse {
                t.triangle_depth(near, 2);
                t.triangle_depth(far, 1);
            } else {
                t.triangle_depth(far, 1);
                t.triangle_depth(near, 2);
            }
            assert_eq!(pixels[W + 1], 2);
            assert_eq!(depth[W + 1], 200);
        }
    }

    #[test]
    fn intersecting_surfaces_interpolate_depth_per_pixel() {
        let mut pixels = vec![0; W * H];
        let mut depth = vec![0; W * H];
        let mut t = Target {
            pixels: &mut pixels,
            depth: &mut depth,
            width: W,
            height: H,
        };
        t.triangle_depth([(0, 0, 150), (16 << 16, 0, 150), (0, 16 << 16, 150)], 1);
        t.triangle_depth([(0, 0, 100), (16 << 16, 0, 300), (0, 16 << 16, 100)], 2);
        assert_eq!(pixels[W + 1], 1);
        assert_eq!(pixels[W + 10], 2);
    }

    #[test]
    fn a_triangle_fills_what_it_covers() {
        let px = draw(&[[p(0.0, 0.0), p(8.0, 0.0), p(0.0, 8.0)]]);
        assert_eq!(px[0], 1, "the corner's centre is inside");
        assert_eq!(px[8], 0, "past its width");
        assert_eq!(px[8 * W], 0, "past its height");
        // Pixel (i, j) has its centre inside when i + j + 1 < 8.
        assert_eq!(px.iter().sum::<u32>(), 28);
    }

    #[test]
    fn two_triangles_sharing_an_edge_leave_no_gap_and_no_overlap() {
        let (a, b, c, d) = (p(1.3, 1.1), p(13.7, 2.2), p(12.1, 14.3), p(2.4, 13.9));
        let mut pixels = vec![0u32; W * H];
        let mut depths = vec![0; W * H];
        let t = Target {
            depth: &mut depths,
            pixels: &mut pixels,
            width: W,
            height: H,
        };
        for tri in [[a, b, c], [a, c, d]] {
            // Counting, to see overlap: spans add through a scratch buffer.
            let mut one = vec![0u32; W * H];
            let mut scratch_depth = vec![0; W * H];
            let mut s = Target {
                depth: &mut scratch_depth,
                pixels: &mut one,
                width: W,
                height: H,
            };
            s.triangle(tri[0], tri[1], tri[2], 1);
            for (sum, px) in t.pixels.iter_mut().zip(one) {
                *sum += px;
            }
        }
        assert!(pixels.iter().all(|&n| n <= 1), "no pixel twice");
        let whole = draw(&[[a, b, c], [a, c, d]]);
        assert_eq!(pixels, whole, "and every pixel of the whole");
    }

    #[test]
    fn nothing_is_written_outside_however_far_a_vertex_is() {
        let far = 1i64 << 50;
        for tri in [
            [(-far, -far), (far, -far), (0, far)],
            [p(-100.0, 8.0), p(100.0, 7.0), p(5.0, 9000.0)],
            [p(20.0, 20.0), p(30.0, 20.0), p(25.0, 30.0)],
            [p(5.0, 5.0), p(5.0, 5.0), p(5.0, 5.0)],
        ] {
            draw(&[tri]);
        }
        assert!(draw(&[[(-far, -far), (far, -far), (0, far)]]).contains(&1));
    }
}
