// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The check every client batch passes before it runs (ADR 0083). On Ivy
//! Bridge a non-secure batch still reaches the blitter's register page,
//! which holds the PPGTT's own registers: the laptop showed it. So the
//! kernel copies each batch into a page of its own, checks the copy against
//! this short list, and runs the copy. What is not on the list does not run.
//!
//! The list: `MI_NOOP`; `MI_BATCH_BUFFER_END`; `MI_FLUSH_DW` with no store;
//! linear 32-bit `XY_COLOR_BLT` and `XY_SRC_COPY_BLT` whose every byte lies
//! in one of the client's mappings; and `MI_BATCH_BUFFER_START` to the copy's
//! own start, which can only loop until the engine is reset. The client does
//! not know where its copy runs: it writes address 0 for "my own start", and
//! the check writes the copy's address in its place.

use crate::cmd;

/// A range of the client's GPU address space, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

/// Why a batch does not run: the dword where the check stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A command not on the list, or with bits the list does not allow.
    Command(usize),
    /// A blit that would touch memory outside the client's mappings.
    Outside(usize),
    /// The batch ends before its last command does, or never ends.
    Unended(usize),
}

impl Fault {
    pub fn at(&self) -> usize {
        match *self {
            Fault::Command(i) | Fault::Outside(i) | Fault::Unended(i) => i,
        }
    }
}

const BLT_COLOR: u32 = 2 << 29 | 0x50 << 22 | 4;
const BLT_COPY: u32 = 2 << 29 | 0x53 << 22 | 6;
/// The write enables of a 32-bit blit (bits 21:20), the only bits besides
/// the opcode and length a blit's first dword may have: no tiling.
const WRITE_ENABLES: u32 = 3 << 20;
/// The second dword above the pitch: 32 bits a pixel (bits 25:24) and the
/// raster operation (23:16) the PRM gives each blit, the pattern for a fill
/// and the source for a copy (Vol 1 Part 4, 1.9.8 and 1.9.14); no clipping
/// (bit 30) and nothing else (audit L1).
const FILL_MODE: u32 = 3 << 24 | 0xF0 << 16;
const COPY_MODE: u32 = 3 << 24 | 0xCC << 16;

/// The bytes a blit touches on a surface at `base` with `pitch`, from
/// pixel (`x1`, `y1`), `w` by `h` pixels. `None` when a field is out of its
/// signed 16-bit range or the area is empty.
fn area(base: u32, pitch: u32, x1: u32, y1: u32, w: u32, h: u32) -> Option<Range> {
    let s16 = |v: u32| v <= i16::MAX as u32;
    if !(s16(pitch) && s16(x1) && s16(y1) && s16(x1 + w) && s16(y1 + h)) {
        return None;
    }
    if w == 0
        || h == 0
        || pitch == 0
        || !pitch.is_multiple_of(4)
        || w * 4 > pitch
        || !base.is_multiple_of(4)
    {
        return None;
    }
    let first = base as u64 + y1 as u64 * pitch as u64 + x1 as u64 * 4;
    let last = base as u64 + (y1 + h - 1) as u64 * pitch as u64 + (x1 + w) as u64 * 4;
    Some(Range {
        start: first,
        end: last,
    })
}

fn inside(r: &Range, maps: &[Range]) -> bool {
    maps.iter().any(|m| m.start <= r.start && r.end <= m.end)
}

/// Checks `batch`, the kernel's copy, which will run from `start` in the
/// client's GPU address space; `maps`: the client's mappings, none of which
/// may cover the copy. Returns the dwords up to and including the command
/// that ends it.
pub fn check(batch: &mut [u32], start: u32, maps: &[Range]) -> Result<usize, Fault> {
    let mut i = 0;
    while i < batch.len() {
        let w = batch[i];
        let need = |n: usize| {
            if i + n <= batch.len() {
                Ok(&batch[i..i + n])
            } else {
                Err(Fault::Unended(i))
            }
        };
        match w {
            cmd::NOOP => i += 1,
            cmd::BATCH_END => return Ok(i + 1),
            w if w == cmd::BATCH_START | cmd::BATCH_PPGTT => {
                if need(2)?[1] != 0 {
                    return Err(Fault::Command(i));
                }
                batch[i + 1] = start;
                return Ok(i + 2);
            }
            cmd::FLUSH_DW => {
                let c = need(3)?;
                if c[1] != 0 || c[2] != 0 {
                    return Err(Fault::Command(i));
                }
                i += 3;
            }
            w if w & !WRITE_ENABLES == BLT_COLOR => {
                let c = need(6)?;
                if c[1] & !0xFFFF != FILL_MODE {
                    return Err(Fault::Command(i));
                }
                let (x1, y1, x2, y2) = (c[2] & 0xFFFF, c[2] >> 16, c[3] & 0xFFFF, c[3] >> 16);
                let dst = (x2 > x1 && y2 > y1)
                    .then(|| area(c[4], c[1] & 0xFFFF, x1, y1, x2 - x1, y2 - y1))
                    .flatten()
                    .ok_or(Fault::Command(i))?;
                if !inside(&dst, maps) {
                    return Err(Fault::Outside(i));
                }
                i += 6;
            }
            w if w & !WRITE_ENABLES == BLT_COPY => {
                let c = need(8)?;
                if c[1] & !0xFFFF != COPY_MODE || c[6] & !0xFFFF != 0 {
                    return Err(Fault::Command(i));
                }
                let (x1, y1, x2, y2) = (c[2] & 0xFFFF, c[2] >> 16, c[3] & 0xFFFF, c[3] >> 16);
                if x2 <= x1 || y2 <= y1 {
                    return Err(Fault::Command(i));
                }
                let (w, h) = (x2 - x1, y2 - y1);
                let dst = area(c[4], c[1] & 0xFFFF, x1, y1, w, h).ok_or(Fault::Command(i))?;
                let src =
                    area(c[7], c[6], c[5] & 0xFFFF, c[5] >> 16, w, h).ok_or(Fault::Command(i))?;
                if !inside(&dst, maps) || !inside(&src, maps) {
                    return Err(Fault::Outside(i));
                }
                // Overlapping linear copies are defined only within one
                // surface whose pitch is whole 64 bytes (PRM Vol 1 Part 4,
                // 1.7.1).
                let overlap = dst.start < src.end && src.start < dst.end;
                let pitch = c[1] & 0xFFFF;
                if overlap && (c[4] != c[7] || pitch != c[6] || !pitch.is_multiple_of(64)) {
                    return Err(Fault::Command(i));
                }
                i += 8;
            }
            _ => return Err(Fault::Command(i)),
        }
    }
    Err(Fault::Unended(i))
}
