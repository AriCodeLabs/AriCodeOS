//! Protocolo de arranque de AriCodeOS: ABI compartido entre AriBoot y AriKernel.
//!
//! Esta crate no depende de nada y no contiene lógica: solo el layout `repr(C)`
//! de la información que el bootloader entrega al kernel y las constantes que
//! ambos deben acordar. Cualquier cambio incompatible incrementa
//! [`BOOT_PROTOCOL_VERSION`].

#![no_std]

/// "ARICODE1" en ASCII, little endian. Primer campo de [`BootInfo`].
pub const BOOT_PROTOCOL_MAGIC: u64 = 0x3145_444F_4349_5241;

/// Versión del protocolo. El kernel rechaza versiones que no entiende.
pub const BOOT_PROTOCOL_VERSION: u32 = 3;

/// Dirección virtual base donde se enlaza y se mapea el kernel (mitad alta,
/// últimos 2 GiB del espacio canónico: compatible con `code-model=kernel`).
pub const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Base del mapa lineal de memoria física (entrada 256 del PML4). AriBoot
/// mapea ahí toda la memoria física no reservada, RW y NX, de modo que el
/// kernel accede a cualquier dirección física `p` en `PHYS_MAP_BASE + p`
/// desde su primera instrucción y sin depender del mapa de identidad.
pub const PHYS_MAP_BASE: u64 = 0xFFFF_8000_0000_0000;

/// Ruta del kernel dentro de la partición EFI, tal y como la abre AriBoot.
pub const KERNEL_PATH: &str = "\\aricodeos\\kernel.elf";

/// Línea de comandos opcional del kernel (ver crate `aricode-cmdline`).
pub const CMDLINE_PATH: &str = "\\aricodeos\\cmdline.txt";

/// Tipo de memoria UEFI (rango OEM/OS loader) con el que AriBoot marca todas
/// las páginas que el kernel debe conservar: imagen del kernel, tablas de
/// páginas iniciales, pila y el propio `BootInfo`.
pub const MEMORY_TYPE_KERNEL: u32 = 0x8000_0000;

/// Firma de la función de entrada del kernel.
///
/// AriBoot salta aquí con interrupciones deshabilitadas, paginación activa
/// (identidad para toda la memoria física, mapa físico en `PHYS_MAP_BASE` y
/// kernel en `KERNEL_VIRT_BASE`), `rsp` apuntando a la pila reservada a través
/// del mapa físico y `rdi` = puntero a `BootInfo` a través del mapa físico.
///
/// Todas las direcciones contenidas en `BootInfo` son físicas; el kernel las
/// convierte con `phys_map_offset`. La identidad es solo para el cargador y
/// el kernel no debe contar con ella.
pub type KernelEntry = unsafe extern "C" fn(boot_info: *const BootInfo) -> !;

/// Información entregada por el bootloader. Vive en una página propia
/// (tipo `MEMORY_TYPE_KERNEL`) accesible por identidad.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    /// Debe ser [`BOOT_PROTOCOL_MAGIC`].
    pub magic: u64,
    /// Debe ser [`BOOT_PROTOCOL_VERSION`].
    pub version: u32,
    /// `size_of::<BootInfo>()` según el bootloader. Permite extender la
    /// estructura al final sin romper kernels antiguos.
    pub size: u32,

    pub memory_map: MemoryMap,
    pub framebuffer: FramebufferInfo,

    /// Dirección física del RSDP de ACPI, o 0 si el firmware no lo expone.
    pub rsdp_address: u64,

    /// Dirección física donde se cargó la imagen del kernel.
    pub kernel_phys_base: u64,
    /// Dirección virtual del primer byte del kernel (= `KERNEL_VIRT_BASE`).
    pub kernel_virt_base: u64,
    /// Tamaño en bytes ocupado por el kernel en memoria (redondeado a página).
    pub kernel_size: u64,

    /// Dirección física de la pila inicial del kernel (base) y su tamaño.
    pub kernel_stack_base: u64,
    pub kernel_stack_size: u64,

    /// Dirección física del PML4 activo al entrar al kernel.
    pub page_table_root: u64,

    /// initrd: reservado para fases posteriores. 0 si no hay.
    pub initrd_address: u64,
    pub initrd_size: u64,

    /// Línea de comandos (UTF-8, sin terminador) copiada por AriBoot a memoria
    /// de tipo `MEMORY_TYPE_KERNEL`. `cmdline_size == 0` si no existe.
    pub cmdline_address: u64,
    pub cmdline_size: u64,

    /// Desplazamiento del mapa físico lineal (= `PHYS_MAP_BASE`): la dirección
    /// física `p` es accesible en `p + phys_map_offset`.
    pub phys_map_offset: u64,
    /// Fin (exclusivo) de la memoria física cubierta por el mapa lineal.
    pub phys_map_limit: u64,
}

