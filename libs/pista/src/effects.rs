// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Bloom and colour finishing. CPU reference for the optional Gen7 compute pass.
pub const PAD: usize = 4;
pub const TAPS: [(i32, i32); 5] = [(0, 0), (-4, 0), (4, 0), (0, -4), (0, 4)];

pub fn pixel(samples: [u32; 5]) -> u32 {
    let mut out = 0;
    for (channel, scale) in [(16, 244), (8, 240), (0, 236)] {
        let base = (samples[0] >> channel) & 255;
        let mut glow = 0;
        for (i, pixel) in samples.into_iter().enumerate() {
            let c = (pixel >> channel) & 255;
            let square = (c * c) >> 8;
            let energy = (square * square) >> 8;
            glow += energy * if i == 0 { 4 } else { 1 };
        }
        out |= (((base * scale) >> 8) + (glow >> 5)).min(255) << channel;
    }
    out
}

pub fn finish(src: &[u32], dst: &mut [u32], width: usize, height: usize) {
    assert!(src.len() >= width * height && dst.len() >= width * height);
    if width == 0 || height == 0 {
        return;
    }
    for y in 0..height {
        for x in 0..width {
            dst[y * width + x] = pixel(TAPS.map(|(dx, dy)| {
                let xx = (x as i32 + dx).clamp(0, width as i32 - 1) as usize;
                let yy = (y as i32 + dy).clamp(0, height as i32 - 1) as usize;
                src[yy * width + xx]
            }));
        }
    }
}

/// Replicated border: GPU media reads never address outside the allocation.
pub fn pad(src: &[u32], dst: &mut [u32], width: usize, height: usize) {
    let pitch = width + 2 * PAD;
    assert!(
        width > 0
            && height > 0
            && src.len() >= width * height
            && dst.len() >= pitch * (height + 2 * PAD)
    );
    for y in 0..height + 2 * PAD {
        let row = &src[y.saturating_sub(PAD).min(height - 1) * width..][..width];
        let out = &mut dst[y * pitch..][..pitch];
        out[..PAD].fill(row[0]);
        out[PAD..PAD + width].copy_from_slice(row);
        out[PAD + width..].fill(row[width - 1]);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    #[test]
    fn border_sampling_matches_the_gpu_layout_at_every_edge() {
        let (w, h) = (16, 9);
        let source: std::vec::Vec<u32> = (0..w * h).map(|i| (i as u32 * 7919) & 0xffffff).collect();
        let mut padded = vec![0; (w + 8) * (h + 8)];
        pad(&source, &mut padded, w, h);
        let mut cpu = vec![0; w * h];
        finish(&source, &mut cpu, w, h);
        for y in 0..h {
            for x in 0..w {
                let samples = TAPS.map(|(dx, dy)| {
                    padded[((y + PAD) as i32 + dy) as usize * (w + 8)
                        + ((x + PAD) as i32 + dx) as usize]
                });
                assert_eq!(cpu[y * w + x], pixel(samples));
            }
        }
    }
    #[test]
    fn highlights_bloom_without_overflow_or_colouring_black() {
        assert_eq!(pixel([0; 5]), 0);
        assert_eq!(pixel([0xffffff; 5]), 0xffffff);
        assert!(pixel([0, 0xffffff, 0, 0, 0]) > 0);
        let mut tiny = [0];
        finish(&[0xffffff], &mut tiny, 1, 1);
        assert_eq!(tiny[0], 0xffffff);
    }
}
