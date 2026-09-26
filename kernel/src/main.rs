//! AriKernel: núcleo de AriCodeOS.
//!
//! Estado: 0.0.3. Recibe `BootInfo`, valida el protocolo, levanta la consola
//! (serie + framebuffer) y el logger con niveles, describe el hardware,
//! instala GDT/IDT, PMM, VMM (tablas propias, pila con guard) y heap, y
//! anuncia `ARICODEOS_KERNEL_READY`.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod console;
mod framebuffer;
mod memory;
mod qemu;
mod serial;
mod sync;
mod task;
mod time;

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
    let fb_end = info.framebuffer.address + info.framebuffer.size;
    memory::phys::init(info.phys_map_offset, info.phys_map_limit, fb_end);

    // SAFETY: AriBoot copió la cmdline a memoria de kernel accesible por el mapa físico.
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

    arch::x86_64::init();
    let sel = arch::x86_64::gdt::selectors();
    info!(
        "cpu: GDT, TSS (IST para #DF) e IDT instaladas (cs={:#x} ds={:#x} tr={:#x})",
        sel.code.0, sel.data.0, sel.tss.0
    );

    memory::pmm::init(info);
    let stack_top = memory::vmm::init(info);

    // Continuar en la pila nueva (con guard page); la del cargador queda atrás.
    // SAFETY: `stack_top` está mapeado RW por el VMM; `kernel_main` no retorna.
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack}",
            "xor rbp, rbp",
            "call {main}",
            stack = in(reg) stack_top - 8,
            main = sym kernel_main,
            in("rdi") info as *const BootInfo,
            options(noreturn)
        );
    }
}

/// Segunda etapa, ya sobre la pila definitiva y las tablas del kernel.
extern "C" fn kernel_main(info: *const BootInfo) -> ! {
    // SAFETY: mismo puntero validado en `kernel_entry`; memoria de tipo Kernel
    // accesible por el mapa físico.
    let info = unsafe { &*info };
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    debug!("kernel_main en pila {:#x}", memory::vmm::KERNEL_STACK_TOP);
    memory::heap::init();

    arch::x86_64::pic::disable();
    arch::x86_64::apic::init();
    // SAFETY: IDT, APIC y todo el estado global (tras IrqLock) están listos.
    unsafe { sync::enable_interrupts() };
    info!("interrupciones habilitadas");

    task::init();

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
    // SAFETY: el mapa está en memoria de boot accesible por el mapa físico.
    for d in unsafe { info.memory_regions() } {
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
    let Some(name) = cmdline.get(keys::TEST) else {
        return;
    };
    info!("test provocado: {name}");
    match name {
        "panic" => panic!("test=panic: fallo provocado"),
        "pmm" => memory::pmm::selftest(),
        "vmm" => memory::vmm::selftest(),
        "heap" => memory::heap::selftest(),
        "heap_oom" => memory::heap::oom_selftest(),
        "timer" => time::selftest(),
        "threads" => task::scheduler::selftest(),
        // W^X: escribir en .rodata -> #PF por protección.
        "write_rodata" => {
            static RO: u64 = 7;
            // SAFETY: fallo deliberado; la página es de solo lectura.
            unsafe { (&RO as *const u64 as *mut u64).write_volatile(1) };
            unreachable!("la escritura en .rodata debia provocar #PF");
        }
        // NX: ejecutar desde .data -> #PF por fetch.
        "exec_data" => {
            static mut CODE: [u8; 16] = [0xC3; 16]; // ret
                                                    // SAFETY: fallo deliberado; la página es NX.
            let f: extern "C" fn() =
                unsafe { core::mem::transmute(core::ptr::addr_of!(CODE) as *const u8) };
            f();
            unreachable!("ejecutar .data debia provocar #PF");
        }
        // Desbordar la pila hasta la guard page -> #PF con pila inválida -> #DF en IST.
        "stack_overflow" => {
            fn recurse(depth: u64) -> u64 {
                let buf = [depth as u8; 512];
                core::hint::black_box(&buf);
                // Condición opaca: nunca se cumple, pero el compilador no puede
                // demostrarlo ni convertir la recursión en un bucle.
                if core::hint::black_box(depth) == u64::MAX {
                    return 0;
                }
                1 + recurse(depth + 1)
            }
            let _ = recurse(0);
            unreachable!("la recursion debia agotar la pila");
        }
        // #BP es recuperable: el handler avisa y la ejecución continúa.
        // SAFETY: instrucción de depuración; la IDT ya está instalada.
        "breakpoint" => unsafe { core::arch::asm!("int3", options(nomem, nostack)) },
        // SAFETY: ud2 provoca #UD de forma deliberada; termina en pánico.
        "invalid_opcode" => unsafe { core::arch::asm!("ud2", options(nomem, nostack, noreturn)) },
        // Escritura a una dirección canónica no mapeada -> #PF con CR2 legible.
        "page_fault" => {
            // SAFETY: fallo deliberado; nunca retorna.
            unsafe { (0xFFFF_9000_0000_0000 as *mut u64).write_volatile(0xDEAD) };
            unreachable!("la escritura debia provocar #PF");
        }
        // Pila apuntando a memoria no mapeada + int3: la CPU no puede empujar el
        // marco de #BP (-> #PF) ni el de #PF (-> #DF); solo la pila IST salva el
        // diagnóstico.
        // SAFETY: fallo deliberado; nunca retorna.
        "double_fault" => unsafe {
            core::arch::asm!("mov rsp, {0}", "int3", in(reg) 0xFFFF_9000_0000_0000u64, options(noreturn))
        },
        other => warn!("test: '{other}' desconocido, ignorado"),
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