impl BootInfo {
    /// Dirección virtual, a través del mapa físico, de una dirección física.
    pub const fn phys_to_virt(&self, phys: u64) -> u64 {
        phys + self.phys_map_offset
    }

    /// Línea de comandos como `&str` (vacía si no hay o no es UTF-8).
    ///
    /// # Safety
    ///
    /// El mapa físico descrito por `phys_map_offset` debe estar activo y la
    /// cmdline debe permanecer inmutable durante el lifetime devuelto.
    pub unsafe fn cmdline(&self) -> &str {
        if self.cmdline_size == 0 {
            return "";
        }
        let bytes = core::slice::from_raw_parts(
            self.phys_to_virt(self.cmdline_address) as *const u8,
            self.cmdline_size as usize,
        );
        core::str::from_utf8(bytes).unwrap_or("")
    }

    /// Iterador sobre el mapa de memoria a través del mapa físico.
    ///
    /// # Safety
    ///
    /// El mapa físico debe estar activo y el buffer de descriptores intacto.
    pub unsafe fn memory_regions(&self) -> MemoryMapIter {
        self.memory_map.iter(self.phys_map_offset)
    }
}

/// Mapa de memoria UEFI tal y como lo devolvió `ExitBootServices`.
///
/// Los descriptores se recorren con paso `descriptor_size` (no
/// `size_of::<MemoryDescriptor>()`), como exige la especificación UEFI.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryMap {
    /// Dirección física del primer descriptor.
    pub descriptors: u64,
    pub descriptor_size: u32,
    pub descriptor_count: u32,
}

impl MemoryMap {
    /// Itera los descriptores leyéndolos en `descriptors + virt_offset`.
    ///
    /// # Safety
    ///
    /// El buffer de descriptores (`descriptors + virt_offset`,
    /// `descriptor_size * descriptor_count` bytes) debe estar mapeado y sin
    /// modificar mientras viva el iterador.
    pub unsafe fn iter(&self, virt_offset: u64) -> MemoryMapIter {
        MemoryMapIter {
            map: *self,
            virt_offset,
            index: 0,
        }
    }
}

pub struct MemoryMapIter {
    map: MemoryMap,
    virt_offset: u64,
    index: u32,
}

impl Iterator for MemoryMapIter {
    type Item = MemoryDescriptor;

    fn next(&mut self) -> Option<MemoryDescriptor> {
        if self.index >= self.map.descriptor_count {
            return None;
        }
        let addr = self.map.descriptors
            + self.virt_offset
            + self.index as u64 * self.map.descriptor_size as u64;
        self.index += 1;
        // SAFETY: el llamador de `MemoryMap::iter` garantizó que el buffer es
        // accesible; `read_unaligned` tolera descriptores con padding extra.
        Some(unsafe { core::ptr::read_unaligned(addr as *const MemoryDescriptor) })
    }
}

/// `EFI_MEMORY_DESCRIPTOR` (UEFI 2.x, §7.2). Layout fijo por especificación.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryDescriptor {
    pub ty: u32,
    pub _padding: u32,
    pub phys_start: u64,
    pub virt_start: u64,
    pub page_count: u64,
    pub attribute: u64,
}

impl MemoryDescriptor {
    pub fn kind(&self) -> MemoryRegionKind {
        MemoryRegionKind::from_uefi_type(self.ty)
    }

    pub fn size_bytes(&self) -> u64 {
        self.page_count * 4096
    }

    pub fn end(&self) -> u64 {
        self.phys_start + self.size_bytes()
    }
}

