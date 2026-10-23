// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriBoot: the AriCodeOS UEFI loader.
//!
//! Sequence (section 11 of the master document):
//!   GOP framebuffer -> ACPI RSDP -> load kernel.elf -> reserve memory ->
//!   page tables -> BootInfo -> ExitBootServices -> jump to the kernel.
//!
//! Before that, on the disk with the `ARIBOOT` partition: the carried
//! kernel seed (its successor written first, ADR 0043) and the A/B slot.

#![no_std]
#![no_main]

extern crate alloc;

mod memmap;
mod qemu;
mod seed;
mod slots;

use alloc::vec::Vec;
use aricode_cmdline::{keys, CmdLine};
use aricode_elf as elf;
use aricode_x86_paging::{
    self as paging, BumpFrames, IdentityMap, Mapper, GLOBAL, HUGE_2M, NO_EXECUTE, PAGE_SIZE,
    WRITABLE,
};
use boot_protocol::{
    Beacon, BootInfo, FramebufferInfo, MemoryMap, MemoryRegionKind, PixelFormat,
    BOOT_PROTOCOL_MAGIC, BOOT_PROTOCOL_VERSION, KERNEL_VIRT_BASE, PHYS_MAP_BASE,
};
use core::ptr::NonNull;
use log::{error, info, warn};
use uefi::boot::{self, AllocateType, MemoryType, OpenProtocolAttributes, OpenProtocolParams};
use uefi::mem::memory_map::MemoryMap as _;
use uefi::proto::console::gop::{self, GraphicsOutput};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
use uefi::table::cfg::ConfigTableEntry;
use uefi::{cstr16, system, CStr16, Status};

const KERNEL_STACK_PAGES: usize = 16; // 64 KiB
/// Memory type of the pages reserved for the kernel. Standard on purpose (ADR
/// 0021): with a custom type from the OS loader range some firmwares hang in
/// `GetMemoryMap`. The kernel keeps these pages through `kernel_ranges`.
const KERNEL_MEM: MemoryType = MemoryType::LOADER_DATA;

#[uefi::entry]
fn main() -> Status {
    uefi::helpers::init().expect("helpers");
    info!(
        "AriBoot {} (AriCodeOS UEFI loader)",
        env!("CARGO_PKG_VERSION")
    );

    // The cmdline is optional; it is copied into kernel memory so that it
    // survives ExitBootServices and AriKernel can see it.
    let mut cmdline_bytes = read_file(cstr16!("\\aricodeos\\cmdline.txt")).unwrap_or_default();
    let qemu_exit = CmdLine::from_bytes(&cmdline_bytes).has(keys::QEMU_EXIT);

    // A/B boot: if there is a valid slot image, the kernel learns from the
    // cmdline where it booted from (and whether it must confirm it). The
    // same disk carries the kernel seed (ADR 0043).
    let slots::Found { loaded, seed } = slots::load();
    if let Some(l) = &loaded {
        cmdline_bytes.extend_from_slice(b" boot_slot=");
        cmdline_bytes.extend_from_slice(l.slot.name().as_bytes());
        if l.pending {
            cmdline_bytes.extend_from_slice(b" boot_pending");
        }
    }

    match run(&cmdline_bytes, loaded, seed) {
        Ok(never) => match never {},
        Err(e) => {
            error!("AriBoot: unrecoverable failure: {e}");
            if qemu_exit {
                qemu::exit(qemu::ExitCode::Failed);
            }
            boot::stall(core::time::Duration::from_secs(10));
            Status::LOAD_ERROR
        }
    }
}

