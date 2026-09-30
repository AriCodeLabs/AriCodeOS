//! Reloj del kernel: tick periódico del APIC y tiempo monótono (Fase 8).

use core::sync::atomic::{AtomicU64, Ordering};
use log::info;

/// Frecuencia del tick del scheduler.
pub const TICK_HZ: u64 = 100;
pub const MS_PER_TICK: u64 = 1000 / TICK_HZ;

static TICKS: AtomicU64 = AtomicU64::new(0);
/// TSC al calibrar (antes del primer tick): origen de `tsc_since_boot`.
static BOOT_TSC: AtomicU64 = AtomicU64::new(0);
/// Ciclos de TSC por milisegundo, medidos contra el PIT en el arranque; 0
/// hasta entonces (el reloj cae al tick).
static TSC_PER_MS: AtomicU64 = AtomicU64::new(0);

/// Llamado desde el handler del timer. Devuelve el tick recién contado.
pub fn tick() -> u64 {
    TICKS.fetch_add(1, Ordering::Relaxed) + 1
}

/// Fija el origen y la frecuencia del TSC (lo hace `apic::init` en el BSP
/// midiendo contra el PIT). Supone un TSC invariante y común a las CPUs.
pub fn calibrate_tsc(per_ms: u64) {
    BOOT_TSC.store(rdtsc(), Ordering::Relaxed);
    TSC_PER_MS.store(per_ms, Ordering::Relaxed);
}

/// Microsegundos desde el arranque: por el TSC si está calibrado, si no
/// con la granularidad del tick.
pub fn uptime_us() -> u64 {
    match TSC_PER_MS.load(Ordering::Relaxed) {
        0 => ticks() * MS_PER_TICK * 1000,
        per_ms => tsc_since_boot() / per_ms.div_ceil(1000).max(1),
    }
}

fn rdtsc() -> u64 {
    // SAFETY: rdtsc solo lee el contador de tiempo.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Ciclos de TSC desde el primer tick (para el reposo por CPU, etapa 5).
pub fn tsc_since_boot() -> u64 {
    rdtsc().wrapping_sub(BOOT_TSC.load(Ordering::Relaxed))
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Milisegundos desde el arranque (ver `uptime_us`).
pub fn uptime_ms() -> u64 {
    uptime_us() / 1000
}

/// Duerme `ms` cediendo la CPU al planificador.
pub fn sleep_ms(ms: u64) {
    crate::task::scheduler::sleep_ms(ms);
}

/// Autoprueba (`test=timer`): el tick avanza al ritmo esperado medido contra
/// el PIT, `sleep_ms` despierta y no hay interrupciones espurias.
pub fn selftest() {
    assert!(
        crate::sync::interrupts_enabled(),
        "las interrupciones deben estar habilitadas"
    );
    let t0 = ticks();
    crate::arch::x86_64::pit::wait_ms(50);
    let elapsed = ticks() - t0;
    // 50 ms a 100 Hz son 5 ticks; se toleran los bordes.
    assert!(
        (4..=7).contains(&elapsed),
        "ticks en 50 ms: {elapsed}, esperados ~5"
    );

    let before = uptime_ms();
    sleep_ms(30);
    let slept = uptime_ms() - before;
    assert!((30..=60).contains(&slept), "sleep_ms(30) durmio {slept} ms");

    assert_eq!(
        crate::arch::x86_64::apic::spurious_count(),
        0,
        "interrupciones espurias"
    );
    info!("timer selftest ok ({elapsed} ticks en 50 ms de PIT, sleep_ms(30) -> {slept} ms)");
}
