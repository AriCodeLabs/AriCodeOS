//! AriKernel: the AriCodeOS kernel.
//!
//! Status: 0.0.3. Receives `BootInfo`, validates the protocol, brings up the
//! console (serial + framebuffer) and the leveled logger, describes the
//! hardware, installs GDT/IDT, PMM, VMM (own tables, stack with guard) and
//! heap, and announces `ARICODEOS_KERNEL_READY`.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod arch;
mod console;
mod console_input;
mod cpu;
mod events;
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
use boot_protocol::{Beacon, BootInfo, BOOT_PROTOCOL_MAGIC, BOOT_PROTOCOL_VERSION};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use log::{debug, info, trace, warn, LevelFilter};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// If `true`, the kernel terminates QEMU when done or on panic.
/// Set once from the command line, before any possible panic.
static QEMU_EXIT: AtomicBool = AtomicBool::new(false);

/// Entry point. See `boot_protocol::KernelEntry` for the contract.
///
/// # Safety
///
/// Must only be called by AriBoot, once, with interrupts disabled, paging
/// enabled (identity + kernel at `KERNEL_VIRT_BASE`), a valid stack in `rsp`
/// and `boot_info` pointing to an identity-mapped `BootInfo`.
#[no_mangle]
pub unsafe extern "C" fn kernel_entry(boot_info: *const BootInfo) -> ! {
    serial::Serial.init();
    let info = validate_boot_info(boot_info);
    let fb_end = info.framebuffer.address + info.framebuffer.size;
    memory::phys::init(info.phys_map_offset, info.phys_map_limit, fb_end);
    if info.framebuffer.address != 0 {
        // "Kernel has started" beacon for hardware without serial
        // (docs/hardware.md); the console clears it when it starts.
        let base = memory::phys::phys_to_virt(info.framebuffer.address) as *mut u8;
        // SAFETY: AriBoot mapped the whole framebuffer in the physical map.
        unsafe { info.framebuffer.beacon(base, Beacon::KERNEL_ENTRY) };
    }
    initrd::snapshot_digest(info);

    // SAFETY: AriBoot copied the cmdline to kernel memory reachable through the physical map.
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    QEMU_EXIT.store(cmdline.has(keys::QEMU_EXIT), Ordering::Relaxed);

    framebuffer::set_info(info.framebuffer);
    // With `gui` the screen belongs to the user compositor: the kernel only
    // writes to serial.
    let gui = cmdline.has(keys::GUI);
    let has_fb = !gui && console::init_framebuffer(info.framebuffer);
    println!();
    println!("AriCodeOS booting...");
    println!("AriCodeOS Kernel {VERSION}");

    let (level, level_error) = log_level_from(&cmdline);
    console::init_logger(level);
    if let Some(bad) = level_error {
        warn!("cmdline: unknown log={bad}, using {level}");
    }
    if !cmdline.is_empty() {
        info!("cmdline: {}", cmdline.raw().trim());
    }
    trace!("logger active at level {level}");

    report_framebuffer(info, has_fb, gui);
    report_kernel(info);
    report_memory(info);

    arch::x86_64::init();
    let sel = arch::x86_64::gdt::selectors();
    info!(
        "cpu: GDT, TSS (IST for #DF) and IDT installed (cs={:#x} ds={:#x} tr={:#x})",
        sel.code.0, sel.data.0, sel.tss.0
    );

    memory::pmm::init(info);
    let stack_top = memory::vmm::init(info);
    check_boot_stack(info);

    // Continue on the new stack (with a guard page); the loader's is left behind.
    // SAFETY: `stack_top` is mapped RW by the VMM; `kernel_main` does not return.
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

/// AriBoot's stack is small (64 KiB) and sits right next to the initrd: if
/// early boot overflows it, it silently corrupts the initrd. Usage is
/// measured just before leaving it and a margin is required; an excess is a
/// panic with diagnostics, not a corruption.
fn check_boot_stack(info: &BootInfo) {
    let rsp: u64;
    // SAFETY: read of the stack pointer.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack)) };
    let top = memory::phys::phys_to_virt(info.kernel_stack_base + info.kernel_stack_size);
    let used = top.saturating_sub(rsp);
    const MARGIN: u64 = 16 * 1024;
    debug!(
        "boot stack: {} KiB used of {} KiB",
        used.div_ceil(1024),
        info.kernel_stack_size / 1024
    );
    assert!(
        used + MARGIN <= info.kernel_stack_size,
        "the {} KiB boot stack is out of margin: {} bytes used",
        info.kernel_stack_size / 1024,
        used
    );
}

/// Second stage, now on the final stack and the kernel's tables.
extern "C" fn kernel_main(info: *const BootInfo) -> ! {
    // SAFETY: same pointer validated in `kernel_entry`; a kept page reachable
    // through the physical map.
    let info = unsafe { &*info };
    let cmdline = CmdLine::new(unsafe { info.cmdline() });
    debug!("kernel_main on stack {:#x}", memory::vmm::KERNEL_STACK_TOP);
    memory::heap::init();

    acpi::init(info);
    arch::x86_64::pic::disable();
    arch::x86_64::apic::init();
    arch::x86_64::ioapic::init();
    // SAFETY: IDT, APIC and all global state (behind IrqLock) are ready.
    unsafe { sync::enable_interrupts() };
    info!("interrupts enabled");

    task::init();
    console::start_cursor_thread();
    if cmdline.get(keys::SMP) == Some("off") {
        info!("smp: disabled on the command line (smp=off); 1 CPU");
    } else {
        arch::x86_64::smp::start_aps(info);
    }
    random::init();
    let layout = match cmdline.get(keys::KBD) {
        None => aricode_input::Layout::Us,
        Some(name) => aricode_input::Layout::parse(name).unwrap_or_else(|| {
            warn!("cmdline: unknown kbd={name}, using us");
            aricode_input::Layout::Us
        }),
    };
    console_input::init(layout);
    initrd::init(info);
    pci::init();
    if cmdline.get(keys::IOMMU) == Some("off") {
        info!("iommu: disabled on the command line (iommu=off); untranslated DMA");
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
        if cmdline.has(keys::RESTART_STORM) {
            init_flags |= aricode_abi::init_arg::RESTART_STORM;
        }
        if cmdline.has(keys::SERVICE_HANG) {
            init_flags |= aricode_abi::init_arg::SERVICE_HANG;
        }
        run_init(init_flags, info.framebuffer);
    }

    println!("ARICODEOS_KERNEL_READY");
    finish(qemu::ExitCode::Success)
}

/// `log=<trace|debug|info|warn|error|off>`; `info` by default.
/// Also returns the rejected value, if any, to warn about it after init.
fn log_level_from(cmdline: &CmdLine) -> (LevelFilter, Option<&'static str>) {
    match cmdline.get(keys::LOG) {
        None => (LevelFilter::Info, None),
        Some(v) => match v.parse::<LevelFilter>() {
            Ok(l) => (l, None),
            // The value lives in the cmdline (kernel memory, effectively 'static).
            Err(_) => (LevelFilter::Info, Some(unsafe { extend_lifetime(v) })),
        },
    }
}

/// The cmdline is in kernel memory that is never freed; the `&str` is valid
/// for the whole run.
///
/// # Safety
///
/// Only for strings that point into the cmdline handed over by AriBoot.
unsafe fn extend_lifetime(s: &str) -> &'static str {
    unsafe { &*(s as *const str) }
}

