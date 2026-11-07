// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Commands of the blitter's command streamer (PRM Vol 1 Part 4: 2.2 for the
//! MI commands, 1.9.8 `XY_COLOR_BLT`, 1.9.14 `XY_SRC_COPY_BLT`), checked
//! against Linux's `intel_gpu_commands.h`. The kernel writes the MI commands
//! into its ring; a client writes the blits into its own batches, which run
//! non-secure in its PPGTT, so a wrong one can only spoil the client's own
//! memory. The checks here are for the client's sake.

/// The MI command with `opcode` (bits 28:23).
const fn mi(opcode: u32) -> u32 {
    opcode << 23
}

pub const NOOP: u32 = 0;
pub const USER_INTERRUPT: u32 = mi(0x02);
pub const BATCH_END: u32 = mi(0x0A);
/// Stores a dword at an offset in the status page; length 1.
pub const STORE_DWORD_INDEX: u32 = mi(0x21) | 1;
/// Flush, with an optional store after it; length 1.
pub const FLUSH_DW: u32 = mi(0x26) | 1;
pub const FLUSH_DW_STORE_INDEX: u32 = 1 << 21;
pub const FLUSH_DW_INVALIDATE_TLB: u32 = 1 << 18;
pub const FLUSH_DW_STORE_DWORD: u32 = 1 << 14;
/// In the address dword: the address is in the global GTT (only from the
/// ring or a secure batch).
pub const FLUSH_DW_GLOBAL_GTT: u32 = 1 << 2;
/// Starts a batch; the next dword is its address.
pub const BATCH_START: u32 = mi(0x31);
/// The batch is in the PPGTT and runs non-secure: on this family the one
/// bit says both (PRM Vol 1 Part 4, `MI_BATCH_BUFFER_START` bit 8).
pub const BATCH_PPGTT: u32 = 1 << 8;

/// A 2D blit command, `opcode` and the dwords after the first.
const fn blit(opcode: u32, len: u32) -> u32 {
    2 << 29 | opcode << 22 | len
}
/// Write all four bytes of each 32-bit pixel (bits 21:20).
const WRITE_RGBA: u32 = 3 << 20;
/// 32 bits a pixel (bits 25:24 of the second dword).
const DEPTH_32: u32 = 3 << 24;
const ROP_PATTERN: u32 = 0xF0 << 16;
const ROP_SOURCE: u32 = 0xCC << 16;

/// A linear surface of 32-bit pixels, at an address in the client's PPGTT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub address: u32,
    /// Bytes from one row to the next.
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
}

/// A rectangle, `x2` and `y2` excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x1: u32,
    pub y1: u32,
    pub x2: u32,
    pub y2: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlitError {
    /// Pitch not a whole number of pixels, or past the signed 16-bit field
    /// (PRM Vol 1 Part 4, 1.10.6).
    Pitch,
    /// Address not aligned to a pixel.
    Address,
    /// Empty, or not inside the surface, or past the signed 16-bit
    /// coordinates.
    Rect,
    /// Overlapping rows of one surface, whose pitch is not a multiple of 64
    /// (PRM Vol 1 Part 4, 1.7.1).
    Overlap,
    /// The surface ends past 4 GiB of the PPGTT.
    TooLarge,
}

const COORD_MAX: u32 = i16::MAX as u32;

impl Surface {
    fn check(&self) -> Result<(), BlitError> {
        if self.pitch == 0 || !self.pitch.is_multiple_of(4) || self.pitch > COORD_MAX {
            return Err(BlitError::Pitch);
        }
        if !self.address.is_multiple_of(4) {
            return Err(BlitError::Address);
        }
        if self.width > COORD_MAX || self.height > COORD_MAX || self.width * 4 > self.pitch {
            return Err(BlitError::Rect);
        }
        let bytes = self.pitch as u64 * self.height as u64;
        if self.address as u64 + bytes > 1 << 32 {
            return Err(BlitError::TooLarge);
        }
        Ok(())
    }

    fn holds(&self, r: &Rect) -> bool {
        r.x1 < r.x2 && r.y1 < r.y2 && r.x2 <= self.width && r.y2 <= self.height
    }
}

/// `XY_COLOR_BLT`: fills `rect` of `dst` with `colour`.
pub fn fill(dst: &Surface, rect: &Rect, colour: u32) -> Result<[u32; 6], BlitError> {
    dst.check()?;
    if !dst.holds(rect) {
        return Err(BlitError::Rect);
    }
    Ok([
        blit(0x50, 4) | WRITE_RGBA,
        DEPTH_32 | ROP_PATTERN | dst.pitch,
        rect.y1 << 16 | rect.x1,
        rect.y2 << 16 | rect.x2,
        dst.address,
        colour,
    ])
}

/// `XY_SRC_COPY_BLT`: copies `rect` of `src` to `dst` at (`x`, `y`).
pub fn copy(
    dst: &Surface,
    x: u32,
    y: u32,
    src: &Surface,
    rect: &Rect,
) -> Result<[u32; 8], BlitError> {
    dst.check()?;
    src.check()?;
    let to = Rect {
        x1: x,
        y1: y,
        x2: x.saturating_add(rect.x2.saturating_sub(rect.x1)),
        y2: y.saturating_add(rect.y2.saturating_sub(rect.y1)),
    };
    if !src.holds(rect) || !dst.holds(&to) {
        return Err(BlitError::Rect);
    }
    let (d0, d1) = (
        dst.address as u64,
        dst.address as u64 + dst.pitch as u64 * dst.height as u64,
    );
    let (s0, s1) = (
        src.address as u64,
        src.address as u64 + src.pitch as u64 * src.height as u64,
    );
    if d0 < s1 && s0 < d1 && (dst != src || !dst.pitch.is_multiple_of(64)) {
        return Err(BlitError::Overlap);
    }
    Ok([
        blit(0x53, 6) | WRITE_RGBA,
        DEPTH_32 | ROP_SOURCE | dst.pitch,
        to.y1 << 16 | to.x1,
        to.y2 << 16 | to.x2,
        dst.address,
        rect.y1 << 16 | rect.x1,
        src.pitch,
        src.address,
    ])
}
