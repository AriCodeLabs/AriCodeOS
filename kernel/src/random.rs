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
/// Tries per `rdrand` value: CF = 0 means the DRNG had nothing ready, and
/// Intel's DRNG software guide recommends retrying up to 10 times.
const RDRAND_RETRIES: usize = 10;
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

/// `rdrand` with the recommended retries.
fn rdrand_retry() -> Option<u64> {
    (0..RDRAND_RETRIES).find_map(|_| rdrand())
}

/// What the seed made of `rdrand` (for the boot line).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RdrandSeed {
    /// CPUID does not advertise it.
    Absent,
    /// Advertised, but no value came back.
    NoData,
    /// Values came back but look broken: fewer than two distinct values
    /// other than all-zero and all-one bits (the AMD parts that return
    /// all ones with CF set after resume; Linux `x86_init_rdrand` rejects
    /// the same way). They are still hashed in, but not counted.
    Rejected,
    /// Accepted.
    Used,
}

/// Values worth counting as hardware entropy: not all-zero or all-one
/// bits, and at least two distinct ones; otherwise none.
fn plausible_rdrand(values: &[u64]) -> usize {
    let mut good = values.iter().filter(|&&v| v != 0 && v != u64::MAX);
    let Some(&first) = good.clone().next() else {
        return 0;
    };
    if good.clone().all(|&v| v == first) {
        return 0;
    }
    good.by_ref().count()
}

/// 32-byte seed: hash of several samples from the best available source,
/// and the account of it. The source is `Rdrand` only if `rdrand` returned
/// plausible data (`plausible_rdrand`): a CPU that advertises it and never
/// delivers, or delivers a constant, is seeded from the TSC alone as far
/// as the account goes, and reported as such.
fn seed() -> ([u8; 32], RandomSource, RdrandSeed) {
    let mut h = Sha256::new();
    let mut values = [0u64; SEED_RDRAND];
    let mut got = 0;
    if has_rdrand() {
        for _ in 0..SEED_RDRAND {
            if let Some(v) = rdrand_retry() {
                h.update(&v.to_le_bytes());
                values[got] = v;
                got += 1;
            }
        }
    }
    let hardware = plausible_rdrand(&values[..got]);
    let verdict = match (has_rdrand(), got, hardware) {
        (false, _, _) => RdrandSeed::Absent,
        (true, 0, _) => RdrandSeed::NoData,
        (true, _, 0) => RdrandSeed::Rejected,
        _ => RdrandSeed::Used,
    };
    // The TSC is always mixed in: jitter between samples and the boot instant.
    for _ in 0..SEED_TSC {
        h.update(&rdtsc().to_le_bytes());
    }
    h.update(&crate::time::ticks().to_le_bytes());
    let mixed = RandomSource {
        source: if verdict == RdrandSeed::Used {
            EntropySource::Rdrand
        } else {
            EntropySource::TscInterrupts
        },
        hardware_samples: hardware as u64,
        tsc_samples: SEED_TSC,
        interrupt_events: 0,
    };
    (h.finish(), mixed, verdict)
}

fn seeded() -> (Generator, RdrandSeed) {
    let (s, mixed, verdict) = seed();
    let generator = Generator {
        rng: Rng::new(s),
        mixed,
    };
    (generator, verdict)
}

pub fn init() -> EntropySource {
    let (generator, verdict) = seeded();
    let source = generator.mixed.source;
    *RNG.lock() = Some(generator);
    info!(
        "random: ChaCha20 generator seeded with {}",
        match verdict {
            RdrandSeed::Used => "rdrand",
            RdrandSeed::Absent => "TSC (no rdrand: limited entropy)",
            RdrandSeed::NoData => "TSC (rdrand returned no data: limited entropy)",
            RdrandSeed::Rejected =>
                "TSC (rdrand values rejected: constant or all-0/all-1 bits; limited entropy)",
        }
    );
    source
}

/// What seeded the generator and what has been mixed into it since
/// (`system_info(RANDOM_SOURCE)`). Seeds it first if nothing did yet, as
/// `fill` does.
pub fn source() -> RandomSource {
    RNG.lock().get_or_insert_with(|| seeded().0).mixed
}

/// Fills `out` with random bytes; mixes in a fresh sample first.
pub fn fill(out: &mut [u8]) {
    let mut guard = RNG.lock();
    let g = guard.get_or_insert_with(|| seeded().0);
    // Fresh material: cycle counter, `rdrand` if the seed trusted it, and
    // the interrupt pool.
    let mut h = Sha256::new();
    h.update(&rdtsc().to_le_bytes());
    g.mixed.tsc_samples += 1;
    if g.mixed.source.is_hardware() {
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
    // Implausible `rdrand` output is not counted as hardware entropy.
    assert_eq!(plausible_rdrand(&[]), 0);
    assert_eq!(plausible_rdrand(&[u64::MAX; 8]), 0, "random: all ones");
    assert_eq!(
        plausible_rdrand(&[0, u64::MAX, 0, 7, 7]),
        0,
        "random: constant"
    );
    assert_eq!(plausible_rdrand(&[0, 5, 9, u64::MAX]), 2);
    // The account: the seed's samples plus one TSC sample per read, the
    // stirs counted up to the last read, and the source `init` announced.
    // Like the stir count above, the event count assumes no device
    // interrupt stirred between `stir()` and the last read (none is
    // expected this early in a `test=random` boot).
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
