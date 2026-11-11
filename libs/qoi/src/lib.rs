// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A QOI encoder ("Quite OK Image" format, specification 1.0 of
//! qoiformat.org), for the shell's `screenshot`: a desktop is mostly runs
//! and repeated colours, which QOI packs in one pass with no tables to
//! build, and common tools read it (ffmpeg, ImageMagick, GIMP). PNG would
//! need a deflate encoder for a smaller file and no gain in what it shows.
//!
//! It streams: pixels go in one at a time and the bytes come out through a
//! sink in pieces of at most [`CHUNK`], so a screen of any size needs no
//! buffer of its size. Pixels are `0x00RRGGBB`, as the compositor keeps
//! them; the image says 3 channels, sRGB.

#![no_std]

#[cfg(any(test, feature = "decode"))]
extern crate alloc;

/// The bytes before the pixels.
pub const HEADER_LEN: usize = 14;
/// What ends the stream.
pub const END: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];
/// The most bytes the sink gets at once.
pub const CHUNK: usize = 1024;

const OP_INDEX: u8 = 0x00;
const OP_DIFF: u8 = 0x40;
const OP_LUMA: u8 = 0x80;
const OP_RUN: u8 = 0xC0;
const OP_RGB: u8 = 0xFE;
/// A run holds 1 to 62 pixels (63 and 64 would read as `OP_RGB`/`OP_RGBA`).
const MAX_RUN: u8 = 62;
/// An opaque pixel in the index; an empty entry is 0 and never matches one.
const OPAQUE: u32 = 0xFF00_0000;

/// The header of a `width` x `height` image: "qoif", the size big-endian,
/// 3 channels, sRGB with linear alpha.
pub fn header(width: u32, height: u32) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(b"qoif");
    h[4..8].copy_from_slice(&width.to_be_bytes());
    h[8..12].copy_from_slice(&height.to_be_bytes());
    h[12] = 3;
    h[13] = 0;
    h
}

/// The most bytes a `width` x `height` image can take: every pixel an
/// `OP_RGB`, header and end included.
pub fn max_len(width: u32, height: u32) -> u64 {
    HEADER_LEN as u64 + width as u64 * height as u64 * 4 + END.len() as u64
}

fn hash(px: u32) -> usize {
    let r = (px >> 16) & 0xFF;
    let g = (px >> 8) & 0xFF;
    let b = px & 0xFF;
    ((r * 3 + g * 5 + b * 7 + 255 * 11) % 64) as usize
}

/// Encodes pixels one at a time into a sink.
pub struct Encoder<F: FnMut(&[u8])> {
    sink: F,
    out: [u8; CHUNK],
    len: usize,
    /// The last pixel, opaque; it starts black.
    prev: u32,
    index: [u32; 64],
    run: u8,
}

impl<F: FnMut(&[u8])> Encoder<F> {
    /// Starts an image of `width` x `height`: the header goes to `sink`
    /// first. Exactly `width * height` pixels must follow, then
    /// [`finish`](Self::finish).
    pub fn new(width: u32, height: u32, sink: F) -> Self {
        let mut e = Self {
            sink,
            out: [0; CHUNK],
            len: 0,
            prev: OPAQUE,
            index: [0; 64],
            run: 0,
        };
        e.put(&header(width, height));
        e
    }

