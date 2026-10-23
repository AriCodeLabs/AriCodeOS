// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriCodeOS boot protocol: the ABI shared between AriBoot and AriKernel.
//!
//! This crate has no dependencies and contains no logic: only the `repr(C)`
//! layout of the information the bootloader hands to the kernel and the
//! constants both must agree on. Any incompatible change bumps
//! [`BOOT_PROTOCOL_VERSION`].

#![no_std]

/// "ARICODE1" in ASCII, little endian. First field of [`BootInfo`].
pub const BOOT_PROTOCOL_MAGIC: u64 = 0x3145_444F_4349_5241;

/// Protocol version. The kernel rejects versions it does not understand.
pub const BOOT_PROTOCOL_VERSION: u32 = 4;

/// Virtual base address where the kernel is linked and mapped (higher half,
/// last 2 GiB of the canonical space: compatible with `code-model=kernel`).
pub const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Base of the linear physical memory map (PML4 entry 256). AriBoot maps all
/// non-reserved physical memory there, RW and NX, so the kernel reaches any
/// physical address `p` at `PHYS_MAP_BASE + p` from its first instruction,
/// without depending on the identity map.
pub const PHYS_MAP_BASE: u64 = 0xFFFF_8000_0000_0000;

/// Path of the kernel inside the EFI partition, as AriBoot opens it.
pub const KERNEL_PATH: &str = "\\aricodeos\\kernel.elf";

/// Optional kernel command line (see the `aricode-cmdline` crate).
pub const CMDLINE_PATH: &str = "\\aricodeos\\cmdline.txt";

/// Signature of the kernel entry function.
///
/// AriBoot jumps here with interrupts disabled, paging enabled (identity for
/// all physical memory, physical map at `PHYS_MAP_BASE` and kernel at
/// `KERNEL_VIRT_BASE`), `rsp` pointing to the reserved stack through the
/// physical map and `rdi` = pointer to `BootInfo` through the physical map.
///
/// All addresses contained in `BootInfo` are physical; the kernel converts
/// them with `phys_map_offset`. The identity map is for the loader only and
/// the kernel must not rely on it.
pub type KernelEntry = unsafe extern "C" fn(boot_info: *const BootInfo) -> !;

/// Information handed over by the bootloader. Lives in its own page
/// (`boot_info_address`) reachable through the identity map.
///
/// All memory AriBoot reserves for the kernel is `LOADER_DATA` (ADR 0021):
/// some firmwares hang in `GetMemoryMap` with custom memory types. In the map
/// those pages are `Boot`; the kernel keeps the ones described by
/// [`BootInfo::kernel_ranges`] and reclaims the rest.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    /// Must be [`BOOT_PROTOCOL_MAGIC`].
    pub magic: u64,
    /// Must be [`BOOT_PROTOCOL_VERSION`].
    pub version: u32,
    /// `size_of::<BootInfo>()` according to the bootloader. Allows extending
    /// the structure at the end without breaking older kernels.
    pub size: u32,

    pub memory_map: MemoryMap,
    pub framebuffer: FramebufferInfo,

    /// Physical address of the ACPI RSDP, or 0 if the firmware does not expose it.
    pub rsdp_address: u64,

    /// Physical address where the kernel image was loaded.
    pub kernel_phys_base: u64,
    /// Virtual address of the first kernel byte (= `KERNEL_VIRT_BASE`).
    pub kernel_virt_base: u64,
    /// Size in bytes taken by the kernel in memory (rounded up to a page).
    pub kernel_size: u64,

    /// Physical address of the initial kernel stack (base) and its size.
    pub kernel_stack_base: u64,
    pub kernel_stack_size: u64,

    /// Physical address of the PML4 active on kernel entry.
    pub page_table_root: u64,
    /// Pages (base and size) of the tables AriBoot built; the kernel reclaims
    /// them once it activates its own.
    pub page_tables_base: u64,
    pub page_tables_size: u64,
    /// Physical address of the page containing this `BootInfo`.
    pub boot_info_address: u64,

    /// initrd: reserved for later phases. 0 if there is none.
    pub initrd_address: u64,
    pub initrd_size: u64,

    /// Command line (UTF-8, no terminator) copied by AriBoot into memory of
    /// type `MEMORY_TYPE_KERNEL`. `cmdline_size == 0` if there is none.
    pub cmdline_address: u64,
    pub cmdline_size: u64,

    /// Offset of the linear physical map (= `PHYS_MAP_BASE`): physical
    /// address `p` is reachable at `p + phys_map_offset`.
    pub phys_map_offset: u64,
    /// End (exclusive) of the physical memory covered by the linear map.
    pub phys_map_limit: u64,

    /// Kernel seed carried from the previous boot (ADR 0043): the kernel's
    /// share of the seed AriBoot read from the `ARIBOOT` partition, handed
    /// over only after its successor reached the disk, so no seed serves
    /// two boots. All zero unless `carried_seed_status` is
    /// [`carried_seed::CARRIED`]. The kernel copies it and zeroes it here
    /// before anything else runs. Appended in protocol version 4: a kernel
    /// reads it only when `size` covers it ([`BootInfo::CARRIED_SEED_END`]).
    pub carried_seed: SeedBytes,
    /// One of the [`carried_seed`] values.
    pub carried_seed_status: u32,
    pub _reserved: u32,
}

