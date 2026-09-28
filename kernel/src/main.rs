//! AriKernel: núcleo de AriCodeOS.
//!
//! Estado: 0.0.3. Recibe `BootInfo`, valida el protocolo, levanta la consola
//! (serie + framebuffer) y el logger con niveles, describe el hardware,
//! instala GDT/IDT, PMM, VMM (tablas propias, pila con guard) y heap, y
//! anuncia `ARICODEOS_KERNEL_READY`.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod arch;
mod console;
mod console_input;
mod cpu;
mod framebuffer;
mod hw;
mod initrd;
mod iommu;
mod ipc;
mod memory;
mod object;
mod pci;
mod qemu;
mod random;
mod serial;
mod sync;
mod task;
mod time;
mod user;

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
    initrd::snapshot_digest(info);

    // SAFETY: AriBoot copió la cmdline a memoria de kernel accesible por el mapa físico.
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    QEMU_EXIT.store(cmdline.has(keys::QEMU_EXIT), Ordering::Relaxed);

    framebuffer::set_info(info.framebuffer);
    // Con `gui` la pantalla es del compositor de usuario: el kernel solo
    // escribe por serie.
    let gui = cmdline.has(keys::GUI);
    let has_fb = !gui && console::init_framebuffer(info.framebuffer);
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

    report_framebuffer(info, has_fb, gui);
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
    check_boot_stack(info);

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

/// La pila de AriBoot es pequeña (64 KiB) y está pegada al initrd: si el
/// arranque temprano la desborda, corrompe el initrd en silencio. Se mide
/// lo usado justo antes de abandonarla y se exige margen; un exceso es un
/// pánico con diagnóstico, no una corrupción.
fn check_boot_stack(info: &BootInfo) {
    let rsp: u64;
    // SAFETY: lectura del puntero de pila.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack)) };
    let top = memory::phys::phys_to_virt(info.kernel_stack_base + info.kernel_stack_size);
    let used = top.saturating_sub(rsp);
    const MARGIN: u64 = 16 * 1024;
    debug!(
        "stack de arranque: {} KiB usados de {} KiB",
        used.div_ceil(1024),
        info.kernel_stack_size / 1024
    );
    assert!(
        used + MARGIN <= info.kernel_stack_size,
        "la pila de arranque de {} KiB se queda sin margen: {} bytes usados",
        info.kernel_stack_size / 1024,
        used
    );
}

