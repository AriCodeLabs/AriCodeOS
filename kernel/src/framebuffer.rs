//! Superficie MMIO sobre el framebuffer GOP para `aricode-fb-console`.

use aricode_fb_console::{Rgb, Surface};
use boot_protocol::{FramebufferInfo, PixelFormat};

pub struct MmioSurface {
    info: FramebufferInfo,
}

impl MmioSurface {
    pub fn new(info: FramebufferInfo) -> Option<Self> {
        if info.address == 0
            || info.pixel_format == PixelFormat::None
            || info.width == 0
            || info.height == 0
        {
            return None;
        }
        Some(Self { info })
    }

    fn encode(&self, Rgb(r, g, b): Rgb) -> u32 {
        match self.info.pixel_format {
            PixelFormat::Rgb => (r as u32) | (g as u32) << 8 | (b as u32) << 16,
            PixelFormat::Bgr => (b as u32) | (g as u32) << 8 | (r as u32) << 16,
            PixelFormat::Bitmask => {
                let place = |v: u8, mask: u32| -> u32 {
                    if mask == 0 {
                        return 0;
                    }
                    let shift = mask.trailing_zeros();
                    let bits = (mask >> shift).count_ones().min(8);
                    ((v as u32) >> (8 - bits)) << shift
                };
                place(r, self.info.red_mask)
                    | place(g, self.info.green_mask)
                    | place(b, self.info.blue_mask)
            }
            PixelFormat::None => 0,
        }
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
        // SAFETY: framebuffer identidad-mapeado por AriBoot; `idx` acotado por
        // width/height/stride y por `size`.
        unsafe {
            (self.info.address as *mut u32)
                .add(idx)
                .write_volatile(self.encode(color))
        };
    }
}