fn run(
    cmdline_bytes: &[u8],
    loaded: Option<slots::Loaded>,
    seed: seed::Carried,
) -> Result<core::convert::Infallible, &'static str> {
    // 1. GOP framebuffer (optional: without GOP we continue with serial only).
    let framebuffer = framebuffer_info();
    if framebuffer.address != 0 {
        info!(
            "GOP: {}x{} stride {} {:?} @ {:#x}",
            framebuffer.width,
            framebuffer.height,
            framebuffer.stride,
            framebuffer.pixel_format,
            framebuffer.address
        );
    } else {
        warn!("GOP: not available");
    }

    // 2. ACPI RSDP.
    let rsdp = find_rsdp();
    info!("ACPI RSDP: {rsdp:#x}");

    // 3. Kernel ELF: from the verified slot image or, failing that, from the ESP.
    let (kernel_data, initrd_data) = match loaded {
        Some(l) => (l.kernel, Some(l.initrd)),
        None => (
            read_file(cstr16!("\\aricodeos\\kernel.elf"))
                .map_err(|_| "could not read \\aricodeos\\kernel.elf")?,
            None,
        ),
    };
    info!("kernel.elf: {} bytes", kernel_data.len());
    let elf = elf::Elf::parse(&kernel_data).map_err(|e| {
        error!("kernel.elf: {e:?}");
        match e {
            elf::ElfError::EntryOutsideText => {
                "kernel.elf: entry point outside an executable segment"
            }
            elf::ElfError::AddressOverflow | elf::ElfError::OverlappingSegments => {
                "kernel.elf: invalid segments"
            }
            _ => "kernel.elf: invalid ELF64",
        }
    })?;
    let (virt_lo, virt_hi) = elf.virt_range();
    if virt_lo != KERNEL_VIRT_BASE {
        error!("kernel linked at {virt_lo:#x}, expected {KERNEL_VIRT_BASE:#x}");
        return Err("kernel.elf: wrong base address");
    }
    let kernel_size = virt_hi - virt_lo;
    let kernel_pages = (kernel_size / PAGE_SIZE) as usize;

    // 4. Reserve contiguous physical memory and install the kernel there.
    let kernel_phys = alloc_pages(kernel_pages, "kernel")?;
    // SAFETY: freshly reserved region of `kernel_pages` pages.
    unsafe { core::ptr::write_bytes(kernel_phys.as_ptr(), 0, kernel_pages * PAGE_SIZE as usize) };
    for seg in &elf.segments {
        let src = elf.segment_bytes(seg);
        let offset = seg.vaddr - virt_lo;
        // Defence in depth: the parser already guarantees that [vaddr, end)
        // fits in virt_range; re-check it against the actual reservation.
        if offset + src.len() as u64 > kernel_size {
            return Err("kernel.elf: segment outside the reservation");
        }
        let dst = kernel_phys.as_ptr() as u64 + offset;
        // SAFETY: `dst..dst+len` lies within the reserved `kernel_pages` (checked above).
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    }
    let kernel_phys_base = kernel_phys.as_ptr() as u64;
    info!(
        "kernel: {} KiB at phys {:#x}, entry {:#x}",
        kernel_size / 1024,
        kernel_phys_base,
        elf.entry
    );

    // 5. Kernel stack and BootInfo page.
    let stack = alloc_pages(KERNEL_STACK_PAGES, "stack")?;
    let stack_base = stack.as_ptr() as u64;
    let stack_top = stack_base + KERNEL_STACK_PAGES as u64 * PAGE_SIZE;
    let boot_info_page = alloc_pages(1, "bootinfo")?;
    let (cmdline_address, cmdline_size) = copy_cmdline(cmdline_bytes)?;
    let (initrd_address, initrd_size) = load_initrd(initrd_data)?;
    if cmdline_size != 0 {
        info!(
            "cmdline: {}",
            CmdLine::from_bytes(cmdline_bytes).raw().trim()
        );
    }

    // 6. Physical extent to map: current map + framebuffer.
    // Reserved/Unusable are excluded: firmware often advertises huge MMIO
    // apertures (e.g. 64-bit PCIe) that the kernel will map on demand.
    // The step beacons (cyan) narrow down a hang on hardware even if the
    // firmware console stops drawing (docs/hardware.md).
    // SAFETY: the firmware keeps the framebuffer identity-mapped and
    // `framebuffer` describes exactly that memory.
    let step =
        |n: u32| unsafe { framebuffer.beacon(framebuffer.address as *mut u8, Beacon::step(n)) };
    let snapshot = memmap::snapshot()?;
    step(0);
    let phys_limit = snapshot
        .regions()
        .filter(|d| d.kind() != MemoryRegionKind::Reserved)
        .map(|d| d.end())
        .max()
        .unwrap_or(0)
        .max(4 << 30) // at least 4 GiB (classic MMIO below 4G)
        .next_multiple_of(HUGE_2M);
    info!(
        "memory: {} UEFI regions, physical limit {} MiB",
        snapshot.map.descriptor_count,
        phys_limit >> 20
    );
    drop(snapshot);

    // 7. Page tables: identity (for the loader only), linear physical map at
    // PHYS_MAP_BASE (what the kernel will use) and the kernel in the higher half.
    let pool_pages = paging::pool_size_for(phys_limit, 2, kernel_pages as u64);
    let pool_mem = alloc_pages(pool_pages, "page tables")?;
    step(1);
    // SAFETY: `pool_mem` is `pool_pages` pages freshly reserved by UEFI,
    // aligned, identity-mapped and exclusively ours until the kernel inherits them.
    let pool = unsafe { BumpFrames::new(pool_mem.as_ptr() as u64, pool_pages) };
    let mut tables = Mapper::new(pool, IdentityMap).map_err(|_| "tables: out of pages")?;
    tables
        .map_range_2m(0, 0, phys_limit, WRITABLE)
        .map_err(|_| "tables: identity")?;
    tables
        .map_range_2m(PHYS_MAP_BASE, 0, phys_limit, WRITABLE | NO_EXECUTE | GLOBAL)
        .map_err(|_| "tables: physical map")?;
    step(2);
    if framebuffer.address != 0 {
        let fb_start = framebuffer.address & !(HUGE_2M - 1);
        let fb_end = (framebuffer.address + framebuffer.size).next_multiple_of(HUGE_2M);
        // Only the part outside the physical map already built.
        let start = fb_start.max(phys_limit);
        if start < fb_end {
            tables
                .map_range_2m(
                    PHYS_MAP_BASE + start,
                    start,
                    fb_end - start,
                    WRITABLE | NO_EXECUTE | GLOBAL,
                )
                .map_err(|_| "tables: framebuffer")?;
        }
    }
    for seg in &elf.segments {
        let mut flags = GLOBAL;
        if seg.writable() {
            flags |= WRITABLE;
        }
        if !seg.executable() {
            flags |= NO_EXECUTE;
        }
        let (start, end) = seg.page_range();
        tables
            .map_range_4k(
                start,
                kernel_phys_base + (start - virt_lo),
                end - start,
                flags,
            )
            .map_err(|_| "tables: kernel")?;
    }
    step(3);
    info!(
        "paging: identity and physical map, {} of {} tables used",
        tables.allocator().used_pages(),
        pool_pages
    );

    // 8. BootInfo (the memory map is filled in after ExitBootServices).
    let mut boot_info = BootInfo {
        magic: BOOT_PROTOCOL_MAGIC,
        version: BOOT_PROTOCOL_VERSION,
        size: core::mem::size_of::<BootInfo>() as u32,
        memory_map: MemoryMap {
            descriptors: 0,
            descriptor_size: 0,
            descriptor_count: 0,
        },
        framebuffer,
        rsdp_address: rsdp,
        kernel_phys_base,
        kernel_virt_base: KERNEL_VIRT_BASE,
        kernel_size,
        kernel_stack_base: stack_base,
        kernel_stack_size: KERNEL_STACK_PAGES as u64 * PAGE_SIZE,
        page_table_root: tables.root_phys(),
        page_tables_base: pool_mem.as_ptr() as u64,
        page_tables_size: pool_pages as u64 * PAGE_SIZE,
        boot_info_address: boot_info_page.as_ptr() as u64,
        initrd_address,
        initrd_size,
        cmdline_address,
        cmdline_size,
        phys_map_offset: PHYS_MAP_BASE,
        phys_map_limit: phys_limit,
        carried_seed: boot_protocol::SeedBytes(seed.seed),
        carried_seed_status: seed.status,
        _reserved: 0,
    };
    // The kernel's share now lives only in `boot_info` (wiped below once
    // it is in its page) and, after the jump, in the kernel.
    drop(seed);

    info!("ExitBootServices -> AriKernel");
    // Beacons for hardware without serial (docs/hardware.md): white before
    // leaving boot services, green right before the jump.
    // SAFETY: the firmware keeps the framebuffer identity-mapped and the new
    // tables cover it too (identity up to phys_limit or the physical map);
    // `framebuffer` describes exactly that memory.
    unsafe { framebuffer.beacon(framebuffer.address as *mut u8, Beacon::LOADER_BEFORE_EXIT) };

    // 9. Exit boot services. From here on: no log, no allocator.
    // SAFETY: no references remain to protocols or boot services memory that
    // we will use; the returned map lives in LOADER_DATA (identity).
    let final_map = unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };
    boot_info.memory_map = MemoryMap {
        descriptors: final_map.buffer().as_ptr() as u64,
        descriptor_size: final_map.meta().desc_size as u32,
        descriptor_count: final_map.len() as u32,
    };
    // The map buffer must survive: do not free it (it could not be freed anyway).
    core::mem::forget(final_map);
    // SAFETY: as for the previous beacon; ExitBootServices does not change paging.
    unsafe { framebuffer.beacon(framebuffer.address as *mut u8, Beacon::LOADER_JUMP) };

    let boot_info_ptr = boot_info_page.as_ptr() as *mut BootInfo;
    // SAFETY: our own page, aligned, identity-mapped in the new tables.
    unsafe { boot_info_ptr.write(boot_info) };
    // This stack becomes free memory for the kernel: no seed left on it.
    aricode_crypto::wipe(&mut boot_info.carried_seed.0);

    // 10. Jump. The stack and BootInfo are handed over through the physical
    // map so the kernel never depends on the identity map. rsp = top - 8 to
    // satisfy the ABI entry alignment.
    // SAFETY: the tables built above map this code (identity), the stack and
    // the kernel; the kernel does not return.
    unsafe {
        paging::enable_nxe();
        core::arch::asm!(
            "cli",
            "mov cr3, {cr3}",
            "mov rsp, {rsp}",
            "xor rbp, rbp",
            "jmp {entry}",
            cr3 = in(reg) tables.root_phys(),
            rsp = in(reg) PHYS_MAP_BASE + stack_top - 8,
            entry = in(reg) elf.entry,
            in("rdi") PHYS_MAP_BASE + boot_info_ptr as u64,
            options(noreturn)
        );
    }
}