/// Checks the protocol magic and version; any mismatch is fatal.
unsafe fn validate_boot_info(ptr: *const BootInfo) -> &'static BootInfo {
    // SAFETY: AriBoot guarantees a valid pointer to an identity-mapped page.
    let Some(info) = ptr.as_ref() else {
        panic!("null BootInfo");
    };
    if info.magic != BOOT_PROTOCOL_MAGIC {
        panic!("BootInfo: invalid magic {:#x}", info.magic);
    }
    if info.version != BOOT_PROTOCOL_VERSION {
        panic!(
            "BootInfo: unsupported version {} (expected {})",
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
                " (reserved for the compositor)"
            } else {
                ""
            }
        );
    } else {
        warn!("framebuffer: not available, serial only");
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
    const NAMES: [&str; 6] = ["free", "boot", "firmware", "acpi", "mmio", "reserved"];
    let mut totals = [0u64; NAMES.len()];
    let mut count = 0u32;
    // SAFETY: the map is in boot memory reachable through the physical map.
    for d in unsafe { info.memory_regions() } {
        totals[d.kind() as usize] += d.size_bytes();
        count += 1;
    }
    info!(
        "memory map: {count} regions (desc {} bytes)",
        info.memory_map.descriptor_size
    );
    for (name, total) in NAMES.iter().zip(totals) {
        if total != 0 {
            debug!("  {:<9} {:>8} KiB", name, total / 1024);
        }
    }
}

