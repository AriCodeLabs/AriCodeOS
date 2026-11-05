// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The display mode AriBoot boots with (ADR 0078, stage 0). Firmware often
//! leaves the GOP in a small mode (the reference laptop: 800x600 on a
//! larger panel). AriBoot chooses, in this order:
//! 1. the mode `video=WxH` asks for on the command line, if offered;
//! 2. the panel's preferred resolution, from its EDID (the first detailed
//!    timing, VESA E-EDID 1.4 section 3.10.2), if offered;
//! 3. the mode the firmware left.
//!
//! Only modes the system can draw on qualify: a pixel format of RGB or BGR,
//! and a buffer (stride x height x 4) of at most `max_bytes`, the
//! compositor's back buffer limit.

#![no_std]

/// One mode the GOP offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Pixels per scan line.
    pub stride: u32,
    /// RGB or BGR (a bit-mask or blit-only mode is not drawn on).
    pub direct: bool,
}

impl Mode {
    fn bytes(&self) -> u64 {
        self.stride as u64 * self.height as u64 * 4
    }

    /// Whether the system can draw on this mode.
    pub fn usable(&self, max_bytes: u64) -> bool {
        self.direct && self.bytes() <= max_bytes
    }
}

/// Why a mode was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `video=WxH` on the command line.
    Requested,
    /// The panel's preferred resolution (EDID).
    Edid,
    /// The firmware's mode, kept.
    Firmware,
}

impl Source {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Edid => "edid",
            Self::Firmware => "firmware",
        }
    }
}

/// `WxH`, as in `video=1366x768`.
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once('x')?;
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// The preferred resolution of an EDID block: the active size of its first
/// detailed timing descriptor. `None` for a block that is too short, lacks
/// the header, fails its checksum, or whose first descriptor is not a
/// timing (pixel clock 0).
pub fn edid_preferred(edid: &[u8]) -> Option<(u32, u32)> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    let base = edid.get(..128)?;
    if base[..8] != HEADER || base.iter().fold(0u8, |s, &b| s.wrapping_add(b)) != 0 {
        return None;
    }
    let d = &base[54..72];
    if d[0] == 0 && d[1] == 0 {
        return None;
    }
    let width = d[2] as u32 | ((d[4] as u32 & 0xF0) << 4);
    let height = d[5] as u32 | ((d[7] as u32 & 0xF0) << 4);
    (width > 0 && height > 0).then_some((width, height))
}

/// The index in `modes` to boot with, and why. `current` is the firmware's
/// mode; it is kept when nothing better qualifies, even if it does not
/// qualify itself (the system then draws nothing on it, as before).
pub fn choose(
    modes: &[Mode],
    current: usize,
    requested: Option<(u32, u32)>,
    preferred: Option<(u32, u32)>,
    max_bytes: u64,
) -> (usize, Source) {
    let find = |(w, h): (u32, u32)| {
        modes
            .iter()
            .position(|m| m.width == w && m.height == h && m.usable(max_bytes))
    };
    if let Some(i) = requested.and_then(find) {
        return (i, Source::Requested);
    }
    if let Some(i) = preferred.and_then(find) {
        return (i, Source::Edid);
    }
    (current, Source::Firmware)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EDID base block whose first detailed timing is `w` x `h`.
    fn edid(w: u32, h: u32) -> [u8; 128] {
        let mut e = [0u8; 128];
        e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        e[54] = 0x1A; // pixel clock, any non-zero
        e[55] = 0x36;
        e[56] = (w & 0xFF) as u8;
        e[58] = ((w >> 8) as u8) << 4;
        e[59] = (h & 0xFF) as u8;
        e[61] = ((h >> 8) as u8) << 4;
        let sum = e[..127].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        e[127] = 0u8.wrapping_sub(sum);
        e
    }

    fn mode(width: u32, height: u32) -> Mode {
        Mode {
            width,
            height,
            stride: width,
            direct: true,
        }
    }

    const MAX: u64 = 8 * 1024 * 1024;

    #[test]
    fn edid_gives_the_first_detailed_timing() {
        assert_eq!(edid_preferred(&edid(1366, 768)), Some((1366, 768)));
        assert_eq!(edid_preferred(&edid(1920, 1080)), Some((1920, 1080)));
        let mut bad = edid(1366, 768);
        bad[100] ^= 1;
        assert_eq!(edid_preferred(&bad), None, "checksum");
        assert_eq!(edid_preferred(&bad[..100]), None, "too short");
        let mut no_timing = edid(1366, 768);
        no_timing[54] = 0;
        no_timing[55] = 0;
        no_timing[127] = no_timing[127].wrapping_add(0x1A + 0x36);
        assert_eq!(edid_preferred(&no_timing), None, "a display descriptor");
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("1366x768"), Some((1366, 768)));
        assert_eq!(parse_size("0x768"), None);
        assert_eq!(parse_size("1366"), None);
        assert_eq!(parse_size("ax768"), None);
    }

    #[test]
    fn request_then_edid_then_firmware() {
        let modes = [
            mode(800, 600),
            mode(1024, 768),
            mode(1366, 768),
            mode(2560, 1600),
        ];
        // The laptop's case: the firmware left 800x600, the panel is 1366x768.
        assert_eq!(
            choose(&modes, 0, None, Some((1366, 768)), MAX),
            (2, Source::Edid)
        );
        // A request wins over the EDID.
        assert_eq!(
            choose(&modes, 0, Some((1024, 768)), Some((1366, 768)), MAX),
            (1, Source::Requested)
        );
        // A request or a preference not offered falls through.
        assert_eq!(
            choose(&modes, 0, Some((1280, 1024)), Some((1366, 768)), MAX),
            (2, Source::Edid)
        );
        assert_eq!(
            choose(&modes, 3, None, Some((1600, 900)), MAX),
            (3, Source::Firmware)
        );
        // No EDID: the firmware's mode (QEMU stays at its 1280x800).
        assert_eq!(choose(&modes, 1, None, None, MAX), (1, Source::Firmware));
    }

    #[test]
    fn modes_that_cannot_be_drawn_on_do_not_qualify() {
        let mut bitmask = mode(1366, 768);
        bitmask.direct = false;
        let modes = [mode(800, 600), bitmask, mode(2560, 1600)];
        assert_eq!(
            choose(&modes, 0, None, Some((1366, 768)), MAX),
            (0, Source::Firmware)
        );
        // 2560x1600 needs 15.6 MiB: over the compositor's buffer.
        assert_eq!(
            choose(&modes, 0, Some((2560, 1600)), None, MAX),
            (0, Source::Firmware)
        );
        // The stride counts, not the width.
        let mut padded = mode(1920, 1080);
        padded.stride = 2048;
        assert_eq!(
            choose(&[mode(800, 600), padded], 0, None, Some((1920, 1080)), MAX).1,
            Source::Firmware
        );
    }
}
