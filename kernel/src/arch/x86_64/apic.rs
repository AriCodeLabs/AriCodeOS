// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Local APIC: enabling, spurious vector, EOI, periodic timer calibrated
//! with the PIT (Phase 8) and inter-processor interrupts (Phase 32).
//!
//! Every CPU's LAPIC is at the same virtual address: each one sees its own.
//! The BSP calibrates the timer once and the APs reuse the count.

use super::msr;
use crate::memory::vmm;
use crate::sync::KernelIrq;
use crate::time;
use aricode_sync::IrqPolicy;
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
use core::sync::atomic::{AtomicU32, Ordering};
use log::{info, warn};

/// Virtual address where the LAPIC is mapped (one page, uncached).
const LAPIC_VIRT: u64 = 0xFFFF_D000_0000_0000;

pub const TIMER_VECTOR: u8 = 0x20;
/// IPI: there is work for an idle CPU (only takes it out of `hlt`).
pub const RESCHED_VECTOR: u8 = 0x21;
/// IPI: invalidate the TLB (see `smp::flush_others`).
pub const TLB_FLUSH_VECTOR: u8 = 0x22;
/// IPI: halt the CPU forever (panic).
pub const HALT_VECTOR: u8 = 0x23;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

// Registers (offsets from the base).
const REG_ID: u32 = 0x020;
const REG_VERSION: u32 = 0x030;
const REG_TPR: u32 = 0x080;
const REG_EOI: u32 = 0x0B0;
const REG_SVR: u32 = 0x0F0;
const REG_ICR_LOW: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INIT: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const DIVIDE_BY_16: u32 = 0b0011;

const ICR_DELIVERY_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_STARTUP: u32 = 0b110 << 8;
const ICR_PENDING: u32 = 1 << 12;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_ALL_EXCLUDING_SELF: u32 = 0b11 << 18;

static SPURIOUS: AtomicU32 = AtomicU32::new(0);
/// Initial timer count for `TICK_HZ`, calibrated by the BSP.
static TIMER_INITIAL: AtomicU32 = AtomicU32::new(0);
static BSP_ID: AtomicU32 = AtomicU32::new(0);

fn read(reg: u32) -> u32 {
    // SAFETY: LAPIC MMIO register within the page mapped in `init`.
    unsafe { ((LAPIC_VIRT + reg as u64) as *const u32).read_volatile() }
}

fn write(reg: u32, value: u32) {
    // SAFETY: same as `read`.
    unsafe { ((LAPIC_VIRT + reg as u64) as *mut u32).write_volatile(value) }
}

/// Enables the calling CPU's LAPIC (global MSR bit, TPR and SVR).
fn enable_local() {
    let base_msr = msr::read(msr::IA32_APIC_BASE);
    // Bit 11: global APIC enable (in xAPIC). The MSR is per CPU.
    // SAFETY: enabling the global APIC does not change its base address.
    unsafe { msr::write(msr::IA32_APIC_BASE, base_msr | (1 << 11)) };
    write(REG_TPR, 0);
    write(REG_SVR, SVR_ENABLE | SPURIOUS_VECTOR as u32);
    write(REG_LVT_TIMER, LVT_MASKED);
}

fn start_timer(initial: u32) {
    write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    write(REG_LVT_TIMER, TIMER_VECTOR as u32 | LVT_PERIODIC);
    write(REG_TIMER_INIT, initial);
}