/// Clasificación propia de AriCodeOS sobre los tipos de memoria UEFI.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegionKind {
    /// Utilizable inmediatamente por el allocator físico.
    Free = 0,
    /// Imagen, tablas, pila y BootInfo del kernel. Nunca liberar.
    Kernel = 1,
    /// Datos del bootloader y de boot services. Reclamable una vez el kernel
    /// ha copiado lo que necesita del `BootInfo`.
    Boot = 2,
    /// Código/datos de runtime services. Reservado mientras se usen.
    Firmware = 3,
    /// Tablas ACPI (reclamable) y NVS (no).
    Acpi = 4,
    /// Regiones MMIO / puertos.
    Mmio = 5,
    /// Reservado, inutilizable o desconocido.
    Reserved = 6,
}

impl MemoryRegionKind {
    pub fn from_uefi_type(ty: u32) -> Self {
        match ty {
            7 => Self::Free,         // EfiConventionalMemory
            1..=4 => Self::Boot,     // Loader*, BootServices*
            5 | 6 => Self::Firmware, // RuntimeServices*
            9 | 10 => Self::Acpi,    // AcpiReclaim, AcpiNvs
            11 | 12 => Self::Mmio,   // Mmio, MmioPortSpace
            MEMORY_TYPE_KERNEL => Self::Kernel,
            _ => Self::Reserved, // Reserved, Unusable, Pal, Persistent...
        }
    }
}

/// Formato de píxel del framebuffer GOP.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 32 bpp, byte 0 = R, 1 = G, 2 = B, 3 = reservado.
    Rgb = 0,
    /// 32 bpp, byte 0 = B, 1 = G, 2 = R, 3 = reservado.
    Bgr = 1,
    /// Máscaras explícitas en `FramebufferInfo::{red,green,blue}_mask`.
    Bitmask = 2,
    /// Sin framebuffer lineal accesible (o GOP ausente).
    None = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    /// Dirección física del framebuffer. 0 si no hay.
    pub address: u64,
    /// Tamaño en bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Píxeles por línea (puede ser mayor que `width`).
    pub stride: u32,
    pub pixel_format: PixelFormat,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub _reserved: u32,
}

impl FramebufferInfo {
    pub const fn none() -> Self {
        Self {
            address: 0,
            size: 0,
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: PixelFormat::None,
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
            _reserved: 0,
        }
    }

    /// Codifica un color (8 bits por canal) como píxel de 32 bits según
    /// `pixel_format`. Con `None` devuelve 0.
    pub fn encode(&self, r: u8, g: u8, b: u8) -> u32 {
        match self.pixel_format {
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
                place(r, self.red_mask) | place(g, self.green_mask) | place(b, self.blue_mask)
            }
            PixelFormat::None => 0,
        }
    }

    /// Pinta una baliza de arranque: un cuadrado de [`Beacon::SIZE`] píxeles en
    /// la fila superior de la pantalla, en la posición `beacon.index`. Sin
    /// puerto serie, las balizas dicen hasta dónde llegó el arranque antes de
    /// que el kernel tenga consola (`docs/hardware.md`). Se recorta a la
    /// pantalla y a `size`; sin framebuffer no hace nada.
    ///
    /// # Safety
    /// `base` debe apuntar a los `size` bytes del framebuffer, mapeados y
    /// escribibles.
    pub unsafe fn beacon(&self, base: *mut u8, beacon: Beacon) {
        if self.address == 0 || self.pixel_format == PixelFormat::None {
            return;
        }
        let [r, g, b] = beacon.rgb;
        let pixel = self.encode(r, g, b);
        let x0 = Beacon::GAP + beacon.index * (Beacon::SIZE + Beacon::GAP);
        let x1 = (x0 + Beacon::SIZE).min(self.width);
        let y1 = (Beacon::GAP + Beacon::SIZE).min(self.height);
        let pixels = (self.size / 4) as usize;
        for y in Beacon::GAP..y1 {
            for x in x0..x1 {
                let idx = y as usize * self.stride as usize + x as usize;
                if idx >= pixels {
                    return;
                }
                // SAFETY: `idx` está dentro de `size` y de la pantalla; el
                // llamador garantiza que `base` es el framebuffer mapeado.
                unsafe { base.cast::<u32>().add(idx).write_volatile(pixel) };
            }
        }
    }
}

