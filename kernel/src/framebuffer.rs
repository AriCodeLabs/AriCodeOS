//! Superficie MMIO sobre el framebuffer GOP para `aricode-fb-console`.

use crate::sync::IrqLock;
use aricode_fb_console::{Rgb, Surface};
use boot_protocol::{FramebufferInfo, PixelFormat};

static INFO: IrqLock<Option<FramebufferInfo>> = IrqLock::new(None);

/// Guarda la descripción del framebuffer GOP para `system_info`.
pub fn set_info(info: FramebufferInfo) {
    *INFO.lock() = Some(info);
}

/// Descripción del framebuffer en el formato del ABI (`sysinfo::Framebuffer`),
/// o `None` si no hay pantalla.
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
    /// Dirección virtual del framebuffer (a través del mapa físico).
    base: *mut u32,
}

// SAFETY: `base` es un puntero a MMIO global que no pertenece a ningún hilo;
// el acceso queda serializado por el lock de la consola.
unsafe impl Send for MmioSurface {}

impl MmioSurface {
    /// `base` es la dirección virtual donde está mapeado `info.address`.
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
        // SAFETY: `base` es el framebuffer mapeado (AriBoot y luego el VMM lo
        // cubren en el mapa físico); `idx` acotado por width/height/stride y `size`.
        unsafe { self.base.add(idx).write_volatile(self.encode(color)) };
    }
}
