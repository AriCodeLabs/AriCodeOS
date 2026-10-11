//! Kernel clock: periodic APIC tick and monotonic time (Phase 8).

use core::sync::atomic::{AtomicU64, Ordering};
use log::info;

/// Scheduler tick frequency.
pub const TICK_HZ: u64 = 100;
pub const MS_PER_TICK: u64 = 1000 / TICK_HZ;

static TICKS: AtomicU64 = AtomicU64::new(0);
/// TSC at calibration (before the first tick): origin of `tsc_since_boot`.
static BOOT_TSC: AtomicU64 = AtomicU64::new(0);
/// TSC cycles per millisecond, measured against the PIT at boot; 0 until
/// then (the clock falls back to the tick).
static TSC_PER_MS: AtomicU64 = AtomicU64::new(0);

/// Called from the timer handler. Returns the tick just counted.
pub fn tick() -> u64 {
    TICKS.fetch_add(1, Ordering::Relaxed) + 1
}

/// Sets the TSC origin and frequency (done by `apic::init` on the BSP by
/// measuring against the PIT). Assumes an invariant TSC shared by all CPUs.
pub fn calibrate_tsc(per_ms: u64) {
    BOOT_TSC.store(rdtsc(), Ordering::Relaxed);
    TSC_PER_MS.store(per_ms, Ordering::Relaxed);
}

/// Microseconds since boot: from the TSC if calibrated, otherwise with tick
/// granularity.
pub fn uptime_us() -> u64 {
    match TSC_PER_MS.load(Ordering::Relaxed) {
        0 => ticks() * MS_PER_TICK * 1000,
        per_ms => tsc_since_boot() / per_ms.div_ceil(1000).max(1),
    }
}

fn rdtsc() -> u64 {
    // SAFETY: rdtsc only reads the time stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC cycles since the first tick (for per-CPU idle time, stage 5).
pub fn tsc_since_boot() -> u64 {
    rdtsc().wrapping_sub(BOOT_TSC.load(Ordering::Relaxed))
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Milliseconds since boot (see `uptime_us`).
pub fn uptime_ms() -> u64 {
    uptime_us() / 1000
}

/// Sleeps `ms`, yielding the CPU to the scheduler.
pub fn sleep_ms(ms: u64) {
    crate::task::scheduler::sleep_ms(ms);
}

/// Self-test (`test=timer`): the tick advances at the expected rate measured
/// against the PIT, `sleep_ms` wakes up and there are no spurious interrupts.
pub fn selftest() {
    assert!(
        crate::sync::interrupts_enabled(),
        "interrupts must be enabled"
    );
    let t0 = ticks();
    crate::arch::x86_64::pit::wait_ms(50);
    let elapsed = ticks() - t0;
    // 50 ms at 100 Hz is 5 ticks; edges are tolerated.
    assert!(
        (4..=7).contains(&elapsed),
        "ticks in 50 ms: {elapsed}, expected ~5"
    );

    let before = uptime_ms();
    sleep_ms(30);
    let slept = uptime_ms() - before;
    assert!((30..=60).contains(&slept), "sleep_ms(30) slept {slept} ms");

    assert_eq!(
        crate::arch::x86_64::apic::spurious_count(),
        0,
        "spurious interrupts"
    );
    info!("timer selftest ok ({elapsed} ticks in 50 ms of PIT, sleep_ms(30) -> {slept} ms)");
}
