//! AriKernel: núcleo de AriCodeOS.
//!
//! Estado: M0 First Boot. Recibe `BootInfo`, valida el protocolo, inicializa
//! la consola serie, pinta el framebuffer, describe el hardware que le han
//! entregado y anuncia `ARICODEOS_KERNEL_READY`.

#![no_std]
#![no_main]

mod framebuffer;
mod qemu;
mod serial;

use aricode_cmdline::{keys, CmdLine};
use boot_protocol::{BootInfo, BOOT_PROTOCOL_MAGIC, BOOT_PROTOCOL_VERSION};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use framebuffer::Framebuffer;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Si es `true`, el kernel termina QEMU al acabar o al entrar en pánico.
/// Se fija una vez desde la línea de comandos, antes de cualquier panic posible.
static QEMU_EXIT: AtomicBool = AtomicBool::new(false);

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

    let info = validate_boot_info(boot_info);

    // SAFETY: AriBoot copió la cmdline a memoria de kernel identidad-mapeada.
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    QEMU_EXIT.store(cmdline.has(keys::QEMU_EXIT), Ordering::Relaxed);
    if !cmdline.is_empty() {
        println!("cmdline: {}", cmdline.raw().trim());
    }

    report_framebuffer(info);
    report_kernel(info);
    report_memory(info);

    run_requested_test(&cmdline);

    println!("ARICODEOS_KERNEL_READY");
    finish(qemu::ExitCode::Success)
}

/// Comprueba magic y versión del protocolo; cualquier discrepancia es fatal.
unsafe fn validate_boot_info(ptr: *const BootInfo) -> &'static BootInfo {
    // SAFETY: AriBoot garantiza un puntero válido a una página identidad-mapeada.
    let Some(info) = ptr.as_ref() else {
        panic!("BootInfo nulo");
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
    info
}

fn report_framebuffer(info: &BootInfo) {
    match Framebuffer::new(info.framebuffer) {
        Some(fb) => {
            fb.draw_boot_screen();
            let f = &info.framebuffer;
            println!(
                "framebuffer: {}x{} stride {} {:?} @ {:#x}",
                f.width, f.height, f.stride, f.pixel_format, f.address
            );
        }
        None => println!("framebuffer: no disponible"),
    }
}

fn report_kernel(info: &BootInfo) {
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
}

fn report_memory(info: &BootInfo) {
    const NAMES: [&str; 7] = [
        "free", "kernel", "boot", "firmware", "acpi", "mmio", "reserved",
    ];
    let mut totals = [0u64; NAMES.len()];
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
    for (name, total) in NAMES.iter().zip(totals) {
        if total != 0 {
            println!("  {:<9} {:>8} KiB", name, total / 1024);
        }
    }
}

/// Fallos provocados para los escenarios de test (`test=<nombre>`).
fn run_requested_test(cmdline: &CmdLine) {
    match cmdline.get(keys::TEST) {
        None => {}
        Some("panic") => panic!("test=panic: fallo provocado"),
        Some(other) => println!("test: '{other}' desconocido, ignorado"),
    }
}

/// Fin de la ejecución: termina QEMU si se pidió, si no detiene la CPU.
fn finish(code: qemu::ExitCode) -> ! {
    if QEMU_EXIT.load(Ordering::Relaxed) {
        qemu::exit(code)
    }
    halt_loop()
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
    finish(qemu::ExitCode::Failed)
}
