//! Generador aleatorio del kernel (Fase 31): ChaCha20 sembrado con `rdrand`
//! cuando la CPU lo tiene y, si no, con muestras del TSC y del tiempo de
//! arranque (entropía pobre, y así se anuncia). Cada lectura mezcla otra
//! muestra de TSC. Se expone a usuario por `system_info(RANDOM)`.

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
    // SAFETY: rdtsc no tiene efectos secundarios.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// `rdrand` está disponible (CPUID.1:ECX bit 30).
fn has_rdrand() -> bool {
    let r = core::arch::x86_64::__cpuid(1);
    r.ecx & (1 << 30) != 0
}

/// Un valor de `rdrand`, o `None` si la instrucción no devuelve datos.
fn rdrand() -> Option<u64> {
    let v: u64;
    let ok: u8;
    // SAFETY: rdrand solo escribe el registro destino y CF; el llamador ya
    // comprobó que la CPU lo soporta.
    unsafe {
        core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack));
    }
    (ok != 0).then_some(v)
}

/// Semilla de 32 bytes: hash de varias muestras de la mejor fuente disponible.
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
    // Siempre se mezcla el TSC: jitter entre muestras y el instante de arranque.
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
        "random: generador ChaCha20 sembrado con {}",
        match source {
            Source::Rdrand => "rdrand",
            Source::Tsc => "TSC (sin rdrand: entropia limitada)",
        }
    );
    source
}

/// Rellena `out` con bytes aleatorios; mezcla una muestra fresca antes.
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

/// Autoprueba (`test=random`): dos lecturas distintas y no nulas.
pub fn selftest() {
    let source = init();
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    fill(&mut a);
    fill(&mut b);
    assert!(a.iter().any(|&x| x != 0), "random: bloque nulo");
    assert_ne!(a, b, "random: dos lecturas iguales");
    let mut hex = [0u8; 64];
    crate::println!(
        "random: fuente {} ok, muestra {}",
        if source == Source::Rdrand {
            "rdrand"
        } else {
            "tsc"
        },
        aricode_crypto::hex(&a[..8], &mut hex)
    );
}
