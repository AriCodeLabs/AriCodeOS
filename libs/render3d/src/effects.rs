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

#[cfg(test)]
mod tests {
    use super::*;
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
