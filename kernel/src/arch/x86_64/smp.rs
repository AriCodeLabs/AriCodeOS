//! Arranque de los procesadores de aplicación (Fase 32).
//!
//! El BSP copia un trampolín a una página de memoria baja y, por cada LAPIC
//! de la MADT, envía INIT y SIPI. El AP arranca en modo real en esa página,
//! pasa a modo protegido y a modo largo con unas tablas de arranque
//! (identidad del primer MiB + mitad del kernel) y salta a `ap_entry` en la
//! pila de su hilo idle, ya en la mitad alta. Allí carga el CR3 definitivo,
//! sus tablas por CPU, la IDT compartida, `syscall`, su LAPIC y entra en el
//! bucle idle del planificador.
//!
//! También viven aquí las IPI de servicio: invalidación de TLB en las demás
//! CPUs (`flush_others`) y parada de emergencia (`halt_others`).

use super::{apic, gdt, idt, pit, syscall};
use crate::cpu::{self, MAX_CPUS};
use crate::memory::{phys, pmm};
use crate::task::scheduler;
use aricode_pmm::Frame;
use aricode_sync::SpinLock;
use aricode_x86_paging::{HUGE, PAGE_SIZE, PRESENT, WRITABLE};
use boot_protocol::{BootInfo, MemoryRegionKind};
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use log::{info, warn};

/// Página física del trampolín (el SIPI la señala como `vector << 12`).
const TRAMPOLINE_PHYS: u64 = 0x8000;
const STARTUP_VECTOR: u8 = (TRAMPOLINE_PHYS >> 12) as u8;
/// Desplazamientos fijos de los datos dentro de la página del trampolín.
const GDT_OFFSET: usize = 0xE00;
const GDT_PTR_OFFSET: usize = 0xE20;
const PARAMS_OFFSET: usize = 0xF00;

// Selectores de la GDT temporal del trampolín.
// 0x08 código 32 bits, 0x10 datos, 0x18 código 64 bits.
global_asm!(
    r#"
.section .text.smp_trampoline, "ax"
.global smp_trampoline_start
.global smp_trampoline_end
.global smp_trampoline_params
.code16
smp_trampoline_start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov bx, {base} + {gdt_ptr_off}
    lgdt [bx]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    // jmp far 0x08:pm32
    .byte 0xEA
    .word (3f - smp_trampoline_start + {base})
    .word 0x08
.code32
3:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    // PAE
    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax
    // Tablas de arranque
    mov ebx, {base} + {params_off}
    mov eax, [ebx]
    mov cr3, eax
    // EFER: modo largo y NX
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8) | (1 << 11)
    wrmsr
    // Paginacion (y proteccion, ya activa)
    mov eax, cr0
    or eax, (1 << 31) | (1 << 0)
    mov cr0, eax
    // jmp far 0x18:lm64
    .byte 0xEA
    .long (4f - smp_trampoline_start + {base})
    .word 0x18
.code64
4:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    xor ax, ax
    mov fs, ax
    mov gs, ax
    mov ebx, {base} + {params_off}
    mov rsp, [rbx + 8]
    mov rdi, [rbx + 24]
    mov rax, [rbx + 16]
    jmp rax

// Datos en desplazamientos fijos dentro de la pagina: asi el codigo los
// direcciona con constantes y no con aritmetica de simbolos.
.org {gdt_off}
    // GDT temporal
    .quad 0
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A000000FFFF
.org {gdt_ptr_off}
    .word 4 * 8 - 1
    .long {base} + {gdt_off}
.org {params_off}
smp_trampoline_params:
    .quad 0     // [0]  PML4 de arranque (fisico, < 4 GiB)
    .quad 0     // [8]  rsp inicial
    .quad 0     // [16] direccion de ap_entry
    .quad 0     // [24] argumento (indice de CPU)
smp_trampoline_end:
"#,
    base = const TRAMPOLINE_PHYS,
    gdt_off = const GDT_OFFSET,
    gdt_ptr_off = const GDT_PTR_OFFSET,
    params_off = const PARAMS_OFFSET,
);

extern "C" {
    static smp_trampoline_start: u8;
    static smp_trampoline_end: u8;
    static smp_trampoline_params: u8;
}

/// CR3 del kernel y registros de control del BSP que los APs copian.
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);
static BSP_CR0: AtomicU64 = AtomicU64::new(0);
static BSP_CR4: AtomicU64 = AtomicU64::new(0);

fn read_cr(which: u8) -> u64 {
    let v: u64;
    // SAFETY: lectura de registros de control.
    unsafe {
        match which {
            0 => asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)),
            3 => asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)),
            _ => asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)),
        }
    }
    v
}

/// La página del trampolín debe ser RAM que el firmware dejó libre o de
/// arranque (nunca reservada ni NVS).
fn trampoline_page_usable(info: &BootInfo) -> bool {
    // SAFETY: el mapa vive en memoria de arranque accesible por el mapa físico.
    unsafe { info.memory_regions() }.any(|d| {
        matches!(d.kind(), MemoryRegionKind::Free | MemoryRegionKind::Boot)
            && d.phys_start <= TRAMPOLINE_PHYS
            && d.end() >= TRAMPOLINE_PHYS + PAGE_SIZE
    })
}

