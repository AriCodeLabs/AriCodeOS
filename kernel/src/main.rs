//! AriKernel: núcleo de AriCodeOS.
//!
//! Estado: 0.0.2 en curso. Recibe `BootInfo`, valida el protocolo, levanta la
//! consola (serie + framebuffer) y el logger con niveles, describe el hardware
//! que le han entregado y anuncia `ARICODEOS_KERNEL_READY`.

#![no_std]
#![no_main]

mod console;
mod framebuffer;
mod qemu;
mod serial;

use aricode_cmdline::{keys, CmdLine};
use boot_protocol::{BootInfo, BOOT_PROTOCOL_MAGIC, BOOT_PROTOCOL_VERSION};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use log::{debug, info, trace, warn, LevelFilter};

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
    let info = validate_boot_info(boot_info);

    // SAFETY: AriBoot copió la cmdline a memoria de kernel identidad-mapeada.
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    QEMU_EXIT.store(cmdline.has(keys::QEMU_EXIT), Ordering::Relaxed);

    let has_fb = console::init_framebuffer(info.framebuffer);
    println!();
    println!("AriCodeOS booting...");
    println!("AriCodeOS Kernel {VERSION}");

    let (level, level_error) = log_level_from(&cmdline);
    console::init_logger(level);
    if let Some(bad) = level_error {
        warn!("cmdline: log={bad} desconocido, se usa {level}");
    }
    if !cmdline.is_empty() {
        info!("cmdline: {}", cmdline.raw().trim());
    }
    trace!("logger activo en nivel {level}");

    report_framebuffer(info, has_fb);
    report_kernel(info);
    report_memory(info);

    run_requested_test(&cmdline);

    println!("ARICODEOS_KERNEL_READY");
    finish(qemu::ExitCode::Success)
}

/// `log=<trace|debug|info|warn|error|off>`; por defecto `info`.
/// Devuelve también el valor rechazado, si lo hubo, para avisar tras el init.
fn log_level_from(cmdline: &CmdLine) -> (LevelFilter, Option<&'static str>) {
    match cmdline.get(keys::LOG) {
        None => (LevelFilter::Info, None),
        Some(v) => match v.parse::<LevelFilter>() {
            Ok(l) => (l, None),
            // El valor vive en la cmdline (memoria de kernel, 'static de facto).
            Err(_) => (LevelFilter::Info, Some(unsafe { extend_lifetime(v) })),
        },
    }
}

/// La cmdline está en memoria de kernel que nunca se libera; el `&str` es
/// válido durante toda la ejecución.
///
/// # Safety
///
/// Solo para cadenas que apuntan a la cmdline entregada por AriBoot.
unsafe fn extend_lifetime(s: &str) -> &'static str {
    unsafe { &*(s as *const str) }
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

fn report_framebuffer(info: &BootInfo, has_fb: bool) {
    if has_fb {
        let f = &info.framebuffer;
        info!(
            "framebuffer: {}x{} stride {} {:?} @ {:#x}",
            f.width, f.height, f.stride, f.pixel_format, f.address
        );
    } else {
        warn!("framebuffer: no disponible, solo serie");
    }
}

fn report_kernel(info: &BootInfo) {
    info!(
        "kernel: phys {:#x} virt {:#x} size {} KiB",
        info.kernel_phys_base,
        info.kernel_virt_base,
        info.kernel_size / 1024
    );
    debug!(
        "stack: {:#x} ({} KiB)  pml4: {:#x}",
        info.kernel_stack_base,
        info.kernel_stack_size / 1024,
        info.page_table_root
    );
    info!("acpi rsdp: {:#x}", info.rsdp_address);
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
    info!(
        "memory map: {count} regiones (desc {} bytes)",
        info.memory_map.descriptor_size
    );
    for (name, total) in NAMES.iter().zip(totals) {
        if total != 0 {
            debug!("  {:<9} {:>8} KiB", name, total / 1024);
        }
    }
}

/// Fallos provocados para los escenarios de test (`test=<nombre>`).
fn run_requested_test(cmdline: &CmdLine) {
    match cmdline.get(keys::TEST) {
        None => {}
        Some("panic") => panic!("test=panic: fallo provocado"),
        Some(other) => warn!("test: '{other}' desconocido, ignorado"),
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
    // Solo serie: el panic puede ocurrir con la consola de framebuffer a medio
    // escribir y no debe tocar estado compartido.
    serial_println!();
    serial_println!("============================");
    serial_println!("ARICODEOS KERNEL PANIC");
    serial_println!("============================");
    if let Some(loc) = info.location() {
        serial_println!("Where: {}:{}:{}", loc.file(), loc.line(), loc.column());
    }
    serial_println!("Reason: {}", info.message());
    serial_println!("============================");
    finish(qemu::ExitCode::Failed)
}
