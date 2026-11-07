// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Original, deterministic material artwork. No downloaded images or decoder.
//! Integer, periodic noise keeps the source portable and textures repeatable.

use aricode_render3d::Texture;

pub const SIZES: [usize; 4] = [128, 128, 128, 64];

pub fn make() -> Option<[Texture; 4]> {
    Some([
        Texture::try_new(SIZES[0], |x, y| texel(0, x, y))?,
        Texture::try_new(SIZES[1], |x, y| texel(1, x, y))?,
        Texture::try_new(SIZES[2], |x, y| texel(2, x, y))?,
        Texture::try_new(SIZES[3], |x, y| texel(3, x, y))?,
    ])
}

fn hash(x: usize, y: usize, seed: u32) -> i32 {
    let mut v = (x as u32).wrapping_mul(0x9e3779b9) ^ (y as u32).wrapping_mul(0x85ebca6b) ^ seed;
    v ^= v >> 16;
    v = v.wrapping_mul(0x7feb352d);
    ((v ^ (v >> 15)) & 255) as i32
}

/// Bilinear value noise whose grid wraps exactly at the material boundary.
fn noise(x: usize, y: usize, step: usize, size: usize, seed: u32) -> i32 {
    let (gx, gy, n) = (x / step, y / step, size / step);
    let (fx, fy, d) = ((x % step) as i32, (y % step) as i32, step as i32);
    let at = |dx, dy| hash((gx + dx) % n, (gy + dy) % n, seed);
    let a = at(0, 0) * (d - fx) + at(1, 0) * fx;
    let b = at(0, 1) * (d - fx) + at(1, 1) * fx;
    (a * (d - fy) + b * fy) / (d * d) - 128
}

fn rgb(r: i32, g: i32, b: i32) -> u32 {
    ((r.clamp(0, 255) as u32) << 16) | ((g.clamp(0, 255) as u32) << 8) | b.clamp(0, 255) as u32
}

/// 0: braced wooden crate, 1: dressed stone, 2: moss/earth, 3: worn steel.
/// The public sampler also lets the format comparison encode identical source art.
pub fn texel(material: usize, x: usize, y: usize) -> u32 {
    let size = SIZES[material];
    let (x, y) = (x % size, y % size);
    let fine = hash(x, y, 17) / 16 - 8;
    match material {
        0 => {
            let border = !(9..119).contains(&x) || !(9..119).contains(&y);
            let brace = x.abs_diff(y) < 7 || x.abs_diff(127 - y) < 7;
            let (along, across) = if border || brace { (x + y, x) } else { (y, x) };
            let grain =
                noise(along % 128, across, 4, 128, 91) / 6 + hash(across, along / 32, 81) / 12 - 10;
            let shade = if border || brace { 24 } else { 0 };
            let seam = if !border && !brace && x % 24 < 2 {
                -65
            } else {
                0
            };
            // Four inset nails, with a highlight and a dark rim.
            let dx = x.min(127 - x) as i32 - 5;
            let dy = y.min(127 - y) as i32 - 5;
            if dx * dx + dy * dy < 10 {
                let light = if dx + dy < 0 { 155 } else { 50 };
                return rgb(light, light, light - 8);
            }
            let v = grain + fine + shade + seam;
            rgb(137 + v, 88 + v * 3 / 4, 43 + v / 2)
        }
        1 => {
            let row = y / 32;
            let bx = (x + (row % 2) * 32) % 64;
            let by = y % 32;
            let edge = bx.min(63 - bx).min(by.min(31 - by));
            if edge < 2 {
                return rgb(44 + fine, 43 + fine, 39 + fine);
            }
            let block = hash((x + (row % 2) * 32) / 64, row, 35) / 12 - 10;
            let relief = if edge < 4 {
                if bx < 4 || by < 4 {
                    22
                } else {
                    -24
                }
            } else {
                0
            };
            let v = noise(x, y, 8, 128, 61) / 4 + fine + block + relief;
            rgb(133 + v, 129 + v, 115 + v)
        }
        2 => {
            let patch = noise(x, y, 32, 128, 123);
            let detail = noise(x, y, 4, 128, 57) / 5 + fine;
            if hash(x / 2, y / 2, 923) > 250 {
                return rgb(128 + detail, 119 + detail, 91 + detail);
            }
            let moss = patch.max(0) / 2;
            rgb(80 + detail - moss / 2, 71 + detail + moss, 40 + detail)
        }
        _ => {
            let edge = x.min(63 - x).min(y.min(63 - y));
            let dx = x.min(63 - x) as i32 - 6;
            let dy = y.min(63 - y) as i32 - 6;
            if dx * dx + dy * dy < 8 {
                let v = if dx + dy < 0 { 205 } else { 48 };
                return rgb(v, v, v);
            }
            let rust = (noise(x, y, 8, 64, 243) - 10).max(0);
            let brushed = hash(0, y, 3) / 24 - 5;
            let seam = if edge == 0 {
                -65
            } else if edge == 1 {
                30
            } else {
                0
            };
            let v = fine / 2 + brushed + seam;
            rgb(105 + v + rust / 2, 126 + v - rust, 132 + v - rust * 3 / 2)
        }
    }
}