/// Tablas de arranque: copia del PML4 del kernel más la identidad del
/// primer MiB (una entrada de 2 MiB). Tres frames.
fn build_boot_tables(kernel_root: u64) -> Option<[Frame; 3]> {
    let frames = pmm::with(|p| {
        let a = p.allocate()?;
        let b = p.allocate()?;
        let c = p.allocate()?;
        Some([a, b, c])
    })?;
    let [pml4, pdpt, pd] = frames;
    let table = |f: Frame| phys::phys_to_virt(f.addr()) as *mut [u64; 512];
    // SAFETY: frames recién asignados, accesibles por el mapa físico; el PML4
    // del kernel es válido.
    unsafe {
        let src = &*(phys::phys_to_virt(kernel_root) as *const [u64; 512]);
        let dst = &mut *table(pml4);
        *dst = *src;
        dst[0] = pdpt.addr() | PRESENT | WRITABLE;
        let pdpt_t = &mut *table(pdpt);
        *pdpt_t = [0; 512];
        pdpt_t[0] = pd.addr() | PRESENT | WRITABLE;
        let pd_t = &mut *table(pd);
        *pd_t = [0; 512];
        pd_t[0] = PRESENT | WRITABLE | HUGE; // 0..2 MiB identidad
    }
    Some(frames)
}

/// Copia el trampolín y escribe sus parámetros para el AP `idx`.
fn install_trampoline(pml4: u64, stack_top: u64, idx: usize) {
    let (start, end, params) = (
        core::ptr::addr_of!(smp_trampoline_start) as u64,
        core::ptr::addr_of!(smp_trampoline_end) as u64,
        core::ptr::addr_of!(smp_trampoline_params) as u64,
    );
    let len = (end - start) as usize;
    assert!(len <= PAGE_SIZE as usize, "smp: trampolin de {len} bytes");
    assert_eq!(
        (params - start) as usize,
        PARAMS_OFFSET,
        "smp: layout del trampolin"
    );
    let dst = phys::phys_to_virt(TRAMPOLINE_PHYS) as *mut u8;
    // SAFETY: página baja reservada para el trampolín (PMM) y comprobada en
    // el mapa; el origen es texto del kernel legible.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, dst, len);
        let p = dst.add((params - start) as usize) as *mut u64;
        p.write_volatile(pml4);
        p.add(1).write_volatile(stack_top - 8); // como si un `call` hubiera empujado el retorno
        p.add(2)
            .write_volatile(ap_entry as *const () as usize as u64);
        p.add(3).write_volatile(idx as u64);
    }
}

/// Arranca los APs listados en la MADT. Requiere el planificador
/// inicializado (cada AP adopta un hilo idle propio) e interrupciones
/// habilitadas en el BSP.
pub fn start_aps(info: &BootInfo) {
    let ids: alloc::vec::Vec<u8> = crate::acpi::with(|a| a.madt.lapic_ids.clone());
    let bsp = apic::bsp_id();
    let aps: alloc::vec::Vec<u8> = ids.iter().copied().filter(|&id| id != bsp).collect();
    if aps.is_empty() {
        info!("smp: 1 CPU en linea (sin APs en la MADT)");
        return;
    }
    if !trampoline_page_usable(info) {
        warn!(
            "smp: la pagina {TRAMPOLINE_PHYS:#x} no es RAM libre; {} AP(s) sin arrancar",
            aps.len()
        );
        return;
    }
    let kernel_root = read_cr(3);
    KERNEL_CR3.store(kernel_root, Ordering::Relaxed);
    BSP_CR0.store(read_cr(0), Ordering::Relaxed);
    BSP_CR4.store(read_cr(4), Ordering::Relaxed);
    let Some(tables) = build_boot_tables(kernel_root) else {
        warn!("smp: sin frames para las tablas de arranque");
        return;
    };

    let mut started = 0;
    for (n, &id) in aps.iter().enumerate() {
        let idx = n + 1;
        if idx >= MAX_CPUS {
            warn!("smp: mas de {MAX_CPUS} CPUs; se ignoran las restantes");
            break;
        }
        let cpu = cpu::slot(idx);
        cpu.set_lapic_id(id as u32);
        let stack_top = match scheduler::create_idle_for_cpu(idx) {
            Ok(top) => top,
            Err(e) => {
                warn!("smp: cpu {idx}: {e}");
                break;
            }
        };
        install_trampoline(tables[0].addr(), stack_top, idx);
        apic::send_init(id as u32);
        pit::wait_ms(10);
        apic::send_startup(id as u32, STARTUP_VECTOR);
        pit::wait_ms(1);
        if !cpu.is_online() {
            apic::send_startup(id as u32, STARTUP_VECTOR);
        }
        let mut waited = 0;
        while !cpu.is_online() && waited < 500 {
            pit::wait_ms(1);
            waited += 1;
        }
        if cpu.is_online() {
            started += 1;
        } else {
            warn!("smp: la CPU {idx} (lapic {id}) no respondio al SIPI");
        }
    }
    pmm::with(|p| {
        for f in tables {
            p.free(f).expect("frames de las tablas de arranque");
        }
    });
    info!(
        "smp: {} CPUs en linea ({started} AP de {} arrancados), lapic ids {:?}",
        cpu::online_count(),
        aps.len(),
        cpu::online()
            .map(cpu::Cpu::lapic_id)
            .collect::<alloc::vec::Vec<_>>()
    );
}

