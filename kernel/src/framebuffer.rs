//! Salida gráfica mínima sobre el framebuffer GOP: prueba visual de que el
//! kernel controla la pantalla. La consola con fuente llega en la Fase 3.

use boot_protocol::{FramebufferInfo, PixelFormat};

pub struct Framebuffer {
    info: FramebufferInfo,
}

impl Framebuffer {
    pub fn new(info: FramebufferInfo) -> Option<Self> {
        if info.address == 0 || info.pixel_format == PixelFormat::None {
            return None;
        }
        Some(Self { info })
    }

    fn encode(&self, r: u8, g: u8, b: u8) -> u32 {
        match self.info.pixel_format {
            PixelFormat::Rgb => (r as u32) | (g as u32) << 8 | (b as u32) << 16,
            PixelFormat::Bgr => (b as u32) | (g as u32) << 8 | (r as u32) << 16,
            PixelFormat::Bitmask => {
                let place = |v: u8, mask: u32| -> u32 {
                    if mask == 0 {
                        return 0;
                    }
                    let shift = mask.trailing_zeros();
                    let bits = (mask >> shift).count_ones();
                    ((v as u32) >> (8 - bits.min(8))) << shift
                };
                place(r, self.info.red_mask)
                    | place(g, self.info.green_mask)
                    | place(b, self.info.blue_mask)
            }
            PixelFormat::None => 0,
        }
    }

    pub fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, rgb: (u8, u8, u8)) {
        let px = self.encode(rgb.0, rgb.1, rgb.2);
        let base = self.info.address as *mut u32;
        let x_end = (x + w).min(self.info.width);
        let y_end = (y + h).min(self.info.height);
        for row in y..y_end {
            for col in x..x_end {
                let idx = row as usize * self.info.stride as usize + col as usize;
                if idx * 4 + 4 <= self.info.size as usize {
                    // SAFETY: framebuffer identidad-mapeado por AriBoot; idx
                    // acotado por stride*height y por `size`.
                    unsafe { base.add(idx).write_volatile(px) };
                }
            }
        }
    }

    /// Fondo oscuro y una franja de color: "AriCodeOS tiene la pantalla".
    pub fn draw_boot_screen(&self) {
        let (w, h) = (self.info.width, self.info.height);
        self.fill_rect(0, 0, w, h, (0x10, 0x12, 0x18));
        let bar = (h / 24).max(4);
        self.fill_rect(0, 0, w, bar, (0x2E, 0x8B, 0xC0));
        let colors = [
            (0xE0, 0x40, 0x40),
            (0x40, 0xC0, 0x60),
            (0x40, 0x70, 0xE0),
            (0xE0, 0xC0, 0x40),
        ];
        let seg = w / 4;
        for (i, c) in colors.iter().enumerate() {
            self.fill_rect(i as u32 * seg, h - bar, seg, bar, *c);
        }
    }
}
