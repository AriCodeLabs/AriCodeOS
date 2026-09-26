//! Local APIC: habilitación, vector espurio, EOI y timer periódico calibrado
//! con el PIT (Fase 8).

use super::msr;
use crate::memory::vmm;
use crate::time;
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
use core::sync::atomic::{AtomicU32, Ordering};
use log::{info, warn};

/// Dirección virtual donde se mapea el LAPIC (una página, sin caché).
const LAPIC_VIRT: u64 = 0xFFFF_D000_0000_0000;

pub const TIMER_VECTOR: u8 = 0x20;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

// Registros (desplazamientos sobre la base).
const REG_ID: u32 = 0x020;
const REG_VERSION: u32 = 0x030;
const REG_TPR: u32 = 0x080;
const REG_EOI: u32 = 0x0B0;
const REG_SVR: u32 = 0x0F0;
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INIT: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const DIVIDE_BY_16: u32 = 0b0011;

static SPURIOUS: AtomicU32 = AtomicU32::new(0);

fn read(reg: u32) -> u32 {
    // SAFETY: registro MMIO del LAPIC dentro de la página mapeada en `init`.
    unsafe { ((LAPIC_VIRT + reg as u64) as *const u32).read_volatile() }
}

fn write(reg: u32, value: u32) {
    // SAFETY: ídem `read`.
    unsafe { ((LAPIC_VIRT + reg as u64) as *mut u32).write_volatile(value) }
}

/// Mapea el LAPIC, lo habilita y arranca el timer periódico a `time::TICK_HZ`.
/// Debe llamarse con las interrupciones deshabilitadas y la IDT instalada.
pub fn init() {
    let base_msr = msr::read(msr::IA32_APIC_BASE);
    let phys = base_msr & 0x000F_FFFF_FFFF_F000;
    // Bit 11: habilitación global del APIC (en xAPIC).
    // SAFETY: habilitar el APIC global no cambia su dirección base.
    unsafe { msr::write(msr::IA32_APIC_BASE, base_msr | (1 << 11)) };

    vmm::with(|m| {
        m.map_4k(
            LAPIC_VIRT,
            phys,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .expect("apic: mapear LAPIC");

    write(REG_TPR, 0);
    write(REG_SVR, SVR_ENABLE | SPURIOUS_VECTOR as u32);
    write(REG_LVT_TIMER, LVT_MASKED);

    let ticks_per_ms = calibrate_timer();
    let initial = (ticks_per_ms * 1000 / time::TICK_HZ) as u32;
    write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    write(REG_LVT_TIMER, TIMER_VECTOR as u32 | LVT_PERIODIC);
    write(REG_TIMER_INIT, initial);

    info!(
        "apic: lapic id {} version {:#x} @ {phys:#x}; timer {} ticks/ms (div 16), tick {} Hz (cuenta {initial})",
        read(REG_ID) >> 24,
        read(REG_VERSION) & 0xFF,
        ticks_per_ms,
        time::TICK_HZ
    );
}

/// Cuenta cuántos ticks del timer (divisor 16) caben en 10 ms de PIT.
fn calibrate_timer() -> u64 {
    const SAMPLE_MS: u64 = 10;
    write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED); // cuenta única, sin interrupción
    write(REG_TIMER_INIT, u32::MAX);
    super::pit::wait_ms(SAMPLE_MS);
    let elapsed = u32::MAX - read(REG_TIMER_CURRENT);
    write(REG_TIMER_INIT, 0); // parar
    let per_ms = elapsed as u64 / SAMPLE_MS;
    assert!(per_ms > 0, "apic: el timer no avanza");
    per_ms
}

pub fn spurious_count() -> u32 {
    SPURIOUS.load(Ordering::Relaxed)
}

fn eoi() {
    write(REG_EOI, 0);
}

/// Entrada para vectores >= 32 desde `exceptions::handle`.
pub fn handle_interrupt(vector: u8) {
    match vector {
        TIMER_VECTOR => {
            let now = time::tick();
            eoi();
            crate::task::scheduler::on_tick(now);
        }
        SPURIOUS_VECTOR => {
            // Sin EOI por especificación.
            SPURIOUS.fetch_add(1, Ordering::Relaxed);
        }
        v => {
            warn!("interrupcion inesperada, vector {v}");
            eoi();
        }
    }
}