    fn put(&mut self, bytes: &[u8]) {
        if self.len + bytes.len() > CHUNK {
            self.flush();
        }
        self.out[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    fn flush(&mut self) {
        if self.len > 0 {
            (self.sink)(&self.out[..self.len]);
            self.len = 0;
        }
    }

    fn end_run(&mut self) {
        if self.run > 0 {
            self.put(&[OP_RUN | (self.run - 1)]);
            self.run = 0;
        }
    }

    /// The next pixel, `0x00RRGGBB` (the top byte is ignored).
    pub fn push(&mut self, px: u32) {
        let px = (px & 0xFF_FFFF) | OPAQUE;
        if px == self.prev {
            self.run += 1;
            if self.run == MAX_RUN {
                self.end_run();
            }
            return;
        }
        self.end_run();
        let at = hash(px);
        if self.index[at] == px {
            self.put(&[OP_INDEX | at as u8]);
        } else {
            self.index[at] = px;
            let channel = |v: u32, shift: u32| (v >> shift) as u8;
            let dr = channel(px, 16).wrapping_sub(channel(self.prev, 16)) as i8;
            let dg = channel(px, 8).wrapping_sub(channel(self.prev, 8)) as i8;
            let db = channel(px, 0).wrapping_sub(channel(self.prev, 0)) as i8;
            let dr_dg = dr.wrapping_sub(dg);
            let db_dg = db.wrapping_sub(dg);
            if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                let bits = ((dr + 2) as u8) << 4 | ((dg + 2) as u8) << 2 | (db + 2) as u8;
                self.put(&[OP_DIFF | bits]);
            } else if (-32..=31).contains(&dg)
                && (-8..=7).contains(&dr_dg)
                && (-8..=7).contains(&db_dg)
            {
                self.put(&[
                    OP_LUMA | (dg + 32) as u8,
                    ((dr_dg + 8) as u8) << 4 | (db_dg + 8) as u8,
                ]);
            } else {
                self.put(&[OP_RGB, channel(px, 16), channel(px, 8), channel(px, 0)]);
            }
        }
        self.prev = px;
    }

    /// Ends the image: the last run, the end marker, and everything still
    /// held goes to the sink.
    pub fn finish(mut self) {
        self.end_run();
        self.put(&END);
        self.flush();
    }
}

/// A decoded image: `0x00RRGGBB` pixels, row after row.
#[cfg(any(test, feature = "decode"))]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: alloc::vec::Vec<u32>,
}

