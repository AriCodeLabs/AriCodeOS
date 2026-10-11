//! MMIO surface over the GOP framebuffer for `aricode-fb-console`.

use crate::sync::IrqLock;
use aricode_fb_console::{Rgb, Surface};
use boot_protocol::{FramebufferInfo, PixelFormat};

static INFO: IrqLock<Option<FramebufferInfo>> = IrqLock::new(None);

/// Stores the GOP framebuffer description for `system_info`.
pub fn set_info(info: FramebufferInfo) {
    *INFO.lock() = Some(info);
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
}