/// Deliberate faults for the test scenarios (`test=<name>`).
fn run_requested_test(cmdline: &CmdLine, info: &BootInfo) {
    let Some(name) = cmdline.get(keys::TEST) else {
        return;
    };
    info!("requested test: {name}");
    match name {
        "panic" => panic!("test=panic: deliberate failure"),
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
        "ipc_late" => ipc::late_reply_selftest(),
        "drivers" => hw::selftest(info),
        "usb_hid" => hw::usb_hid_selftest(),
        "random" => random::selftest(),
        "console_input" => console_input::selftest(),
        "console_ps2" => console_input::ps2_selftest(),
        "process_reap" => user::process::reap_selftest(),
        other if other.starts_with("user_") => run_user_program(&other[5..]),
        // W^X: writing to .rodata -> protection #PF.
        "write_rodata" => {
            static RO: u64 = 7;
            // SAFETY: deliberate fault; the page is read-only.
            unsafe { (&RO as *const u64 as *mut u64).write_volatile(1) };
            unreachable!("the write to .rodata should have raised #PF");
        }
        // NX: executing from .data -> fetch #PF.
        "exec_data" => {
            // 0xC3 = `ret`.
            static mut CODE: [u8; 16] = [0xC3; 16];
            // SAFETY: deliberate fault; the page is NX.
            let f: extern "C" fn() =
                unsafe { core::mem::transmute(core::ptr::addr_of!(CODE) as *const u8) };
            f();
            unreachable!("executing .data should have raised #PF");
        }
        // Overflow the stack into the guard page -> #PF with an invalid stack -> #DF on IST.
        "stack_overflow" => {
            fn recurse(depth: u64) -> u64 {
                let buf = [depth as u8; 512];
                core::hint::black_box(&buf);
                // Opaque condition: never true, but the compiler can neither prove
                // it nor turn the recursion into a loop.
                if core::hint::black_box(depth) == u64::MAX {
                    return 0;
                }
                1 + recurse(depth + 1)
            }
            let _ = recurse(0);
            unreachable!("the recursion should have exhausted the stack");
        }
        // #BP is recoverable: the handler warns and execution continues.
        // SAFETY: debug instruction; the IDT is already installed.
        "breakpoint" => unsafe { core::arch::asm!("int3", options(nomem, nostack)) },
        // SAFETY: ud2 raises #UD deliberately; ends in a panic.
        "invalid_opcode" => unsafe { core::arch::asm!("ud2", options(nomem, nostack, noreturn)) },
        // Write to an unmapped canonical address -> #PF with a readable CR2.
        "page_fault" => {
            // SAFETY: deliberate fault; never returns.
            unsafe { (0xFFFF_9000_0000_0000 as *mut u64).write_volatile(0xDEAD) };
            unreachable!("the write should have raised #PF");
        }
        // Stack pointing at unmapped memory + int3: the CPU can push neither the
        // #BP frame (-> #PF) nor the #PF frame (-> #DF); only the IST stack
        // saves the diagnostics.
        // SAFETY: deliberate fault; never returns.
        "double_fault" => unsafe {
            core::arch::asm!("mov rsp, {0}", "int3", in(reg) 0xFFFF_9000_0000_0000u64, options(noreturn))
        },
        other => warn!("test: unknown '{other}', ignored"),
    }
}

/// Normal boot: launches `init` from the initrd with the capability to
/// create processes and waits for it to finish.
/// Launches `init` with its capabilities (`aricode_abi::handle::INIT_*`) and
/// waits for it to end. `arg` carries the `init_arg` bits (shell, gui, booted
/// slot).
fn run_init(arg: u64, fb: boot_protocol::FramebufferInfo) {
    use aricode_handles::Rights;
    let Some(elf) = user::programs::by_name("init") else {
        warn!("init: no 'init' in the initrd");
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
                    .union(Rights::MANAGE)
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
    .expect("create init");
    let status = user::process::wait(pid);
    match status {
        user::process::ExitStatus::Exited(code) => info!("init exited with code {code}"),
        user::process::ExitStatus::Killed(why) => warn!("init killed: {why}"),
    }
}

/// Launches a user program from the initrd and waits for it to end.
fn run_user_program(name: &str) {
    let Some(code) = user::programs::by_name(name) else {
        warn!("test: unknown user program '{name}'");
        return;
    };
    let pid = user::process::spawn(name, code).expect("create process");
    let status = user::process::wait(pid);
    info!("user: '{name}' exited: {status:?}");
}

/// Orderly shutdown requested from user space (`system_power`): with
/// `qemu_exit` it terminates QEMU successfully; otherwise it powers the
/// machine off through ACPI S5 and, if the firmware does not cooperate,
/// halts the CPU.
pub fn shutdown() -> ! {
    println!("ARICODEOS_SHUTDOWN");
    if !QEMU_EXIT.load(Ordering::Relaxed) {
        arch::x86_64::power::acpi_poweroff();
        println!("AriCodeOS: ACPI power-off had no effect; halting (it is safe to switch off)");
    }
    finish(qemu::ExitCode::Success)
}

/// End of execution: terminates QEMU if requested, otherwise halts the CPU.
fn finish(code: qemu::ExitCode) -> ! {
    if QEMU_EXIT.load(Ordering::Relaxed) {
        qemu::exit(code)
    }
    halt_loop()
}

pub fn halt_loop() -> ! {
    loop {
        // SAFETY: halts the CPU until the next interrupt (none in M0).
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // The other CPUs are halted so the dump does not interleave with anything.
    arch::x86_64::smp::halt_others();
    // Serial only: the panic may happen while the framebuffer console is
    // half-way through a write, and it must not touch shared state.
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