fn alloc_pages(pages: usize, what: &'static str) -> Result<NonNull<u8>, &'static str> {
    boot::allocate_pages(AllocateType::AnyPages, KERNEL_MEM, pages).map_err(|e| {
        error!("allocate_pages({pages}) for {what}: {e:?}");
        "out of memory"
    })
}

/// Copies the initrd (from the slot image, or from the ESP if there is none)
/// into kernel pages.
fn load_initrd(from_slot: Option<Vec<u8>>) -> Result<(u64, u64), &'static str> {
    let bytes = match from_slot {
        Some(b) => b,
        None => match read_file(cstr16!("\\aricodeos\\initrd.img")) {
            Ok(b) => b,
            Err(_) => {
                warn!("initrd.img: missing");
                return Ok((0, 0));
            }
        },
    };
    if bytes.is_empty() {
        return Ok((0, 0));
    }
    let pages = bytes.len().div_ceil(PAGE_SIZE as usize);
    let dst = alloc_pages(pages, "initrd")?;
    // SAFETY: `dst` has `pages * 4096 >= bytes.len()` freshly reserved bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.as_ptr(), bytes.len()) };
    let digest = aricode_crypto::sha256::sha256(&bytes);
    info!(
        "initrd.img: {} KiB at phys {:#x}, sha256 {:02x}{:02x}{:02x}{:02x}",
        bytes.len() / 1024,
        dst.as_ptr() as u64,
        digest[0],
        digest[1],
        digest[2],
        digest[3]
    );
    Ok((dst.as_ptr() as u64, bytes.len() as u64))
}