/// Segunda etapa, ya sobre la pila definitiva y las tablas del kernel.
extern "C" fn kernel_main(info: *const BootInfo) -> ! {
    // SAFETY: mismo puntero validado en `kernel_entry`; memoria de tipo Kernel
    // accesible por el mapa físico.
    let info = unsafe { &*info };
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    debug!("kernel_main en pila {:#x}", memory::vmm::KERNEL_STACK_TOP);
    memory::heap::init();

    acpi::init(info);
    arch::x86_64::pic::disable();
    arch::x86_64::apic::init();
    arch::x86_64::ioapic::init();
    // SAFETY: IDT, APIC y todo el estado global (tras IrqLock) están listos.
    unsafe { sync::enable_interrupts() };
    info!("interrupciones habilitadas");

    task::init();
    if cmdline.get(keys::SMP) == Some("off") {
        info!("smp: desactivado por la linea de comandos (smp=off); 1 CPU");
    } else {
        arch::x86_64::smp::start_aps(info);
    }
    random::init();
    console_input::init();
    initrd::init(info);
    pci::init();
    if cmdline.get(keys::IOMMU) == Some("off") {
        info!("iommu: desactivado por la linea de comandos (iommu=off); DMA sin traducir");
    } else {
        iommu::init(&info.framebuffer);
    }

    if cmdline.get(keys::TEST).is_some() {
        run_requested_test(&cmdline, info);
    } else {
        let mut init_flags = 0;
        if cmdline.has(keys::SHELL) {
            init_flags |= aricode_abi::init_arg::SHELL;
        }
        if cmdline.has(keys::GUI) {
            init_flags |= aricode_abi::init_arg::GUI;
        }
        match cmdline.get(keys::BOOT_SLOT) {
            Some("A") => init_flags |= aricode_abi::init_arg::BOOT_SLOT_A,
            Some("B") => init_flags |= aricode_abi::init_arg::BOOT_SLOT_B,
            _ => {}
        }
        if cmdline.has(keys::BOOT_PENDING) {
            init_flags |= aricode_abi::init_arg::BOOT_PENDING;
        }
        if cmdline.has(keys::INIT_FAIL_LATE) {
            init_flags |= aricode_abi::init_arg::FAIL_LATE;
        }
        run_init(init_flags, info.framebuffer);
    }

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

fn report_framebuffer(info: &BootInfo, has_fb: bool, gui: bool) {
    let f = &info.framebuffer;
    if has_fb || (gui && f.address != 0) {
        info!(
            "framebuffer: {}x{} stride {} {:?} @ {:#x}{}",
            f.width,
            f.height,
            f.stride,
            f.pixel_format,
            f.address,
            if gui {
                " (reservado para el compositor)"
            } else {
                ""
            }
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
fn run_requested_test(cmdline: &CmdLine, info: &BootInfo) {
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
        "threads" => task::scheduler::threads_selftest(),
        "sched" => task::scheduler::selftest(),
        "smp" => task::scheduler::smp_selftest(),
        "iommu" => iommu::selftest(),
        "ipc" => ipc::selftest(),
        "drivers" => hw::selftest(info),
        "random" => random::selftest(),
        "console_input" => console_input::selftest(),
        "console_ps2" => console_input::ps2_selftest(),
        "process_reap" => user::process::reap_selftest(),
        other if other.starts_with("user_") => run_user_program(&other[5..]),
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

/// Arranque normal: lanza `init` del initrd con la capacidad de crear
/// procesos y espera a que termine.
/// Lanza `init` con sus capacidades (`aricode_abi::handle::INIT_*`) y espera
/// su fin. `arg` lleva los bits de `init_arg` (shell, gui, slot arrancado).
fn run_init(arg: u64, fb: boot_protocol::FramebufferInfo) {
    use aricode_handles::Rights;
    let Some(elf) = user::programs::by_name("init") else {
        warn!("init: no hay 'init' en el initrd");
        return;
    };
    let spawner = object::KernelObject::new(object::ObjectKind::Spawner);
    let pci_root = object::KernelObject::new(object::ObjectKind::PciRoot);
    let hardware = object::KernelObject::new(object::ObjectKind::Hardware);
    let system = object::KernelObject::new(object::ObjectKind::System);
    let console_in = object::KernelObject::new(object::ObjectKind::Console);
    let framebuffer = object::KernelObject::new(object::ObjectKind::Mmio(hw::Region {
        phys: fb.address,
        len: fb.size,
        cacheable: false,
        source_id: None,
    }));
    let pid = user::process::spawn_with_handles_and_arg(
        "init",
        elf,
        alloc::vec![
            (
                spawner,
                Rights::READ
                    .union(Rights::MANAGE)
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER)
            ),
            (
                pci_root,
                Rights::READ
                    .union(Rights::MANAGE)
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER)
            ),
            (
                hardware,
                Rights::MANAGE
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER)
            ),
            (
                system,
                Rights::READ
                    .union(Rights::MANAGE)
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER)
            ),
            (
                console_in,
                Rights::READ
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER)
            ),
            (
                framebuffer,
                Rights::READ
                    .union(Rights::WRITE)
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER)
            ),
        ],
        arg,
        0,
    )
    .expect("crear init");
    let status = user::process::wait(pid);
    match status {
        user::process::ExitStatus::Exited(code) => info!("init terminado con codigo {code}"),
        user::process::ExitStatus::Killed(why) => warn!("init terminado por fallo: {why}"),
    }
}

/// Lanza un programa de usuario del initrd y espera su fin.
fn run_user_program(name: &str) {
    let Some(code) = user::programs::by_name(name) else {
        warn!("test: programa de usuario '{name}' desconocido");
        return;
    };
    let pid = user::process::spawn(name, code).expect("crear proceso");
    let status = user::process::wait(pid);
    info!("user: '{name}' terminado: {status:?}");
}

/// Apagado ordenado pedido desde usuario (`system_power`): con `qemu_exit`
/// termina QEMU con éxito; en hardware real detiene la CPU.
pub fn shutdown() -> ! {
    println!("ARICODEOS_SHUTDOWN");
    finish(qemu::ExitCode::Success)
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
    // Las demás CPUs se detienen para que el volcado no se mezcle con nada.
    arch::x86_64::smp::halt_others();
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
    serial_println!("CPU: {}", cpu::id());
    serial_println!("============================");
    finish(qemu::ExitCode::Failed)
}
