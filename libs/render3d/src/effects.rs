// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Image filters: what a 2D effect on a GPU does with a fragment program.

/// `dst` is `src` convolved with the square `kernel` of side `size` (odd), divided by
/// the kernel's sum when it is not zero, edges repeated. Both are `width` x `height`,
/// `stride` apart.
pub fn convolve(
    src: &[u32],
    dst: &mut [u32],
    (width, height, stride): (usize, usize, usize),
    kernel: &[f32],
    size: usize,
) {
    let need = stride * height;
    if size.is_multiple_of(2) || kernel.len() != size * size || src.len() < need || dst.len() < need
    {
        return;
    }
    let sum: f32 = kernel.iter().sum();
    let scale = if sum.abs() > 1e-6 { 1.0 / sum } else { 1.0 };
    let r = (size / 2) as isize;
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0.0f32; 3];
            for ky in 0..size {
                let sy = (y as isize + ky as isize - r).clamp(0, height as isize - 1) as usize;
                for kx in 0..size {
                    let sx = (x as isize + kx as isize - r).clamp(0, width as isize - 1) as usize;
                    let k = kernel[ky * size + kx];
                    let p = src[sy * stride + sx];
                    acc[0] += ((p >> 16) & 0xff) as f32 * k;
                    acc[1] += ((p >> 8) & 0xff) as f32 * k;
                    acc[2] += (p & 0xff) as f32 * k;
                }
            }
            let c = acc.map(|v| (v * scale).clamp(0.0, 255.0) as u32);
            dst[y * stride + x] = c[0] << 16 | c[1] << 8 | c[2];
        }
    }
}

/// 64 colours around a wheel of cosines, turned by `phase`: what colours a fractal.
pub fn palette(phase: f32) -> [u32; 64] {
    core::array::from_fn(|i| {
        let t = i as f32 / 64.0 + phase;
        let channel = |offset: f32| {
            let v = 0.5 + 0.5 * libm::cosf(core::f32::consts::TAU * (t + offset));
            (v * 255.0) as u32
        };
        channel(0.0) << 16 | channel(0.33) << 8 | channel(0.67)
    })
}

/// The Julia set of `c` drawn into `dst` (`width` x `height`, `stride` apart): a view
/// `scale` high around `centre`, each point coloured from `colours` by how many steps
/// of z*z + c it takes to leave the circle of radius 2, black if `max` steps do not.
/// What a fragment program with a loop and a branch does. The steps taken in all, the
/// work done; 0 if `dst` is too short.
pub fn julia(
    dst: &mut [u32],
    (width, height, stride): (usize, usize, usize),
    c: (f32, f32),
    (centre, scale): ((f32, f32), f32),
    max: u32,
    colours: &[u32; 64],
) -> u64 {
    if height == 0 || width > stride || dst.len() < stride * (height - 1) + width {
        return 0;
    }
    let step = scale / height as f32;
    let left = centre.0 - step * width as f32 / 2.0;
    let top = centre.1 + scale / 2.0;
    let mut steps = 0u64;
    for y in 0..height {
        let zy0 = top - y as f32 * step;
        for x in 0..width {
            let (mut zx, mut zy) = (left + x as f32 * step, zy0);
            let mut i = 0;
            while i < max {
                let (xx, yy) = (zx * zx, zy * zy);
                if xx + yy > 4.0 {
                    break;
                }
                zy = 2.0 * zx * zy + c.1;
                zx = xx - yy + c.0;
                i += 1;
            }
            steps += u64::from(i);
            dst[y * stride + x] = if i == max {
                0
            } else {
                colours[(i % 64) as usize]
            };
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn julia_of_zero_is_the_unit_disc() {
        // z*z never leaves inside |z| < 1 and leaves at once beyond 2.
        let colours = palette(0.0);
        let mut dst = alloc::vec![1u32; 9 * 9];
        let steps = julia(
            &mut dst,
            (9, 9, 9),
            (0.0, 0.0),
            ((0.0, 0.0), 6.0),
            32,
            &colours,
        );
        assert_eq!(dst[4 * 9 + 4], 0, "the centre stays");
        assert_ne!(dst[0], 0, "a corner, sqrt(18) out, leaves");
        assert_eq!(dst[0], colours[0], "and at its first step");
        assert!(steps >= 32);
        // Too short a buffer: nothing drawn.
        assert_eq!(
            julia(
                &mut dst[..10],
                (9, 9, 9),
                (0.0, 0.0),
                ((0.0, 0.0), 6.0),
                32,
                &colours
            ),
            0
        );
    }
    use alloc::vec;

    #[test]
    fn identity_keeps_the_image_and_a_box_blur_smooths_an_edge() {
        let (w, h) = (8, 4);
        let src: alloc::vec::Vec<u32> = (0..w * h)
            .map(|i| if i % w < 4 { 0 } else { 0xffffff })
            .collect();
        let mut dst = vec![0; w * h];
        let mut id = [0.0; 9];
        id[4] = 1.0;
        convolve(&src, &mut dst, (w, h, w), &id, 3);
        assert_eq!(dst, src);
        convolve(&src, &mut dst, (w, h, w), &[1.0; 9], 3);
        let edge = dst[w + 3] & 0xff;
        assert!(edge > 0 && edge < 255, "softened: {edge}");
        // A kernel of the wrong shape changes nothing.
        let before = dst.clone();
        convolve(&src, &mut dst, (w, h, w), &[1.0; 4], 2);
        assert_eq!(dst, before);
    }
}