/// The carried seed's bytes in `BootInfo`: a `Debug` of `BootInfo` must
/// never print them.
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
pub struct SeedBytes(pub [u8; 32]);

impl core::fmt::Debug for SeedBytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SeedBytes(..)")
    }
}

/// `BootInfo::carried_seed_status`: what AriBoot found in the seed store.
/// The numbers are those of `aricode_abi::sysinfo::CarriedSeed`.
pub mod carried_seed {
    /// No `ARIBOOT` partition on any disk (or a loader that predates the
    /// field): there is nowhere to keep a seed.
    pub const NO_STORE: u32 = 0;
    /// A seed was read, its successor written, and the kernel's share is
    /// in `carried_seed`.
    pub const CARRIED: u32 = 1;
    /// The store is blank: the first boot of an image, which ships none.
    pub const MISSING: u32 = 2;
    /// The store holds no intact copy, or could not be read.
    pub const UNREADABLE: u32 = 3;
    /// A seed was read but its successor could not be written: it is not
    /// handed over, since the next boot would find it again.
    pub const NOT_SAVED: u32 = 4;
}

impl BootInfo {
    /// Virtual address, through the physical map, of a physical address.
    pub const fn phys_to_virt(&self, phys: u64) -> u64 {
        phys + self.phys_map_offset
    }

    /// Smallest `size` that covers `carried_seed` and its status: a loader
    /// that knows the field (an older one hands a shorter `BootInfo`).
    pub const CARRIED_SEED_END: usize = core::mem::offset_of!(BootInfo, _reserved);

    /// Command line as `&str` (empty if missing or not UTF-8).
    ///
    /// # Safety
    ///
    /// The physical map described by `phys_map_offset` must be active and the
    /// cmdline must stay immutable for the returned lifetime.
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

    /// Physical ranges `[start, end)` the kernel must keep even though the
    /// UEFI map advertises them as boot memory: image, initial stack,
    /// `BootInfo` page, initrd and cmdline (empty if absent). AriBoot's
    /// tables are not included: they are reclaimed once the kernel's are active.
    pub fn kernel_ranges(&self) -> [(u64, u64); 5] {
        let range = |base: u64, size: u64| (base, base.saturating_add(size));
        [
            range(self.kernel_phys_base, self.kernel_size),
            range(self.kernel_stack_base, self.kernel_stack_size),
            range(self.boot_info_address, 4096),
            range(self.initrd_address, self.initrd_size),
            range(self.cmdline_address, self.cmdline_size),
        ]
    }

    /// Iterator over the memory map through the physical map.
    ///
    /// # Safety
    ///
    /// The physical map must be active and the descriptor buffer intact.
    pub unsafe fn memory_regions(&self) -> MemoryMapIter {
        self.memory_map.iter(self.phys_map_offset)
    }
}

/// UEFI memory map as returned by `ExitBootServices`.
///
/// Descriptors are walked with stride `descriptor_size` (not
/// `size_of::<MemoryDescriptor>()`), as the UEFI specification requires.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryMap {
    /// Physical address of the first descriptor.
    pub descriptors: u64,
    pub descriptor_size: u32,
    pub descriptor_count: u32,
}

impl MemoryMap {
    /// Iterates the descriptors, reading them at `descriptors + virt_offset`.
    ///
    /// # Safety
    ///
    /// The descriptor buffer (`descriptors + virt_offset`,
    /// `descriptor_size * descriptor_count` bytes) must be mapped and left
    /// unmodified while the iterator lives.
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
        // SAFETY: the caller of `MemoryMap::iter` guaranteed the buffer is
        // accessible; `read_unaligned` tolerates descriptors with extra padding.
        Some(unsafe { core::ptr::read_unaligned(addr as *const MemoryDescriptor) })
    }
}

