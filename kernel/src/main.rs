//! AriKernel: núcleo de AriCodeOS.
//!
//! Estado: M0 First Boot. Recibe `BootInfo`, valida el protocolo, inicializa
//! la consola serie, pinta el framebuffer, describe el hardware que le han
//! entregado y anuncia `ARICODEOS_KERNEL_READY`.

#![no_std]
#![no_main]

mod framebuffer;
#[cfg(feature = "qemu-exit")]
mod qemu;
mod serial;

use boot_protocol::{BootInfo, MemoryRegionKind, BOOT_PROTOCOL_MAGIC, BOOT_PROTOCOL_VERSION};
use core::panic::PanicInfo;
use framebuffer::Framebuffer;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Punto de entrada. Ver `boot_protocol::KernelEntry` para el contrato.
///
/// # Safety
///
/// Solo debe invocarla AriBoot, una vez, con interrupciones deshabilitadas,
/// paginación activa (identidad + kernel en `KERNEL_VIRT_BASE`), una pila
/// válida en `rsp` y `boot_info` apuntando a un `BootInfo` identidad-mapeado.
#[no_mangle]
pub unsafe extern "C" fn kernel_entry(boot_info: *const BootInfo) -> ! {
    serial::Serial.init();
    println!();
    println!("AriCodeOS booting...");
    println!("AriCodeOS Kernel {VERSION}");

    // SAFETY: AriBoot garantiza un puntero válido a una página identidad-mapeada.
    let info: &BootInfo = match boot_info.as_ref() {
        Some(i) => i,
        None => panic!("BootInfo nulo"),
    };
    if info.magic != BOOT_PROTOCOL_MAGIC {
        panic!("BootInfo: magic invalido {:#x}", info.magic);
    }
    if info.version != BOOT_PROTOCOL_VERSION {
        panic!(
            "BootInfo: version {} no soportada (esperada {})",
            info.version, BOOT_PROTOCOL_VERSION
        );
    }

    if let Some(fb) = Framebuffer::new(info.framebuffer) {
        fb.draw_boot_screen();
        println!(
            "framebuffer: {}x{} stride {} {:?} @ {:#x}",
            info.framebuffer.width,
            info.framebuffer.height,
            info.framebuffer.stride,
            info.framebuffer.pixel_format,
            info.framebuffer.address
        );
    } else {
        println!("framebuffer: no disponible");
    }

    println!(
        "kernel: phys {:#x} virt {:#x} size {} KiB",
        info.kernel_phys_base,
        info.kernel_virt_base,
        info.kernel_size / 1024
    );
    println!(
        "stack: {:#x} ({} KiB)  pml4: {:#x}",
        info.kernel_stack_base,
        info.kernel_stack_size / 1024,
        info.page_table_root
    );
    println!("acpi rsdp: {:#x}", info.rsdp_address);

    report_memory(info);

    println!("ARICODEOS_KERNEL_READY");

    #[cfg(feature = "qemu-exit")]
    qemu::exit(qemu::ExitCode::Success);
    #[cfg(not(feature = "qemu-exit"))]
    halt_loop()
}

fn report_memory(info: &BootInfo) {
    let mut totals = [0u64; 7];
    let mut count = 0u32;
    // SAFETY: el mapa está en memoria de boot identidad-mapeada por AriBoot.
    for d in unsafe { info.memory_map.iter() } {
        totals[d.kind() as usize] += d.size_bytes();
        count += 1;
    }
    println!(
        "memory map: {count} regiones (desc {} bytes)",
        info.memory_map.descriptor_size
    );
    let names = [
        "free", "kernel", "boot", "firmware", "acpi", "mmio", "reserved",
    ];
    for (i, name) in names.iter().enumerate() {
        if totals[i] != 0 {
            println!("  {:<9} {:>8} KiB", name, totals[i] / 1024);
        }
    }
    let _ = MemoryRegionKind::Free;
}

pub fn halt_loop() -> ! {
    loop {
        // SAFETY: detiene la CPU hasta la siguiente interrupción (ninguna en M0).
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!();
    println!("============================");
    println!("ARICODEOS KERNEL PANIC");
    println!("============================");
    if let Some(loc) = info.location() {
        println!("Where: {}:{}:{}", loc.file(), loc.line(), loc.column());
    }
    println!("Reason: {}", info.message());
    println!("============================");
    #[cfg(feature = "qemu-exit")]
    qemu::exit(qemu::ExitCode::Failed);
    #[cfg(not(feature = "qemu-exit"))]
    halt_loop()
}
