//! AriBoot: cargador UEFI de AriCodeOS.
//!
//! Secuencia (sección 11 del documento maestro):
//!   GOP framebuffer -> ACPI RSDP -> cargar kernel.elf -> reservar memoria ->
//!   tablas de páginas -> BootInfo -> ExitBootServices -> salto al kernel.

#![no_std]
#![no_main]

extern crate alloc;

mod qemu;

use alloc::vec::Vec;
use aricode_cmdline::{keys, CmdLine};
use aricode_elf as elf;
use aricode_x86_paging::{
    self as paging, PagePool, PageTables, GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE,
};
use boot_protocol::{
    BootInfo, FramebufferInfo, MemoryMap, PixelFormat, BOOT_PROTOCOL_MAGIC, BOOT_PROTOCOL_VERSION,
    KERNEL_VIRT_BASE, MEMORY_TYPE_KERNEL,
};
use core::ptr::NonNull;
use log::{error, info, warn};
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::mem::memory_map::MemoryMap as _;
use uefi::proto::console::gop::{self, GraphicsOutput};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
use uefi::table::cfg::ConfigTableEntry;
use uefi::{cstr16, system, CStr16, Status};

const KERNEL_STACK_PAGES: usize = 16; // 64 KiB
const KERNEL_MEM: MemoryType = MemoryType::custom(MEMORY_TYPE_KERNEL);

#[uefi::entry]
fn main() -> Status {
    uefi::helpers::init().expect("helpers");
    info!(
        "AriBoot {} (AriCodeOS UEFI loader)",
        env!("CARGO_PKG_VERSION")
    );

    // La cmdline es opcional; se copia a memoria de kernel para que sobreviva
    // a ExitBootServices y la vea AriKernel.
    let cmdline_bytes = read_file(cstr16!("\\aricodeos\\cmdline.txt")).unwrap_or_default();
    let qemu_exit = CmdLine::from_bytes(&cmdline_bytes).has(keys::QEMU_EXIT);

    match run(&cmdline_bytes) {
        Ok(never) => match never {},
        Err(e) => {
            error!("AriBoot: fallo irrecuperable: {e}");
            if qemu_exit {
                qemu::exit(qemu::ExitCode::Failed);
            }
            boot::stall(core::time::Duration::from_secs(10));
            Status::LOAD_ERROR
        }
    }
}