/// `EFI_MEMORY_DESCRIPTOR` (UEFI 2.x, §7.2). Layout fixed by the specification.
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

/// AriCodeOS's own classification of the UEFI memory types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegionKind {
    /// Immediately usable by the physical allocator.
    Free = 0,
    /// Bootloader and boot services data, including the pages AriBoot
    /// reserved for the kernel. Reclaimable except for
    /// [`BootInfo::kernel_ranges`].
    Boot = 1,
    /// Runtime services code/data. Reserved while they are in use.
    Firmware = 2,
    /// ACPI tables (reclaimable) and NVS (not reclaimable).
    Acpi = 3,
    /// MMIO regions / ports.
    Mmio = 4,
    /// Reserved, unusable or unknown.
    Reserved = 5,
}

impl MemoryRegionKind {
    pub fn from_uefi_type(ty: u32) -> Self {
        match ty {
            7 => Self::Free,         // EfiConventionalMemory
            1..=4 => Self::Boot,     // Loader*, BootServices*
            5 | 6 => Self::Firmware, // RuntimeServices*
            9 | 10 => Self::Acpi,    // AcpiReclaim, AcpiNvs
            11 | 12 => Self::Mmio,   // Mmio, MmioPortSpace
            _ => Self::Reserved,     // Reserved, Unusable, Pal, Persistent...
        }
    }
}

/// Pixel format of the GOP framebuffer.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 32 bpp, byte 0 = R, 1 = G, 2 = B, 3 = reserved.
    Rgb = 0,
    /// 32 bpp, byte 0 = B, 1 = G, 2 = R, 3 = reserved.
    Bgr = 1,
    /// Explicit masks in `FramebufferInfo::{red,green,blue}_mask`.
    Bitmask = 2,
    /// No accessible linear framebuffer (or GOP missing).
    None = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    /// Physical address of the framebuffer. 0 if there is none.
    pub address: u64,
    /// Size in bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per line (may be larger than `width`).
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

    /// Encodes a color (8 bits per channel) as a 32-bit pixel according to
    /// `pixel_format`. Returns 0 for `None`.
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

    /// Paints a boot beacon: a square of [`Beacon::SIZE`] pixels in the top
    /// row of the screen, at position `beacon.index`. Without a serial port,
    /// beacons tell how far the boot got before the kernel has a console
    /// (`docs/hardware.md`). Clipped to the screen and to `size`; does
    /// nothing without a framebuffer.
    ///
    /// # Safety
    /// `base` must point to the `size` bytes of the framebuffer, mapped and
    /// writable.
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
                // SAFETY: `idx` is within `size` and the screen; the caller
                // guarantees that `base` is the mapped framebuffer.
                unsafe { base.cast::<u32>().add(idx).write_volatile(pixel) };
            }
        }
    }
}

/// Boot beacon: position in the top row and color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beacon {
    pub index: u32,
    pub rgb: [u8; 3],
}

impl Beacon {
    /// Side of the square in pixels.
    pub const SIZE: u32 = 32;
    /// Spacing between beacons and from the edge.
    pub const GAP: u32 = 8;
    /// AriBoot, right before `ExitBootServices` (white).
    pub const LOADER_BEFORE_EXIT: Beacon = Beacon {
        index: 0,
        rgb: [255, 255, 255],
    };
    /// AriBoot, after `ExitBootServices` and right before jumping to the kernel (green).
    pub const LOADER_JUMP: Beacon = Beacon {
        index: 1,
        rgb: [0, 200, 0],
    };
    /// AriKernel, at entry, before the console (orange).
    pub const KERNEL_ENTRY: Beacon = Beacon {
        index: 2,
        rgb: [255, 160, 0],
    };
    /// First position of the loader steps (cyan).
    pub const FIRST_STEP: u32 = 3;

    /// Loader step `n` after the cmdline (cyan, to the right of the three
    /// above): 0 memory map read, 1 tables reserved, 2 physical map built,
    /// 3 kernel mapped. Tells a firmware hang (console or memory map) apart
    /// from one in the loader itself.
    pub const fn step(n: u32) -> Beacon {
        Beacon {
            index: Self::FIRST_STEP + n,
            rgb: [0, 255, 255],
        }
    }
}

