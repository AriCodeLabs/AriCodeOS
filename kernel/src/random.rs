// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel random generator (Phase 31): ChaCha20 seeded with `rdrand` when
//! the CPU has it and otherwise with TSC samples and the boot time (poor
//! entropy, and it says so). Every read mixes in another TSC sample and
//! the interrupt-timing pool: every device, keyboard and serial interrupt
//! stirs the cycle counter into it (`stir`), which is the only
//! unpredictable input on CPUs without `rdrand`. Exposed to user space
//! through `system_info(RANDOM)`; what seeded it and how much has been
//! mixed in since, through `system_info(RANDOM_SOURCE)` (ADR 0039), so
//! the services that draw secrets can tell a weak generator from a strong
//! one. That reply holds only the source and counters, never state.

use crate::sync::IrqLock;
use aricode_abi::sysinfo::{EntropySource, RandomSource};
use aricode_crypto::chacha20::Rng;
use aricode_crypto::sha256::Sha256;
use core::sync::atomic::{AtomicU64, Ordering};
use log::info;

/// The generator and the account of what went into it, under one lock so
/// the account always matches the state.
struct Generator {
    rng: Rng,
    mixed: RandomSource,
}

static RNG: IrqLock<Option<Generator>> = IrqLock::new(None);

/// `rdrand` values drawn for the seed.
const SEED_RDRAND: usize = 8;
/// Cycle-counter samples hashed into the seed.
const SEED_TSC: u64 = 64;

/// Interrupt-timing pool: cycle counts XORed in by `stir`, lock-free.
static POOL: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// Number of stirs (also selects the pool word).
static STIRS: AtomicU64 = AtomicU64::new(0);

/// Mixes the current cycle count into the pool. Called from interrupt
/// handlers: one `rdtsc` and two relaxed atomics, no lock.
pub fn stir() {
    let n = STIRS.fetch_add(1, Ordering::Relaxed);
    let t = rdtsc().rotate_left((n % 61) as u32);
    POOL[(n % POOL.len() as u64) as usize].fetch_xor(t, Ordering::Relaxed);
}

/// Stirs so far (diagnostics).
pub fn stirs() -> u64 {
    STIRS.load(Ordering::Relaxed)
}

fn rdtsc() -> u64 {
    // SAFETY: rdtsc has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// `rdrand` is available (CPUID.1:ECX bit 30).
fn has_rdrand() -> bool {
    let r = core::arch::x86_64::__cpuid(1);
    r.ecx & (1 << 30) != 0
}

/// A value from `rdrand`, or `None` if the instruction returns no data.
fn rdrand() -> Option<u64> {
    let v: u64;
    let ok: u8;
    // SAFETY: rdrand only writes the destination register and CF; the caller
    // already checked that the CPU supports it.
    unsafe {
        core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack));
    }
    (ok != 0).then_some(v)
}

/// 32-byte seed: hash of several samples from the best available source,
/// and the account of it. The source is `Rdrand` only if `rdrand` actually
/// returned data: a CPU that advertises it and never delivers is seeded
/// from the TSC alone and reported as such.
fn seed() -> ([u8; 32], RandomSource) {
    let mut h = Sha256::new();
    let mut hardware = 0;
    if has_rdrand() {
        for _ in 0..SEED_RDRAND {
            if let Some(v) = rdrand() {
                h.update(&v.to_le_bytes());
                hardware += 1;
            }
        }
    }
    // The TSC is always mixed in: jitter between samples and the boot instant.
    for _ in 0..SEED_TSC {
        h.update(&rdtsc().to_le_bytes());
    }
    h.update(&crate::time::ticks().to_le_bytes());
    let mixed = RandomSource {
        source: if hardware > 0 {
            EntropySource::Rdrand
        } else {
            EntropySource::TscInterrupts
        },
        hardware_samples: hardware,
        tsc_samples: SEED_TSC,
        interrupt_events: 0,
    };
    (h.finish(), mixed)
}

fn seeded() -> Generator {
    let (s, mixed) = seed();
    Generator {
        rng: Rng::new(s),
        mixed,
    }
}

pub fn init() -> EntropySource {
    let generator = seeded();
    let source = generator.mixed.source;
    *RNG.lock() = Some(generator);
    info!(
        "random: ChaCha20 generator seeded with {}",
        match source {
            EntropySource::Rdrand => "rdrand",
            EntropySource::TscInterrupts if has_rdrand() =>
                "TSC (rdrand returned no data: limited entropy)",
            EntropySource::TscInterrupts => "TSC (no rdrand: limited entropy)",
        }
    );
    source
}

/// What seeded the generator and what has been mixed into it since
/// (`system_info(RANDOM_SOURCE)`). Seeds it first if nothing did yet, as
/// `fill` does.
pub fn source() -> RandomSource {
    RNG.lock().get_or_insert_with(seeded).mixed
}

/// Fills `out` with random bytes; mixes in a fresh sample first.
pub fn fill(out: &mut [u8]) {
    let mut guard = RNG.lock();
    let g = guard.get_or_insert_with(seeded);
    // Fresh material: cycle counter, `rdrand` if any, and the interrupt pool.
    let mut h = Sha256::new();
    h.update(&rdtsc().to_le_bytes());
    g.mixed.tsc_samples += 1;
    if has_rdrand() {
        if let Some(v) = rdrand() {
            h.update(&v.to_le_bytes());
            g.mixed.hardware_samples += 1;
        }
    }
    let stirs = STIRS.load(Ordering::Relaxed);
    h.update(&stirs.to_le_bytes());
    for w in &POOL {
        h.update(&w.load(Ordering::Relaxed).to_le_bytes());
    }
    g.mixed.interrupt_events = stirs;
    g.rng.reseed(&h.finish());
    g.rng.fill(out);
}

/// Self-test (`test=random`): two different, non-zero reads.
pub fn selftest() {
    let source = init();
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    fill(&mut a);
    fill(&mut b);
    assert!(a.iter().any(|&x| x != 0), "random: all-zero block");
    assert_ne!(a, b, "random: two identical reads");
    // The interrupt pool changes the output.
    let before = STIRS.load(Ordering::Relaxed);
    stir();
    let mut c = [0u8; 32];
    fill(&mut c);
    assert_ne!(b, c, "random: stir did not change the output");
    assert_eq!(stirs(), before + 1, "random: stir count");
    // The account: the seed's samples plus one TSC sample per read, the
    // stirs counted up to the last read, and the source `init` announced.
    let mixed = self::source();
    assert_eq!(mixed.source, source, "random: source account");
    assert_eq!(mixed.tsc_samples, SEED_TSC + 3, "random: TSC sample count");
    assert_eq!(mixed.interrupt_events, before + 1, "random: event count");
    assert_eq!(
        mixed.hardware_samples > 0,
        source.is_hardware(),
        "random: hardware sample count"
    );
    let mut hex = [0u8; 64];
    crate::println!(
        "random: source {} ok, {} interrupt stirs, {} TSC samples, sample {}",
        if source.is_hardware() {
            "rdrand"
        } else {
            "tsc"
        },
        stirs(),
        mixed.tsc_samples,
        aricode_crypto::hex(&a[..8], &mut hex)
    );
}