/// Decodes a 3- or 4-channel image, alpha dropped, written from the specification:
/// what the test runner checks a `screenshot` with. `None` for anything malformed:
/// a bad header or end, an op cut short, or not exactly width x height pixels.
#[cfg(any(test, feature = "decode"))]
pub fn decode(data: &[u8]) -> Option<Image> {
    if data.len() < HEADER_LEN + END.len() || &data[..4] != b"qoif" {
        return None;
    }
    let width = u32::from_be_bytes(data[4..8].try_into().ok()?);
    let height = u32::from_be_bytes(data[8..12].try_into().ok()?);
    if !matches!(data[12], 3 | 4) || data[data.len() - END.len()..] != END {
        return None;
    }
    let count = (width as usize).checked_mul(height as usize)?;
    let body = &data[HEADER_LEN..data.len() - END.len()];
    // Every op gives at least one pixel: no more pixels than bytes are possible
    // but for runs, so the vector is bounded by what the data can say.
    let mut pixels = alloc::vec::Vec::with_capacity(count.min(body.len() * 62));
    let mut index = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut i = 0;
    while i < body.len() {
        let b = body[i];
        i += 1;
        let mut run = 1;
        if b == OP_RGB {
            px[..3].copy_from_slice(body.get(i..i + 3)?);
            i += 3;
        } else if b == 0xFF {
            px.copy_from_slice(body.get(i..i + 4)?);
            i += 4;
        } else {
            match b & 0xC0 {
                OP_INDEX => px = index[(b & 0x3F) as usize],
                OP_DIFF => {
                    px[0] = px[0].wrapping_add((b >> 4 & 3).wrapping_sub(2));
                    px[1] = px[1].wrapping_add((b >> 2 & 3).wrapping_sub(2));
                    px[2] = px[2].wrapping_add((b & 3).wrapping_sub(2));
                }
                OP_LUMA => {
                    let dg = (b & 0x3F).wrapping_sub(32);
                    let next = *body.get(i)?;
                    i += 1;
                    px[0] = px[0].wrapping_add(dg.wrapping_add((next >> 4).wrapping_sub(8)));
                    px[1] = px[1].wrapping_add(dg);
                    px[2] = px[2].wrapping_add(dg.wrapping_add((next & 0xF).wrapping_sub(8)));
                }
                _ => run = (b & 0x3F) as usize + 1,
            }
        }
        let at =
            (px[0] as usize * 3 + px[1] as usize * 5 + px[2] as usize * 7 + px[3] as usize * 11)
                % 64;
        index[at] = px;
        if pixels.len() + run > count {
            return None;
        }
        let value = u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]);
        pixels.extend(core::iter::repeat_n(value, run));
    }
    (pixels.len() == count).then_some(Image {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn decode(data: &[u8]) -> (u32, u32, Vec<u32>) {
        let image = super::decode(data).expect("a valid image");
        (image.width, image.height, image.pixels)
    }

    fn encode(w: u32, h: u32, pixels: &[u32]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut e = Encoder::new(w, h, |b: &[u8]| {
            assert!(b.len() <= CHUNK);
            data.extend_from_slice(b)
        });
        for &p in pixels {
            e.push(p);
        }
        e.finish();
        data
    }

    #[test]
    fn two_equal_pixels_are_a_colour_and_a_run() {
        let data = encode(2, 1, &[0x804020, 0x804020]);
        let mut want = Vec::from(header(2, 1));
        want.extend_from_slice(&[OP_RGB, 0x80, 0x40, 0x20, OP_RUN]);
        want.extend_from_slice(&END);
        assert_eq!(data, want);
    }

    #[test]
    fn a_black_first_pixel_is_a_run_from_the_start() {
        // The encoder starts from opaque black, as the decoder does.
        let data = encode(1, 1, &[0]);
        assert_eq!(data[HEADER_LEN], OP_RUN);
    }

    #[test]
    fn runs_split_at_62() {
        let pixels = std::vec![0x123456; 200];
        let data = encode(200, 1, &pixels);
        assert_eq!(decode(&data).2, pixels);
        // One colour, then runs of 62, 62, 62 and 13.
        assert_eq!(data.len(), HEADER_LEN + 4 + 4 + 8);
    }

    #[test]
    fn every_op_round_trips() {
        // A gradient (diffs and lumas), noise (full colours), repeats
        // (index hits) and flat areas (runs), with the top byte set.
        let (w, h) = (97, 61);
        let mut seed = 0x1234_5678u32;
        let pixels: Vec<u32> = (0..w * h)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let (x, y) = (i % w, i / w);
                match y % 4 {
                    0 => (x * 2) << 16 | y << 8 | x,
                    1 => seed >> 8,
                    2 => [0xFF0000, 0x00FF00, 0x0000FF][(x % 3) as usize],
                    _ => 0xFF20_4060,
                }
            })
            .collect();
        let data = encode(w, h, &pixels);
        let (dw, dh, decoded) = decode(&data);
        assert_eq!((dw, dh), (w, h));
        let want: Vec<u32> = pixels.iter().map(|p| p & 0xFF_FFFF).collect();
        assert_eq!(decoded, want);
        assert!(data.len() as u64 <= max_len(w, h));
    }

    #[test]
    fn the_decoder_refuses_what_is_malformed() {
        let good = encode(3, 2, &[1, 2, 3, 4, 5, 6]);
        assert!(super::decode(&good).is_some());
        // Cut short, a pixel short, a pixel more, a wrong end, a wrong magic.
        assert!(super::decode(&good[..good.len() - 1]).is_none());
        assert!(super::decode(&encode(3, 2, &[1, 2, 3, 4, 5])).is_none());
        assert!(super::decode(&encode(3, 2, &[1, 2, 3, 4, 5, 6, 7])).is_none());
        let mut bad_end = good.clone();
        *bad_end.last_mut().unwrap() = 2;
        assert!(super::decode(&bad_end).is_none());
        let mut bad_magic = good.clone();
        bad_magic[0] = b'x';
        assert!(super::decode(&bad_magic).is_none());
        // An RGB op whose bytes are missing.
        let mut cut = Vec::from(header(1, 1));
        cut.extend_from_slice(&[OP_RGB, 1]);
        cut.extend_from_slice(&END);
        assert!(super::decode(&cut).is_none());
    }

    #[test]
    fn large_differences_wrap() {
        // 0 to 255 in a channel is -1 in eight bits: a diff, not a colour.
        let pixels = [0x000000, 0xFF0000, 0x00FF00, 0x80_0000];
        let data = encode(4, 1, &pixels);
        assert_eq!(decode(&data).2, pixels);
    }
}
