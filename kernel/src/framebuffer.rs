// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! MMIO surface over the GOP framebuffer for `aricode-fb-console`.

use crate::sync::IrqLock;
use aricode_fb_console::{Rgb, Surface};
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
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

/// The screen's description as it stands: the loader's, or the panel's own
/// size once `use_panel_size` set it.
pub fn current() -> FramebufferInfo {
    (*INFO.lock()).unwrap_or(FramebufferInfo::none())
}

/// The bytes the screen takes, of the firmware's whole region (64 MiB on the
/// reference laptop).
pub fn screen_bytes(info: &FramebufferInfo) -> u64 {
    info.size.min(info.stride as u64 * info.height as u64 * 4)
}

/// BAR 0 of the graphics device, uncached, for `use_panel_size`: kernel only,
/// and left mapped since nothing else uses this address.
const REGS_VIRT: u64 = 0xFFFF_D700_0000_0000;

struct Bar0;

impl aricode_intel_display::Regs for Bar0 {
    fn read(&self, offset: u32) -> u32 {
        // SAFETY: inside BAR 0, mapped uncached at `REGS_VIRT`, whose size
        // `use_panel_size` checked.
        unsafe { ((REGS_VIRT + offset as u64) as *const u32).read_volatile() }
    }

    fn write(&mut self, offset: u32, value: u32) {
        // SAFETY: as in `read`.
        unsafe { ((REGS_VIRT + offset as u64) as *mut u32).write_volatile(value) }
    }
}

/// Reads the device and decides, writing nothing.
fn plan_panel_size(
    info: &FramebufferInfo,
    panel: Option<(u32, u32)>,
) -> Result<Option<aricode_intel_display::Plan>, aricode_intel_display::Refusal> {
    use aricode_intel_display::{Refusal, Request, BAR0_SIZE};
    let d = crate::pci::intel_display().ok_or(Refusal::NotThisFamily)?;
    if !aricode_intel_display::SUPPORTED.contains(&d.device) {
        return Err(Refusal::NotThisFamily);
    }
    let (bar0, aperture) = (d.bar0, d.aperture);
    if bar0.1 != BAR0_SIZE
        || info.address < aperture.0
        || info.address + info.size > aperture.0 + aperture.1
    {
        return Err(Refusal::BadBars);
    }
    crate::memory::vmm::with(|m| {
        m.map_range_2m(
            REGS_VIRT,
            bar0.0,
            BAR0_SIZE,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .map_err(|_| Refusal::BadBars)?;
    aricode_intel_display::plan(
        &Bar0,
        &Request {
            device: d.device,
            panel,
            firmware: (info.width, info.height),
            firmware_stride: info.stride,
            surface: info.address - aperture.0,
            aperture_size: aperture.1,
            max_bytes: BootInfo::SCREEN_MAX_BYTES,
            region: info.size,
        },
    )
}

/// Gives the screen the panel's own size where the firmware did not offer it
/// but left the means (ADR 0079). Runs once, during boot, before any program
/// has the screen. `asked`: the command line chose (`video=`), so nothing
/// changes.
pub fn use_panel_size(asked: bool) {
    use aricode_intel_display::Refusal;
    let info = current();
    let report = *VIDEO.lock();
    let firmware_mode = report.is_none_or(|v| v.source == boot_protocol::video_source::FIRMWARE);
    if asked || !firmware_mode || info.address == 0 || info.pixel_format != PixelFormat::Bgr {
        return;
    }
    let panel = report
        .filter(|v| v.preferred_width != 0)
        .map(|v| (v.preferred_width, v.preferred_height));
    let result = plan_panel_size(&info, panel).and_then(|plan| {
        let Some(plan) = plan else {
            return Ok(None);
        };
        let base = crate::memory::phys::phys_to_virt(info.address) as *mut u32;
        for pixel in 0..plan.bytes / 4 {
            // SAFETY: `plan` found these bytes inside the firmware's region,
            // which the physical map covers.
            unsafe { base.add(pixel as usize).write_volatile(0) };
        }
        // SAFETY: a fence has no operands and touches no state.
        unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
        aricode_intel_display::apply(&mut Bar0, &plan)?;
        Ok(Some(plan))
    });
    let mut report = report.unwrap_or_default();
    match result {
        Ok(None) => {}
        Ok(Some(plan)) => {
            let new = FramebufferInfo {
                width: plan.width,
                height: plan.height,
                stride: plan.stride,
                ..info
            };
            *INFO.lock() = Some(new);
            report.source = boot_protocol::video_source::ON_RUNNING_PIPE;
            *VIDEO.lock() = Some(report);
            crate::console::screen_resized(new);
            log::info!(
                "framebuffer: {}x{}, the panel's own, on the running pipe",
                plan.width,
                plan.height
            );
        }
        // Without an EDID nothing says the screen should be another size, and
        // a graphics device without support is not a fault.
        Err(Refusal::NotThisFamily) if panel.is_none() => {}
        Err(why) => {
            log::warn!("framebuffer: the firmware's mode stays: {why:?}");
            report.native_refused = why as u32;
            *VIDEO.lock() = Some(report);
        }
    }
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
        size: screen_bytes(&info),
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
        native_refused: v.native_refused.min(u8::MAX as u32) as u8,
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
