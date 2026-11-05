// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! MMIO surface over the GOP framebuffer for `aricode-fb-console`.

use crate::sync::IrqLock;
use aricode_fb_console::{Rgb, Surface};
use boot_protocol::{BootInfo, FramebufferInfo, PixelFormat, VideoMode, VideoReport};

const _: () = assert!(BootInfo::SCREEN_MAX_BYTES <= aricode_abi::DMA_MAX_LEN);

static INFO: IrqLock<Option<FramebufferInfo>> = IrqLock::new(None);
static VIDEO: IrqLock<Option<VideoReport>> = IrqLock::new(None);

/// Stores the GOP framebuffer description, and the loader's report on how
/// it got its mode if the loader made one, for `system_info`.
pub fn set_info(info: FramebufferInfo, video: Option<VideoReport>) {
    *INFO.lock() = Some(info);
    *VIDEO.lock() = video;
}

/// The framebuffer's physical range `[start, end)`, if there is one: no
/// device BAR but the display's own may cover it (ADR 0051).
pub fn phys_range() -> Option<(u64, u64)> {
    let info = (*INFO.lock())?;
    (info.address != 0).then(|| (info.address, info.address.saturating_add(info.size)))
}

/// Whether `[phys, phys+len)` covers any of the framebuffer: mapping it
/// takes the screen from the kernel console (ADR 0074 phase B).
pub fn overlaps(phys: u64, len: u64) -> bool {
    phys_range().is_some_and(|(start, end)| phys < end && start < phys.saturating_add(len))
}

/// Framebuffer description in the ABI format (`sysinfo::Framebuffer`), or
/// `None` if there is no display.
pub fn abi_info() -> Option<aricode_abi::sysinfo::Framebuffer> {
    use aricode_abi::sysinfo::{FB_FORMAT_BGR, FB_FORMAT_BITMASK, FB_FORMAT_RGB};
    let info = (*INFO.lock())?;
    if info.address == 0 || info.pixel_format == PixelFormat::None {
        return None;
    }
    Some(aricode_abi::sysinfo::Framebuffer {
        width: info.width,
        height: info.height,
        stride: info.stride,
        format: match info.pixel_format {
            PixelFormat::Rgb => FB_FORMAT_RGB,
            PixelFormat::Bgr => FB_FORMAT_BGR,
            _ => FB_FORMAT_BITMASK,
        },
        size: info.size,
        red_mask: info.red_mask,
        green_mask: info.green_mask,
        blue_mask: info.blue_mask,
    })
}

/// The loader's video report in the ABI format (`sysinfo::Display`), or
/// `None` if there is no screen. From a loader that predates the report,
/// the source is `Unknown` and no mode is listed.
pub fn abi_display() -> Option<aricode_abi::sysinfo::Display> {
    use aricode_abi::sysinfo::{Display, DisplayMode, DisplaySource};
    abi_info()?;
    let Some(v) = *VIDEO.lock() else {
        return Some(Display::default());
    };
    let listed = (v.offered as usize)
        .min(VideoReport::MODES)
        .min(Display::MODES);
    let mut d = Display {
        source: DisplaySource::from_u32(v.source),
        offered: v.offered,
        listed: listed as u8,
        bad_request: v.bad_request != 0,
        requested: (v.requested_width, v.requested_height),
        preferred: (v.preferred_width, v.preferred_height),
        modes: [DisplayMode::default(); Display::MODES],
    };
    for (out, m) in d.modes.iter_mut().zip(&v.modes).take(listed) {
        *out = DisplayMode {
            width: m.width,
            height: m.height,
            usable: m.flags & VideoMode::USABLE != 0,
            chosen: m.flags & VideoMode::CHOSEN != 0,
        };
    }
    Some(d)
}

pub struct MmioSurface {
    info: FramebufferInfo,
    /// Virtual address of the framebuffer (through the physical map).
    base: *mut u32,
}

// SAFETY: `base` is a pointer to global MMIO owned by no thread; access is
// serialized by the console lock.
unsafe impl Send for MmioSurface {}

impl MmioSurface {
    /// `base` is the virtual address where `info.address` is mapped.
    pub fn new(info: FramebufferInfo, base: u64) -> Option<Self> {
        if info.address == 0
            || info.pixel_format == PixelFormat::None
            || info.width == 0
            || info.height == 0
        {
            return None;
        }
        Some(Self {
            info,
            base: base as *mut u32,
        })
    }

    fn encode(&self, Rgb(r, g, b): Rgb) -> u32 {
        self.info.encode(r, g, b)
    }

    /// Pointer to the first pixel of row `y` from column `x`, for `len`
    /// pixels, or `None` if any of them falls outside the width, height or
    /// the framebuffer's size.
    fn span(&self, x: usize, y: usize, len: usize) -> Option<*mut u32> {
        if y >= self.height() || x + len > self.width() {
            return None;
        }
        let idx = y * self.info.stride as usize + x;
        if (idx + len) * 4 > self.info.size as usize {
            return None;
        }
        // SAFETY: `idx + len` is inside the mapped framebuffer (checked above).
        Some(unsafe { self.base.add(idx) })
    }
}

impl Surface for MmioSurface {
    fn width(&self) -> usize {
        self.info.width as usize
    }
    fn height(&self) -> usize {
        self.info.height as usize
    }
    fn put(&mut self, x: usize, y: usize, color: Rgb) {
        if x >= self.width() || y >= self.height() {
            return;
        }
        let idx = y * self.info.stride as usize + x;
        if (idx + 1) * 4 > self.info.size as usize {
            return;
        }
        // SAFETY: `base` is the mapped framebuffer (AriBoot and then the VMM cover
        // it in the physical map); `idx` is bounded by width/height/stride and `size`.
        unsafe { self.base.add(idx).write_volatile(self.encode(color)) };
    }

    /// The console's only drawing path (incident 0031): colours encoded
    /// once, one bounds check per pixel row, and the row written in a tight
    /// loop of volatile stores. Never reads the framebuffer.
    fn glyph(&mut self, x: usize, y: usize, rows: &[u8], fg: Rgb, bg: Rgb) {
        let (fg, bg) = (self.encode(fg), self.encode(bg));
        let cols = self.width().saturating_sub(x).min(8);
        for (dy, &bits) in rows.iter().enumerate() {
            let Some(row) = self.span(x, y + dy, cols) else {
                return;
            };
            for dx in 0..cols {
                let px = if bits & (0x80 >> dx) != 0 { fg } else { bg };
                // SAFETY: `span` checked the `cols` pixels from `row`.
                unsafe { row.add(dx).write_volatile(px) };
            }
        }
    }

    fn fill_rows(&mut self, y: usize, rows: usize, color: Rgb) {
        let color = self.encode(color);
        let w = self.width();
        for yy in y..(y + rows).min(self.height()) {
            let Some(row) = self.span(0, yy, w) else {
                return;
            };
            for x in 0..w {
                // SAFETY: `span` checked the `w` pixels from `row`.
                unsafe { row.add(x).write_volatile(color) };
            }
        }
    }
}