// Compile-time layout checks.
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
        // 5-6-5 masks: each channel is truncated to its bits.
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
        // 20 px wide screen: beacon 0 is clipped in x; beacon 1 falls outside.
        let mut info = fb(20, 30, 20, PixelFormat::Bgr);
        let mut buf = vec![0u32; 20 * 30];
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_BEFORE_EXIT) };
        let lit = buf.iter().filter(|&&p| p != 0).count();
        assert_eq!(lit, ((20 - Beacon::GAP) * (30 - Beacon::GAP)) as usize);
        buf.fill(0);
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_JUMP) };
        assert!(buf.iter().all(|&p| p == 0));
        // `size` smaller than the screen: nothing is written beyond it.
        info.size = 20 * 10 * 4;
        buf.fill(0);
        unsafe { info.beacon(buf.as_mut_ptr().cast(), Beacon::LOADER_BEFORE_EXIT) };
        assert!(buf[20 * 10..].iter().all(|&p| p == 0));
        assert!(buf[..20 * 10].iter().any(|&p| p != 0));
        // Loader steps go to the right of the three fixed beacons.
        assert_eq!(Beacon::step(0).index, Beacon::KERNEL_ENTRY.index + 1);
        assert_eq!(Beacon::step(3).index, 6);
        // Without a framebuffer nothing is touched.
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
        // Custom types from the OS loader range no longer mean anything.
        assert_eq!(
            MemoryRegionKind::from_uefi_type(0x8000_0000),
            MemoryRegionKind::Reserved
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
        // UEFI may use descriptors larger than the struct: stride 48.
        const STRIDE: usize = 48;
        let mut buf = [0u8; STRIDE * 2];
        for (i, (ty, start, pages)) in [(7u32, 0x1000u64, 4u64), (2, 0x9000, 1)].iter().enumerate()
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
        assert_eq!(v.items[1].kind(), MemoryRegionKind::Boot);
    }

    #[test]
    fn kernel_ranges_cover_what_the_kernel_keeps() {
        let mut info = unsafe { core::mem::zeroed::<BootInfo>() };
        info.kernel_phys_base = 0x10_0000;
        info.kernel_size = 0x8_0000;
        info.kernel_stack_base = 0x20_0000;
        info.kernel_stack_size = 0x1_0000;
        info.boot_info_address = 0x21_0000;
        info.page_tables_base = 0x30_0000;
        info.page_tables_size = 0x4_0000;
        info.cmdline_address = 0x22_0000;
        info.cmdline_size = 17;
        let r = info.kernel_ranges();
        assert_eq!(r[0], (0x10_0000, 0x18_0000));
        assert_eq!(r[1], (0x20_0000, 0x21_0000));
        assert_eq!(r[2], (0x21_0000, 0x21_1000));
        assert_eq!(r[3], (0, 0), "no initrd: empty range");
        assert_eq!(r[4], (0x22_0000, 0x22_0011));
        // The loader's tables are reclaimed: they are not among the kept ranges.
        assert!(r.iter().all(|&(s, _)| s != info.page_tables_base));
    }

    #[test]
    fn carried_seed_is_appended_and_gated_by_size() {
        let mut info = unsafe { core::mem::zeroed::<BootInfo>() };
        // Appended after the last field of the older layout, so an older
        // loader's `size` stops before it.
        let old_end = core::mem::offset_of!(BootInfo, phys_map_limit) + 8;
        assert_eq!(core::mem::offset_of!(BootInfo, carried_seed), old_end);
        assert!(old_end < BootInfo::CARRIED_SEED_END);
        assert!(BootInfo::CARRIED_SEED_END <= core::mem::size_of::<BootInfo>());
        assert_eq!(
            BootInfo::CARRIED_SEED_END,
            core::mem::offset_of!(BootInfo, carried_seed_status) + 4
        );
        assert!(core::mem::size_of::<BootInfo>() <= 4096, "one page");
        // `Debug` never shows the seed.
        info.carried_seed = SeedBytes([0xAB; 32]);
        let text = std::format!("{info:?}");
        assert!(text.contains("SeedBytes(..)"), "{text}");
        assert!(!text.contains("171"), "{text}");
    }

    #[test]
    fn cmdline_accessor() {
        let text = b"qemu_exit test=panic";
        let mut info = unsafe { core::mem::zeroed::<BootInfo>() };
        assert_eq!(unsafe { info.cmdline() }, "");
        // Shifted "physical" address: the map offset corrects it.
        info.phys_map_offset = 0x1000;
        info.cmdline_address = text.as_ptr() as u64 - 0x1000;
        info.cmdline_size = text.len() as u64;
        assert_eq!(unsafe { info.cmdline() }, "qemu_exit test=panic");
        assert_eq!(info.phys_to_virt(0x10), 0x1010);
    }

    /// Fixed-size collector without `alloc` for `no_std` tests.
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
