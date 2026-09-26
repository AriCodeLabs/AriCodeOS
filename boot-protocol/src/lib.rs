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
pub const BOOT_PROTOCOL_VERSION: u32 = 1;

/// Dirección virtual base donde se enlaza y se mapea el kernel (mitad alta,
/// últimos 2 GiB del espacio canónico: compatible con `code-model=kernel`).
pub const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Ruta del kernel dentro de la partición EFI, tal y como la abre AriBoot.
pub const KERNEL_PATH: &str = "\\aricodeos\\kernel.elf";

/// Tipo de memoria UEFI (rango OEM/OS loader) con el que AriBoot marca todas
/// las páginas que el kernel debe conservar: imagen del kernel, tablas de
/// páginas iniciales, pila y el propio `BootInfo`.
pub const MEMORY_TYPE_KERNEL: u32 = 0x8000_0000;

/// Firma de la función de entrada del kernel.
///
/// AriBoot salta aquí con interrupciones deshabilitadas, paginación activa
/// (identidad para toda la memoria física + kernel en `KERNEL_VIRT_BASE`),
/// `rsp` apuntando a la pila reservada y `rdi` = puntero físico a `BootInfo`.
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
    /// Itera los descriptores.
    ///
    /// # Safety
    ///
    /// El buffer de descriptores (`descriptors`, `descriptor_size *
    /// descriptor_count` bytes) debe seguir mapeado y sin modificar mientras
    /// viva el iterador.
    pub unsafe fn iter(&self) -> MemoryMapIter {
        MemoryMapIter {
            map: *self,
            index: 0,
        }
    }
}

pub struct MemoryMapIter {
    map: MemoryMap,
    index: u32,
}

impl Iterator for MemoryMapIter {
    type Item = MemoryDescriptor;

    fn next(&mut self) -> Option<MemoryDescriptor> {
        if self.index >= self.map.descriptor_count {
            return None;
        }
        let addr = self.map.descriptors + self.index as u64 * self.map.descriptor_size as u64;
        self.index += 1;
        // SAFETY: el llamador de `MemoryMap::iter` garantizó que el buffer es
        // accesible; `read_unaligned` tolera descriptores con padding extra.
        Some(unsafe { core::ptr::read_unaligned(addr as *const MemoryDescriptor) })
    }
}

/// `EFI_MEMORY_DESCRIPTOR` (UEFI 2.x, §7.2). Layout fijo por especificación.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
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