/// Copies the cmdline into kernel pages. Returns (address, size).
fn copy_cmdline(bytes: &[u8]) -> Result<(u64, u64), &'static str> {
    if bytes.is_empty() {
        return Ok((0, 0));
    }
    let pages = bytes.len().div_ceil(PAGE_SIZE as usize);
    let dst = alloc_pages(pages, "cmdline")?;
    // SAFETY: `dst` has `pages * 4096 >= bytes.len()` freshly reserved bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.as_ptr(), bytes.len()) };
    Ok((dst.as_ptr() as u64, bytes.len() as u64))
}

fn read_file(path: &CStr16) -> uefi::Result<Vec<u8>> {
    let mut fs = boot::get_image_file_system(boot::image_handle())?;
    let mut root = fs.open_volume()?;
    let handle = root.open(path, FileMode::Read, FileAttribute::empty())?;
    let mut file = handle.into_regular_file().ok_or(Status::NOT_FOUND)?;
    let info = file.get_boxed_info::<FileInfo>()?;
    let size = info.file_size() as usize;
    let mut buf = alloc::vec![0u8; size];
    let mut read = 0;
    while read < size {
        let n = file.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buf.truncate(read);
    Ok(buf)
}

fn framebuffer_info() -> FramebufferInfo {
    let Ok(handle) = boot::get_handle_for_protocol::<GraphicsOutput>() else {
        return FramebufferInfo::none();
    };
    // Non-exclusive open: an exclusive one disconnects the firmware console
    // driver and on real hardware the screen stops showing AriBoot's traces
    // (the last visible thing was the slots line).
    // SAFETY: only the framebuffer mode and address are read; the firmware
    // keeps drawing its console until ExitBootServices.
    let Ok(mut gop) = (unsafe {
        boot::open_protocol::<GraphicsOutput>(
            OpenProtocolParams {
                handle,
                agent: boot::image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
    }) else {
        return FramebufferInfo::none();
    };
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let (pixel_format, masks) = match mode.pixel_format() {
        gop::PixelFormat::Rgb => (PixelFormat::Rgb, (0, 0, 0)),
        gop::PixelFormat::Bgr => (PixelFormat::Bgr, (0, 0, 0)),
        gop::PixelFormat::Bitmask => {
            let m = mode.pixel_bitmask().unwrap_or(gop::PixelBitmask {
                red: 0,
                green: 0,
                blue: 0,
                reserved: 0,
            });
            (PixelFormat::Bitmask, (m.red, m.green, m.blue))
        }
        gop::PixelFormat::BltOnly => return FramebufferInfo::none(),
    };
    let mut fb = gop.frame_buffer();
    FramebufferInfo {
        address: fb.as_mut_ptr() as u64,
        size: fb.size() as u64,
        width: width as u32,
        height: height as u32,
        stride: mode.stride() as u32,
        pixel_format,
        red_mask: masks.0,
        green_mask: masks.1,
        blue_mask: masks.2,
        _reserved: 0,
    }
}

fn find_rsdp() -> u64 {
    system::with_config_table(|entries: &[ConfigTableEntry]| {
        let acpi2 = entries
            .iter()
            .find(|e| e.guid == ConfigTableEntry::ACPI2_GUID);
        let acpi1 = entries
            .iter()
            .find(|e| e.guid == ConfigTableEntry::ACPI_GUID);
        acpi2.or(acpi1).map(|e| e.address as u64).unwrap_or(0)
    })
}
