// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Application processor startup (Phase 32).
//!
//! The BSP copies a trampoline to a low-memory page and, for each LAPIC in
//! the MADT, sends INIT and SIPI. The AP starts in real mode on that page,
//! switches to protected mode and then long mode with boot tables (identity
//! map of the first MiB + kernel half) and jumps to `ap_entry` on its idle
//! thread's stack, already in the higher half. There it loads the final
//! CR3, its per-CPU tables, the shared IDT, `syscall`, its LAPIC and enters
//! the scheduler's idle loop.
//!
//! The service IPIs live here too: TLB invalidation on the other CPUs
//! (`flush_others`) and emergency halt (`halt_others`).

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

/// Physical page of the trampoline (the SIPI points at it as `vector << 12`).
const TRAMPOLINE_PHYS: u64 = 0x8000;
const STARTUP_VECTOR: u8 = (TRAMPOLINE_PHYS >> 12) as u8;
/// Fixed offsets of the data within the trampoline page.
const GDT_OFFSET: usize = 0xE00;
const GDT_PTR_OFFSET: usize = 0xE20;
const PARAMS_OFFSET: usize = 0xF00;

// Selectors of the trampoline's temporary GDT.
// 0x08 32-bit code, 0x10 data, 0x18 64-bit code.
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
    // Boot tables
    mov ebx, {base} + {params_off}
    mov eax, [ebx]
    mov cr3, eax
    // EFER: long mode and NX
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8) | (1 << 11)
    wrmsr
    // Paging (and protection, already on)
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

// Data at fixed offsets within the page: the code thus addresses it with
// constants rather than symbol arithmetic.
.org {gdt_off}
    // temporary GDT
    .quad 0
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A000000FFFF
.org {gdt_ptr_off}
    .word 4 * 8 - 1
    .long {base} + {gdt_off}
.org {params_off}
smp_trampoline_params:
    .quad 0     // [0]  boot PML4 (physical, < 4 GiB)
    .quad 0     // [8]  initial rsp
    .quad 0     // [16] address of ap_entry
    .quad 0     // [24] argument (CPU index)
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

/// Kernel CR3 and BSP control registers that the APs copy.
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);
static BSP_CR0: AtomicU64 = AtomicU64::new(0);
static BSP_CR4: AtomicU64 = AtomicU64::new(0);

fn read_cr(which: u8) -> u64 {
    let v: u64;
    // SAFETY: read of control registers.
    unsafe {
        match which {
            0 => asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)),
            3 => asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)),
            _ => asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)),
        }
    }
    v
}

/// The trampoline page must be RAM the firmware left free or as boot memory
/// (never reserved or NVS).
fn trampoline_page_usable(info: &BootInfo) -> bool {
    // SAFETY: the map lives in boot memory reachable through the physical map.
    unsafe { info.memory_regions() }.any(|d| {
        matches!(d.kind(), MemoryRegionKind::Free | MemoryRegionKind::Boot)
            && d.phys_start <= TRAMPOLINE_PHYS
            && d.end() >= TRAMPOLINE_PHYS + PAGE_SIZE
    })
}

/// Boot tables: a copy of the kernel PML4 plus an identity map of the first
/// MiB (one 2 MiB entry). Three frames.
fn build_boot_tables(kernel_root: u64) -> Option<[Frame; 3]> {
    let frames = pmm::with(|p| {
        let a = p.allocate()?;
        let b = p.allocate()?;
        let c = p.allocate()?;
        Some([a, b, c])
    })?;
    let [pml4, pdpt, pd] = frames;
    let table = |f: Frame| phys::phys_to_virt(f.addr()) as *mut [u64; 512];
    // SAFETY: freshly allocated frames, reachable through the physical map;
    // the kernel PML4 is valid.
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
        pd_t[0] = PRESENT | WRITABLE | HUGE; // 0..2 MiB identity
    }
    Some(frames)
}

/// Copies the trampoline and writes its parameters for AP `idx`.
fn install_trampoline(pml4: u64, stack_top: u64, idx: usize) {
    let (start, end, params) = (
        core::ptr::addr_of!(smp_trampoline_start) as u64,
        core::ptr::addr_of!(smp_trampoline_end) as u64,
        core::ptr::addr_of!(smp_trampoline_params) as u64,
    );
    let len = (end - start) as usize;
    assert!(len <= PAGE_SIZE as usize, "smp: trampoline of {len} bytes");
    assert_eq!(
        (params - start) as usize,
        PARAMS_OFFSET,
        "smp: trampoline layout"
    );
    let dst = phys::phys_to_virt(TRAMPOLINE_PHYS) as *mut u8;
    // SAFETY: low page reserved for the trampoline (PMM) and checked in the
    // map; the source is readable kernel text.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, dst, len);
        let p = dst.add((params - start) as usize) as *mut u64;
        p.write_volatile(pml4);
        p.add(1).write_volatile(stack_top - 8); // as if a `call` had pushed the return address
        p.add(2)
            .write_volatile(ap_entry as *const () as usize as u64);
        p.add(3).write_volatile(idx as u64);
    }
}