fn run(cmdline_bytes: &[u8]) -> Result<core::convert::Infallible, &'static str> {
    // 1. Framebuffer GOP (opcional: sin GOP seguimos solo con serie).
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
        warn!("GOP: no disponible");
    }

    // 2. ACPI RSDP.
    let rsdp = find_rsdp();
    info!("ACPI RSDP: {rsdp:#x}");

    // 3. Kernel ELF desde la ESP.
    let kernel_data = read_file(cstr16!("\\aricodeos\\kernel.elf"))
        .map_err(|_| "no se pudo leer \\aricodeos\\kernel.elf")?;
    info!("kernel.elf: {} bytes", kernel_data.len());
    let elf = elf::Elf::parse(&kernel_data).map_err(|_| "kernel.elf: ELF64 invalido")?;
    let (virt_lo, virt_hi) = elf.virt_range();
    if virt_lo != KERNEL_VIRT_BASE {
        error!("kernel enlazado en {virt_lo:#x}, se esperaba {KERNEL_VIRT_BASE:#x}");
        return Err("kernel.elf: direccion base incorrecta");
    }
    let kernel_size = virt_hi - virt_lo;
    let kernel_pages = (kernel_size / PAGE_SIZE) as usize;

    // 4. Reservar e instalar el kernel en memoria física contigua.
    let kernel_phys = alloc_pages(kernel_pages, "kernel")?;
    // SAFETY: región recién reservada de `kernel_pages` páginas.
    unsafe { core::ptr::write_bytes(kernel_phys.as_ptr(), 0, kernel_pages * PAGE_SIZE as usize) };
    for seg in &elf.segments {
        let dst = kernel_phys.as_ptr() as u64 + (seg.vaddr - virt_lo);
        let src = elf.segment_bytes(seg);
        // SAFETY: dst dentro de la región reservada (virt_range cubre todos los segmentos).
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    }
    let kernel_phys_base = kernel_phys.as_ptr() as u64;
    info!(
        "kernel: {} KiB en phys {:#x}, entry {:#x}",
        kernel_size / 1024,
        kernel_phys_base,
        elf.entry
    );

    // 5. Pila del kernel y página de BootInfo.
    let stack = alloc_pages(KERNEL_STACK_PAGES, "stack")?;
    let stack_base = stack.as_ptr() as u64;
    let stack_top = stack_base + KERNEL_STACK_PAGES as u64 * PAGE_SIZE;
    let boot_info_page = alloc_pages(1, "bootinfo")?;
    let (cmdline_address, cmdline_size) = copy_cmdline(cmdline_bytes)?;
    if cmdline_size != 0 {
        info!(
            "cmdline: {}",
            CmdLine::from_bytes(cmdline_bytes).raw().trim()
        );
    }

    // 6. Extensión física a identidad-mapear: mapa actual + framebuffer.
    let snapshot = boot::memory_map(MemoryType::LOADER_DATA).map_err(|_| "memory map")?;
    // Se excluyen Reserved/Unusable: el firmware suele anunciar aperturas MMIO
    // gigantes (p. ej. PCIe 64 bits) que el kernel mapeará bajo demanda.
    let mut phys_limit: u64 = 0;
    for d in snapshot.entries() {
        if matches!(d.ty, MemoryType::RESERVED | MemoryType::UNUSABLE) {
            continue;
        }
        phys_limit = phys_limit.max(d.phys_start + d.page_count * PAGE_SIZE);
    }
    if framebuffer.address != 0 {
        phys_limit = phys_limit.max(framebuffer.address + framebuffer.size);
    }
    phys_limit = phys_limit.max(4 << 30); // al menos 4 GiB (MMIO clásico bajo 4G)
    drop(snapshot);

    // 7. Tablas de páginas.
    let pool_pages = paging::pool_size_for(phys_limit, kernel_pages as u64);
    let pool_mem = alloc_pages(pool_pages, "page tables")?;
    let pool_base = pool_mem.as_ptr() as u64;
    let mut pool = PagePool::new(pool_mem, pool_pages);
    let mut tables = PageTables::new(&mut pool);
    tables.identity_map_2m(&mut pool, phys_limit);
    for seg in &elf.segments {
        let mut flags = GLOBAL;
        if seg.writable() {
            flags |= WRITABLE;
        }
        if !seg.executable() {
            flags |= NO_EXECUTE;
        }
        let start = seg.vaddr & !(PAGE_SIZE - 1);
        let end = (seg.vaddr + seg.memsz + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let mut v = start;
        while v < end {
            tables.map_4k(&mut pool, v, kernel_phys_base + (v - virt_lo), flags);
            v += PAGE_SIZE;
        }
    }
    info!(
        "paginas: identidad hasta {} MiB, {} tablas usadas de {}",
        phys_limit >> 20,
        pool.used_pages(pool_base),
        pool_pages
    );

    // 8. BootInfo (el mapa de memoria se rellena tras ExitBootServices).
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
        initrd_address: 0,
        initrd_size: 0,
        cmdline_address,
        cmdline_size,
    };

    info!("ExitBootServices -> AriKernel");

    // 9. Salir de boot services. A partir de aquí: sin log, sin allocator.
    // SAFETY: no quedan referencias a protocolos ni a memoria de boot services
    // que vayamos a usar; el mapa devuelto vive en LOADER_DATA (identidad).
    let final_map = unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };
    boot_info.memory_map = MemoryMap {
        descriptors: final_map.buffer().as_ptr() as u64,
        descriptor_size: final_map.meta().desc_size as u32,
        descriptor_count: final_map.len() as u32,
    };
    // El buffer del mapa debe sobrevivir: no liberar (tampoco se podría).
    core::mem::forget(final_map);

    let boot_info_ptr = boot_info_page.as_ptr() as *mut BootInfo;
    // SAFETY: página propia, alineada, identidad-mapeada en las tablas nuevas.
    unsafe { boot_info_ptr.write(boot_info) };

    // 10. Salto. rsp = top - 8 para cumplir la alineación de entrada del ABI.
    // SAFETY: tablas construidas arriba mapean este código (identidad), la pila
    // y el kernel; el kernel no retorna.
    unsafe {
        paging::enable_nxe();
        core::arch::asm!(
            "cli",
            "mov cr3, {cr3}",
            "mov rsp, {rsp}",
            "xor rbp, rbp",
            "jmp {entry}",
            cr3 = in(reg) tables.root_phys(),
            rsp = in(reg) stack_top - 8,
            entry = in(reg) elf.entry,
            in("rdi") boot_info_ptr,
            options(noreturn)
        );
    }
}

fn alloc_pages(pages: usize, what: &'static str) -> Result<NonNull<u8>, &'static str> {
    boot::allocate_pages(AllocateType::AnyPages, KERNEL_MEM, pages).map_err(|e| {
        error!("allocate_pages({pages}) para {what}: {e:?}");
        "sin memoria"
    })
}

/// Copia la cmdline a páginas de tipo kernel. Devuelve (dirección, tamaño).
fn copy_cmdline(bytes: &[u8]) -> Result<(u64, u64), &'static str> {
    if bytes.is_empty() {
        return Ok((0, 0));
    }
    let pages = bytes.len().div_ceil(PAGE_SIZE as usize);
    let dst = alloc_pages(pages, "cmdline")?;
    // SAFETY: `dst` tiene `pages * 4096 >= bytes.len()` bytes recién reservados.
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
    let Ok(mut gop) = boot::open_protocol_exclusive::<GraphicsOutput>(handle) else {
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
