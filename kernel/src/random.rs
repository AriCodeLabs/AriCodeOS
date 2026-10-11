//! Kernel random generator (Phase 31): ChaCha20 seeded with `rdrand` when
//! the CPU has it and otherwise with TSC samples and the boot time (poor
//! entropy, and it says so). Every read mixes in another TSC sample.
//! Exposed to user space through `system_info(RANDOM)`.

use crate::sync::IrqLock;
use aricode_crypto::chacha20::Rng;
use aricode_crypto::sha256::Sha256;
use log::info;

static RNG: IrqLock<Option<Rng>> = IrqLock::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Rdrand,
    Tsc,
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

/// 32-byte seed: hash of several samples from the best available source.
fn seed() -> ([u8; 32], Source) {
    let mut h = Sha256::new();
    let source = if has_rdrand() {
        for _ in 0..8 {
            if let Some(v) = rdrand() {
                h.update(&v.to_le_bytes());
            }
        }
        Source::Rdrand
    } else {
        Source::Tsc
    };
    // The TSC is always mixed in: jitter between samples and the boot instant.
    for _ in 0..64 {
        h.update(&rdtsc().to_le_bytes());
    }
    h.update(&crate::time::ticks().to_le_bytes());
    (h.finish(), source)
}

pub fn init() -> Source {
    let (s, source) = seed();
    *RNG.lock() = Some(Rng::new(s));
    info!(
        "random: ChaCha20 generator seeded with {}",
        match source {
            Source::Rdrand => "rdrand",
            Source::Tsc => "TSC (no rdrand: limited entropy)",
        }
    );
    source
}

/// Fills `out` with random bytes; mixes in a fresh sample first.
pub fn fill(out: &mut [u8]) {
    let mut guard = RNG.lock();
    let rng = guard.get_or_insert_with(|| Rng::new(seed().0));
    let mut fresh = [0u8; 32];
    fresh[..8].copy_from_slice(&rdtsc().to_le_bytes());
    if has_rdrand() {
        if let Some(v) = rdrand() {
            fresh[8..16].copy_from_slice(&v.to_le_bytes());
        }
    }
    rng.reseed(&fresh);
    rng.fill(out);
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
    let mut hex = [0u8; 64];
    crate::println!(
        "random: source {} ok, sample {}",
        if source == Source::Rdrand {
            "rdrand"
        } else {
            "tsc"
        },
        aricode_crypto::hex(&a[..8], &mut hex)
    );
}