/// Primer código Rust de un AP: pila de su idle, IF = 0, tablas de arranque.
extern "C" fn ap_entry(idx: usize) -> ! {
    let cpu = cpu::slot(idx);
    // SAFETY: valores tomados del BSP; el CR3 del kernel mapea este código y
    // esta pila igual que las tablas de arranque.
    unsafe {
        asm!("mov cr3, {}", in(reg) KERNEL_CR3.load(Ordering::Relaxed), options(nostack));
        asm!("mov cr4, {}", in(reg) BSP_CR4.load(Ordering::Relaxed), options(nostack));
        asm!("mov cr0, {}", in(reg) BSP_CR0.load(Ordering::Relaxed), options(nostack));
    }
    gdt::init_for(cpu);
    idt::load();
    syscall::init();
    cpu::activate(cpu);
    apic::init_local();
    scheduler::adopt_idle(idx);
    cpu.set_online();
    // SAFETY: IDT, LAPIC y estado por CPU listos.
    unsafe { crate::sync::enable_interrupts() };
    scheduler::idle_loop()
}

// --- IPI de servicio ------------------------------------------------------

/// Página a invalidar (0 = todo el TLB) y CPUs que faltan por confirmar.
static FLUSH_ADDR: AtomicU64 = AtomicU64::new(0);
static FLUSH_PENDING: AtomicUsize = AtomicUsize::new(0);
static FLUSH_SENDER: SpinLock<()> = SpinLock::new(());
static FLUSHES_SERVED: AtomicUsize = AtomicUsize::new(0);

fn invlpg(virt: u64) {
    // SAFETY: invalidación de una entrada de TLB; sin efectos en memoria.
    unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
}

/// Vacía todo el TLB de esta CPU, entradas globales incluidas.
fn flush_all_local() {
    const PGE: u64 = 1 << 7;
    let cr4 = read_cr(4);
    // SAFETY: alternar PGE (o recargar CR3) solo invalida el TLB.
    unsafe {
        if cr4 & PGE != 0 {
            asm!("mov cr4, {}", in(reg) cr4 & !PGE, options(nostack));
            asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        } else {
            asm!("mov cr3, {}", in(reg) read_cr(3), options(nostack));
        }
    }
}

/// Invalida `virt` (o todo con `None`) en las demás CPUs en linea y espera
/// su confirmación. El llamador ya invalidó la suya. Debe llamarse con las
/// interrupciones habilitadas y sin locks tomados: la espera atiende las
/// IPI de otras CPUs que estén haciendo lo mismo.
pub fn flush_others(virt: Option<u64>) {
    let others = cpu::online_count().saturating_sub(1);
    if others == 0 {
        return;
    }
    assert!(
        crate::sync::interrupts_enabled(),
        "smp: tlb shootdown con interrupciones deshabilitadas"
    );
    let _sender = FLUSH_SENDER.lock();
    FLUSH_ADDR.store(virt.unwrap_or(0), Ordering::Relaxed);
    FLUSH_PENDING.store(others, Ordering::SeqCst);
    apic::broadcast_ipi(apic::TLB_FLUSH_VECTOR);
    let mut spins = 0u64;
    while FLUSH_PENDING.load(Ordering::Acquire) != 0 {
        spins += 1;
        assert!(spins < 1 << 30, "smp: tlb shootdown sin respuesta");
        core::hint::spin_loop();
    }
}

/// Handler de `TLB_FLUSH_VECTOR` en la CPU receptora.
pub fn on_tlb_flush_ipi() {
    match FLUSH_ADDR.load(Ordering::Relaxed) {
        0 => flush_all_local(),
        v => invlpg(v),
    }
    FLUSHES_SERVED.fetch_add(1, Ordering::Relaxed);
    let _ = FLUSH_PENDING.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
}

/// IPI de invalidación atendidas por esta y las demás CPUs (autoprueba).
pub fn flushes_served() -> usize {
    FLUSHES_SERVED.load(Ordering::Relaxed)
}

/// Detiene las demás CPUs (pánico). Sin espera: la CPU que llama puede
/// estar en cualquier estado.
pub fn halt_others() {
    if cpu::online_count() > 1 {
        apic::broadcast_ipi(apic::HALT_VECTOR);
    }
}

pub fn halt_forever() -> ! {
    loop {
        // SAFETY: parada definitiva de esta CPU.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}
