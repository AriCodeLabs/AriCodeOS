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
}

// Comprobaciones de layout en tiempo de compilación.
const _: () = assert!(core::mem::size_of::<MemoryDescriptor>() == 40);
const _: () = assert!(core::mem::size_of::<FramebufferInfo>() == 48);
const _: () = assert!(core::mem::size_of::<MemoryMap>() == 16);

#[cfg(test)]
mod tests {
    use super::*;

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