/// Baliza de arranque: posición en la fila superior y color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beacon {
    pub index: u32,
    pub rgb: [u8; 3],
}

impl Beacon {
    /// Lado del cuadrado en píxeles.
    pub const SIZE: u32 = 32;
    /// Separación entre balizas y respecto al borde.
    pub const GAP: u32 = 8;
    /// AriBoot, justo antes de `ExitBootServices` (blanca).
    pub const LOADER_BEFORE_EXIT: Beacon = Beacon {
        index: 0,
        rgb: [255, 255, 255],
    };
    /// AriBoot, tras `ExitBootServices` y justo antes de saltar al kernel (verde).
    pub const LOADER_JUMP: Beacon = Beacon {
        index: 1,
        rgb: [0, 200, 0],
    };
    /// AriKernel, en la entrada, antes de la consola (naranja).
    pub const KERNEL_ENTRY: Beacon = Beacon {
        index: 2,
        rgb: [255, 160, 0],
    };
}

// Comprobaciones de layout en tiempo de compilación.
const _: () = assert!(core::mem::size_of::<MemoryDescriptor>() == 40);
const _: () = assert!(core::mem::size_of::<FramebufferInfo>() == 48);
const _: () = assert!(core::mem::size_of::<MemoryMap>() == 16);

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn fb(width: u32, height: u32, stride: u32, pixel_format: PixelFormat) -> FramebufferInfo {
        FramebufferInfo {
            address: 0x8000_0000,
            size: stride as u64 * height as u64 * 4,
            width,
            height,
            stride,
            pixel_format,
            red_mask: 0xF800,
            green_mask: 0x07E0,
            blue_mask: 0x001F,
            _reserved: 0,
        }
    }

    #[test]
    fn encodes_pixels_by_format() {
        assert_eq!(
            fb(1, 1, 1, PixelFormat::Rgb).encode(0x11, 0x22, 0x33),
            0x0033_2211
        );
        assert_eq!(
            fb(1, 1, 1, PixelFormat::Bgr).encode(0x11, 0x22, 0x33),
            0x0011_2233
        );
        // Máscaras 5-6-5: cada canal se recorta a sus bits.
        assert_eq!(
            fb(1, 1, 1, PixelFormat::Bitmask).encode(0xFF, 0x00, 0xFF),
            0xF81F
        );
        assert_eq!(fb(1, 1, 1, PixelFormat::None).encode(0xFF, 0xFF, 0xFF), 0);
    }

    #[test]
    fn beacon_paints_its_square_only() {
        let info = fb(200, 100, 256, PixelFormat::Rgb);
        let mut buf = vec![0u32; 256 * 100];
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_JUMP) };
        let x0 = Beacon::GAP + Beacon::SIZE + Beacon::GAP;
        let pixel = info.encode(0, 200, 0);
        let lit: Vec<(usize, usize)> = (0..100)
            .flat_map(|y| (0..256).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[y * 256 + x] != 0)
            .collect();
        assert_eq!(lit.len(), (Beacon::SIZE * Beacon::SIZE) as usize);
        for &(x, y) in &lit {
            assert!(x >= x0 as usize && x < (x0 + Beacon::SIZE) as usize);
            assert!(y >= Beacon::GAP as usize && y < (Beacon::GAP + Beacon::SIZE) as usize);
            assert_eq!(buf[y * 256 + x], pixel);
        }
    }

    #[test]
    fn beacon_clips_to_screen_and_size() {
        // Pantalla de 20 px de ancho: la baliza 0 se recorta en x; la 1 cae fuera.
        let mut info = fb(20, 30, 20, PixelFormat::Bgr);
        let mut buf = vec![0u32; 20 * 30];
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_BEFORE_EXIT) };
        let lit = buf.iter().filter(|&&p| p != 0).count();
        assert_eq!(lit, ((20 - Beacon::GAP) * (30 - Beacon::GAP)) as usize);
        buf.fill(0);
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_JUMP) };
        assert!(buf.iter().all(|&p| p == 0));
        // `size` menor que la pantalla: no se escribe más allá.
        info.size = 20 * 10 * 4;
        buf.fill(0);
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_BEFORE_EXIT) };
        assert!(buf[20 * 10..].iter().all(|&p| p == 0));
        assert!(buf[..20 * 10].iter().any(|&p| p != 0));
        // Sin framebuffer no toca nada.
        let none = FramebufferInfo::none();
        unsafe { none.beacon(core::ptr::null_mut(), Beacon::KERNEL_ENTRY) };
    }

    #[test]
    fn classifies_uefi_memory_types() {
        assert_eq!(MemoryRegionKind::from_uefi_type(7), MemoryRegionKind::Free);
        for t in 1..=4 {
            assert_eq!(MemoryRegionKind::from_uefi_type(t), MemoryRegionKind::Boot);
        }
        assert_eq!(
            MemoryRegionKind::from_uefi_type(5),
            MemoryRegionKind::Firmware
        );
        assert_eq!(MemoryRegionKind::from_uefi_type(9), MemoryRegionKind::Acpi);
        assert_eq!(MemoryRegionKind::from_uefi_type(11), MemoryRegionKind::Mmio);
        assert_eq!(
            MemoryRegionKind::from_uefi_type(MEMORY_TYPE_KERNEL),
            MemoryRegionKind::Kernel
        );
        assert_eq!(
            MemoryRegionKind::from_uefi_type(0),
            MemoryRegionKind::Reserved
        );
        assert_eq!(
            MemoryRegionKind::from_uefi_type(8),
            MemoryRegionKind::Reserved
        );
        assert_eq!(
            MemoryRegionKind::from_uefi_type(0x1234),
            MemoryRegionKind::Reserved
        );
    }

    #[test]
    fn iterates_descriptors_with_firmware_stride() {
        // UEFI puede usar descriptores mas grandes que la estructura: stride 48.
        const STRIDE: usize = 48;
        let mut buf = [0u8; STRIDE * 2];
        for (i, (ty, start, pages)) in [(7u32, 0x1000u64, 4u64), (MEMORY_TYPE_KERNEL, 0x9000, 1)]
            .iter()
            .enumerate()
        {
            let d = MemoryDescriptor {
                ty: *ty,
                _padding: 0,
                phys_start: *start,
                virt_start: 0,
                page_count: *pages,
                attribute: 0,
            };
            let bytes = unsafe { core::slice::from_raw_parts(&d as *const _ as *const u8, 40) };
            buf[i * STRIDE..i * STRIDE + 40].copy_from_slice(bytes);
        }
        let map = MemoryMap {
            descriptors: buf.as_ptr() as u64,
            descriptor_size: STRIDE as u32,
            descriptor_count: 2,
        };
        let v: alloc_free::Collect<MemoryDescriptor, 2> = unsafe { map.iter(0) }.collect();
        assert_eq!(v.len, 2);
        assert_eq!(v.items[0].kind(), MemoryRegionKind::Free);
        assert_eq!(v.items[0].size_bytes(), 4 * 4096);
        assert_eq!(v.items[0].end(), 0x1000 + 4 * 4096);
        assert_eq!(v.items[1].kind(), MemoryRegionKind::Kernel);
    }

    #[test]
    fn cmdline_accessor() {
        let text = b"qemu_exit test=panic";
        let mut info = unsafe { core::mem::zeroed::<BootInfo>() };
        assert_eq!(unsafe { info.cmdline() }, "");
        // Direccion "fisica" desplazada: el offset del mapa la corrige.
        info.phys_map_offset = 0x1000;
        info.cmdline_address = text.as_ptr() as u64 - 0x1000;
        info.cmdline_size = text.len() as u64;
        assert_eq!(unsafe { info.cmdline() }, "qemu_exit test=panic");
        assert_eq!(info.phys_to_virt(0x10), 0x1010);
    }

    /// Colector fijo sin `alloc` para tests `no_std`.
    mod alloc_free {
        pub struct Collect<T: Copy + Default, const N: usize> {
            pub items: [T; N],
            pub len: usize,
        }
        impl<T: Copy + Default, const N: usize> FromIterator<T> for Collect<T, N> {
            fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
                let mut c = Self {
                    items: [T::default(); N],
                    len: 0,
                };
                for it in iter {
                    c.items[c.len] = it;
                    c.len += 1;
                }
                c
            }
        }
    }
}
