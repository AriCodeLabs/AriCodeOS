// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Textures with their mipmaps, sampled nearest, bilinear, or bilinear on the level the
//! triangle's size on screen asks for.

use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Linear,
    Mipmap,
}

/// One level: `size` x `size` texels, `0x00RRGGBB`.
#[derive(Debug, Clone)]
struct Level {
    size: usize,
    texels: Vec<u32>,
}

/// A square texture whose side is a power of two, with every mipmap level down to 1x1.
#[derive(Debug, Clone)]
pub struct Texture {
    levels: Vec<Level>,
}

impl Texture {
    /// `texel(x, y)` gives the top level; the rest are averages of four.
    pub fn new(size: usize, texel: impl Fn(usize, usize) -> u32) -> Self {
        assert!(size.is_power_of_two(), "a texture's side is a power of two");
        let mut top = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                top.push(texel(x, y));
            }
        }
        let mut levels = alloc::vec![Level { size, texels: top }];
        while levels.last().is_some_and(|l| l.size > 1) {
            let up = levels.last().unwrap();
            let size = up.size / 2;
            let mut texels = Vec::with_capacity(size * size);
            for y in 0..size {
                for x in 0..size {
                    let at = |dx: usize, dy: usize| up.texels[(y * 2 + dy) * up.size + x * 2 + dx];
                    texels.push(average4([at(0, 0), at(1, 0), at(0, 1), at(1, 1)]));
                }
            }
            levels.push(Level { size, texels });
        }
        Self { levels }
    }

    /// A checker of `cells` squares a side, in two colours, with a thin grid: what shows
    /// filtering, and its faults, most plainly.
    pub fn checker(size: usize, cells: usize, a: u32, b: u32) -> Self {
        let cell = (size / cells).max(1);
        Self::new(size, |x, y| {
            let edge = x % cell == 0 || y % cell == 0;
            match ((x / cell + y / cell).is_multiple_of(2), edge) {
                (_, true) => 0x202020,
                (true, _) => a,
                (false, _) => b,
            }
        })
    }

    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    pub fn size(&self) -> usize {
        self.levels[0].size
    }

    /// The colour at `(u, v)`, repeating, on `level` (0 the largest), as `[r, g, b]` in
    /// 0 to 1.
    pub fn sample(&self, u: f32, v: f32, filter: Filter, level: usize) -> [f32; 3] {
        let level = &self.levels[level.min(self.levels.len() - 1)];
        let n = level.size;
        let wrap = |i: i64| i.rem_euclid(n as i64) as usize;
        let (x, y) = (u * n as f32 - 0.5, v * n as f32 - 0.5);
        if filter == Filter::Nearest {
            let (x, y) = (libm::floorf(x + 0.5) as i64, libm::floorf(y + 0.5) as i64);
            return unpack(level.texels[wrap(y) * n + wrap(x)]);
        }
        let (fx, fy) = (libm::floorf(x), libm::floorf(y));
        let (tx, ty) = (x - fx, y - fy);
        let (x0, y0) = (fx as i64, fy as i64);
        let at = |dx: i64, dy: i64| unpack(level.texels[wrap(y0 + dy) * n + wrap(x0 + dx)]);
        let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
        core::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bottom = c[i] + (d[i] - c[i]) * tx;
            top + (bottom - top) * ty
        })
    }
}

fn unpack(c: u32) -> [f32; 3] {
    [16, 8, 0].map(|s| ((c >> s) & 0xff) as f32 / 255.0)
}

fn average4(c: [u32; 4]) -> u32 {
    [16, 8, 0]
        .map(|s| (c.iter().map(|&p| (p >> s) & 0xff).sum::<u32>() / 4) << s)
        .iter()
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_go_down_to_one_texel_and_average() {
        let t = Texture::new(4, |x, y| if (x + y) % 2 == 0 { 0xffffff } else { 0 });
        assert_eq!(t.levels(), 3);
        // Black and white average to grey on the 1x1 level.
        let grey = t.sample(0.5, 0.5, Filter::Nearest, 2);
        assert!((grey[0] - 127.0 / 255.0).abs() < 1e-3);
    }

    #[test]
    fn nearest_gives_texels_and_linear_blends_between_them() {
        let t = Texture::new(2, |x, _| if x == 0 { 0x000000 } else { 0xff0000 });
        assert_eq!(t.sample(0.25, 0.25, Filter::Nearest, 0), [0.0, 0.0, 0.0]);
        assert_eq!(t.sample(0.75, 0.25, Filter::Nearest, 0), [1.0, 0.0, 0.0]);
        // Halfway between the two texel centres.
        let mid = t.sample(0.5, 0.25, Filter::Linear, 0);
        assert!((mid[0] - 0.5).abs() < 1e-5);
        // It repeats: past the right edge comes the left.
        assert_eq!(t.sample(1.25, 0.25, Filter::Nearest, 0), [0.0, 0.0, 0.0]);
    }
}