/// Maps the LAPIC, enables it and starts the periodic timer at
/// `time::TICK_HZ`. Must be called on the BSP with interrupts disabled and
/// the IDT installed.
pub fn init() {
    let phys = msr::read(msr::IA32_APIC_BASE) & 0x000F_FFFF_FFFF_F000;
    vmm::with(|m| {
        m.map_4k(
            LAPIC_VIRT,
            phys,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .expect("apic: map LAPIC");

    enable_local();
    // The TSC is measured against the same PIT as the timer: microsecond clock (stage 5).
    // SAFETY: rdtsc only reads the time stamp counter.
    let t0 = unsafe { core::arch::x86_64::_rdtsc() };
    super::pit::wait_ms(10);
    // SAFETY: same as above.
    let t1 = unsafe { core::arch::x86_64::_rdtsc() };
    time::calibrate_tsc(t1.wrapping_sub(t0) / 10);
    let ticks_per_ms = calibrate_timer();
    let initial = (ticks_per_ms * 1000 / time::TICK_HZ) as u32;
    TIMER_INITIAL.store(initial, Ordering::Relaxed);
    start_timer(initial);
    BSP_ID.store(id(), Ordering::Relaxed);
    crate::cpu::current().set_lapic_id(id());

    info!(
        "apic: lapic id {} version {:#x} @ {phys:#x}; timer {} ticks/ms (div 16), tick {} Hz (count {initial})",
        id(),
        read(REG_VERSION) & 0xFF,
        ticks_per_ms,
        time::TICK_HZ
    );
}

/// Enables an AP's LAPIC and starts its timer with the BSP's count.
pub fn init_local() {
    enable_local();
    start_timer(TIMER_INITIAL.load(Ordering::Relaxed));
}

/// Counts how many timer ticks (divisor 16) fit in 10 ms of PIT.
fn calibrate_timer() -> u64 {
    const SAMPLE_MS: u64 = 10;
    write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED); // one-shot count, no interrupt
    write(REG_TIMER_INIT, u32::MAX);
    super::pit::wait_ms(SAMPLE_MS);
    let elapsed = u32::MAX - read(REG_TIMER_CURRENT);
    write(REG_TIMER_INIT, 0); // stop
    let per_ms = elapsed as u64 / SAMPLE_MS;
    assert!(per_ms > 0, "apic: the timer does not advance");
    per_ms
}

/// LAPIC identifier of the calling CPU.
pub fn id() -> u32 {
    read(REG_ID) >> 24
}

/// LAPIC identifier of the BSP: destination of device IRQs.
pub fn bsp_id() -> u8 {
    BSP_ID.load(Ordering::Relaxed) as u8
}

pub fn spurious_count() -> u32 {
    SPURIOUS.load(Ordering::Relaxed)
}

fn eoi() {
    write(REG_EOI, 0);
}

fn wait_icr_idle() {
    let mut spins = 0u32;
    while read(REG_ICR_LOW) & ICR_PENDING != 0 {
        spins += 1;
        if spins == 1 << 24 {
            warn!("apic: ICR busy for too long");
            return;
        }
        core::hint::spin_loop();
    }
}

/// Writes the ICR (destination and command) without letting an interrupt on
/// this same CPU slip in between the two halves.
fn icr_send(high: u32, low: u32) {
    let irq = KernelIrq::save_and_disable();
    wait_icr_idle();
    write(REG_ICR_HIGH, high);
    write(REG_ICR_LOW, low);
    wait_icr_idle();
    KernelIrq::restore(irq);
}

/// Fixed IPI with `vector` to the CPU with LAPIC `lapic_id`.
pub fn send_ipi(lapic_id: u32, vector: u8) {
    icr_send(lapic_id << 24, vector as u32);
}

/// Fixed IPI with `vector` to all other CPUs.
pub fn broadcast_ipi(vector: u8) {
    icr_send(0, ICR_ALL_EXCLUDING_SELF | vector as u32);
}

/// INIT to an AP: leaves it waiting for a SIPI.
pub fn send_init(lapic_id: u32) {
    icr_send(lapic_id << 24, ICR_DELIVERY_INIT | ICR_LEVEL_ASSERT);
}

/// SIPI: the AP starts in real mode at page `vector << 12`.
pub fn send_startup(lapic_id: u32, vector: u8) {
    icr_send(lapic_id << 24, ICR_DELIVERY_STARTUP | vector as u32);
}

/// Entry for vectors >= 32 from `exceptions::handle`.
pub fn handle_interrupt(vector: u8, from_user: bool) {
    match vector {
        TIMER_VECTOR => {
            // Only the BSP keeps the clock; the APs only consume quantum.
            let bsp = crate::cpu::is_bsp();
            let now = if bsp { time::tick() } else { time::ticks() };
            eoi();
            crate::task::scheduler::on_tick(now, bsp, from_user);
        }
        RESCHED_VECTOR => {
            // Only need to leave `hlt`: the idle loop checks the queue.
            eoi();
        }
        TLB_FLUSH_VECTOR => {
            super::smp::on_tlb_flush_ipi();
            eoi();
        }
        HALT_VECTOR => {
            eoi();
            super::smp::halt_forever();
        }
        crate::iommu::FAULT_VECTOR => {
            crate::iommu::on_fault_irq();
            eoi();
        }
        SPURIOUS_VECTOR => {
            // No EOI, per the specification.
            SPURIOUS.fetch_add(1, Ordering::Relaxed);
        }
        crate::hw::DEVICE_VECTOR_FIRST..=crate::hw::DEVICE_VECTOR_LAST => {
            crate::hw::handle_device_irq(vector);
            eoi();
        }
        crate::console_input::SERIAL_VECTOR => {
            crate::console_input::on_irq();
            eoi();
        }
        crate::console_input::KEYBOARD_VECTOR => {
            crate::console_input::on_keyboard_irq();
            eoi();
        }
        v => {
            warn!("unexpected interrupt, vector {v}");
            eoi();
        }
    }
}