/// Starts the APs listed in the MADT. Requires the scheduler to be
/// initialized (each AP adopts its own idle thread) and interrupts enabled
/// on the BSP.
pub fn start_aps(info: &BootInfo) {
    let ids: alloc::vec::Vec<u8> = crate::acpi::with(|a| a.madt.lapic_ids.clone());
    let bsp = apic::bsp_id();
    let aps: alloc::vec::Vec<u8> = ids.iter().copied().filter(|&id| id != bsp).collect();
    if aps.is_empty() {
        info!("smp: 1 CPU online (no APs in the MADT)");
        return;
    }
    if !trampoline_page_usable(info) {
        warn!(
            "smp: page {TRAMPOLINE_PHYS:#x} is not free RAM; {} AP(s) not started",
            aps.len()
        );
        return;
    }
    let kernel_root = read_cr(3);
    KERNEL_CR3.store(kernel_root, Ordering::Relaxed);
    BSP_CR0.store(read_cr(0), Ordering::Relaxed);
    BSP_CR4.store(read_cr(4), Ordering::Relaxed);
    let Some(tables) = build_boot_tables(kernel_root) else {
        warn!("smp: no frames for the boot tables");
        return;
    };

    let mut started = 0;
    for (n, &id) in aps.iter().enumerate() {
        let idx = n + 1;
        if idx >= MAX_CPUS {
            warn!("smp: more than {MAX_CPUS} CPUs; ignoring the rest");
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
            warn!("smp: CPU {idx} (lapic {id}) did not answer the SIPI");
        }
    }
    pmm::with(|p| {
        for f in tables {
            p.free(f).expect("boot table frames");
        }
    });
    info!(
        "smp: {} CPUs online ({started} of {} APs started), lapic ids {:?}",
        cpu::online_count(),
        aps.len(),
        cpu::online()
            .map(cpu::Cpu::lapic_id)
            .collect::<alloc::vec::Vec<_>>()
    );
}

/// First Rust code of an AP: its idle stack, IF = 0, boot tables.
extern "C" fn ap_entry(idx: usize) -> ! {
    let cpu = cpu::slot(idx);
    // SAFETY: values taken from the BSP; the kernel CR3 maps this code and this
    // stack just like the boot tables.
    unsafe {
        asm!("mov cr3, {}", in(reg) KERNEL_CR3.load(Ordering::Relaxed), options(nostack));
        asm!("mov cr4, {}", in(reg) BSP_CR4.load(Ordering::Relaxed), options(nostack));
        asm!("mov cr0, {}", in(reg) BSP_CR0.load(Ordering::Relaxed), options(nostack));
    }
    gdt::init_for(cpu);
    idt::load();
    syscall::init();
    cpu::activate(cpu);
    // CR0/CR4 came from the BSP; XCR0 is per CPU (ADR 0065).
    super::fpu::init_cpu();
    apic::init_local();
    scheduler::adopt_idle(idx);
    cpu.set_online();
    // SAFETY: IDT, LAPIC and per-CPU state ready.
    unsafe { crate::sync::enable_interrupts() };
    scheduler::idle_loop()
}

// --- Service IPIs ---------------------------------------------------------

/// Page to invalidate (0 = whole TLB) and CPUs yet to acknowledge.
static FLUSH_ADDR: AtomicU64 = AtomicU64::new(0);
static FLUSH_PENDING: AtomicUsize = AtomicUsize::new(0);
static FLUSH_SENDER: SpinLock<()> = SpinLock::new(());
static FLUSHES_SERVED: AtomicUsize = AtomicUsize::new(0);

fn invlpg(virt: u64) {
    // SAFETY: invalidation of a TLB entry; no memory effects.
    unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
}

/// Flushes this CPU's whole TLB, global entries included.
fn flush_all_local() {
    const PGE: u64 = 1 << 7;
    let cr4 = read_cr(4);
    // SAFETY: toggling PGE (or reloading CR3) only invalidates the TLB.
    unsafe {
        if cr4 & PGE != 0 {
            asm!("mov cr4, {}", in(reg) cr4 & !PGE, options(nostack));
            asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        } else {
            asm!("mov cr3, {}", in(reg) read_cr(3), options(nostack));
        }
    }
}

/// Invalidates `virt` (or everything with `None`) on the other online CPUs
/// and waits for their acknowledgement. The caller already invalidated its
/// own. Must be called with interrupts enabled and no locks held: the wait
/// services IPIs from other CPUs doing the same.
pub fn flush_others(virt: Option<u64>) {
    let others = cpu::online_count().saturating_sub(1);
    if others == 0 {
        return;
    }
    assert!(
        crate::sync::interrupts_enabled(),
        "smp: tlb shootdown with interrupts disabled"
    );
    let _sender = FLUSH_SENDER.lock();
    FLUSH_ADDR.store(virt.unwrap_or(0), Ordering::Relaxed);
    FLUSH_PENDING.store(others, Ordering::SeqCst);
    apic::broadcast_ipi(apic::TLB_FLUSH_VECTOR);
    let mut spins = 0u64;
    while FLUSH_PENDING.load(Ordering::Acquire) != 0 {
        spins += 1;
        assert!(spins < 1 << 30, "smp: tlb shootdown not answered");
        core::hint::spin_loop();
    }
}

/// `TLB_FLUSH_VECTOR` handler on the receiving CPU.
pub fn on_tlb_flush_ipi() {
    match FLUSH_ADDR.load(Ordering::Relaxed) {
        0 => flush_all_local(),
        v => invlpg(v),
    }
    FLUSHES_SERVED.fetch_add(1, Ordering::Relaxed);
    let _ = FLUSH_PENDING.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
}

/// Invalidation IPIs serviced by this and the other CPUs (self-test).
pub fn flushes_served() -> usize {
    FLUSHES_SERVED.load(Ordering::Relaxed)
}

/// Halts the other CPUs (panic). No waiting: the calling CPU may be in any
/// state.
pub fn halt_others() {
    if cpu::online_count() > 1 {
        apic::broadcast_ipi(apic::HALT_VECTOR);
    }
}

pub fn halt_forever() -> ! {
    loop {
        // SAFETY: permanent halt of this CPU.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}
