//! Allocator físico global (Fase 5), construido sobre `aricode-pmm` a partir
//! del mapa UEFI de `BootInfo`.
//!
//! El bitmap se coloca en la región libre más baja con espacio suficiente y
//! se accede a través del mapa físico lineal.

use super::phys;
use crate::sync::SingleCpu;
use aricode_pmm::{BitmapAllocator, Frame, Stats, FRAME_SIZE};
use boot_protocol::{BootInfo, MemoryRegionKind};
use log::{debug, info};

static PMM: SingleCpu<Option<BitmapAllocator<'static>>> = SingleCpu::new(None);

/// Memoria baja que nunca se entrega: frame 0 (null) y el primer MiB, que
/// necesitarán el trampolín SMP y algunos dispositivos heredados.
const LOW_RESERVED_END: u64 = 1024 * 1024;

/// Construye el allocator. Debe llamarse una vez, con el mapa aún accesible.
///
/// # Panics
/// Si el mapa no contiene ninguna región libre capaz de alojar el bitmap.
pub fn init(info: &BootInfo) {
    // SAFETY: el mapa vive en memoria de boot accesible por el mapa físico.
    let regions = || unsafe { info.memory_regions() };

    // Extensión administrada: hasta el final de la última región no reservada.
    let phys_end = regions()
        .filter(|d| d.kind() != MemoryRegionKind::Reserved)
        .map(|d| d.end())
        .max()
        .expect("mapa de memoria vacio");
    let frames = (phys_end / FRAME_SIZE) as usize;
    let bitmap_bytes = BitmapAllocator::bytes_for(frames);
    let bitmap_frames = bitmap_bytes.div_ceil(FRAME_SIZE as usize) as u64;

    // Región libre más baja, por encima de la memoria baja reservada, que
    // pueda alojar el bitmap completo.
    let bitmap_addr = regions()
        .filter(|d| d.kind() == MemoryRegionKind::Free)
        .map(|d| (d.phys_start.max(LOW_RESERVED_END), d.end()))
        .filter(|(start, end)| end.saturating_sub(*start) >= bitmap_frames * FRAME_SIZE)
        .map(|(start, _)| start)
        .min()
        .expect("sin region libre para el bitmap del PMM");

    // SAFETY: región libre según el firmware, accesible por el mapa físico,
    // alineada a frame y de uso exclusivo del PMM a partir de ahora.
    let words: &'static mut [u64] = unsafe {
        core::slice::from_raw_parts_mut(
            phys::phys_to_virt(bitmap_addr) as *mut u64,
            BitmapAllocator::words_for(frames),
        )
    };
    let mut pmm = BitmapAllocator::new(words, frames);
    for d in regions().filter(|d| d.kind() == MemoryRegionKind::Free) {
        pmm.free_range(d.phys_start, d.end());
        debug!(
            "pmm: libre {:#012x}..{:#012x} ({} KiB)",
            d.phys_start,
            d.end(),
            d.size_bytes() / 1024
        );
    }
    pmm.reserve_range(0, LOW_RESERVED_END);
    pmm.reserve_range(bitmap_addr, bitmap_addr + bitmap_frames * FRAME_SIZE);

    let stats = pmm.stats();
    info!(
        "pmm: {} MiB libres de {} MiB direccionables, bitmap {} KiB @ {:#x}",
        stats.free_bytes() >> 20,
        stats.total_bytes() >> 20,
        bitmap_bytes.div_ceil(1024),
        bitmap_addr
    );
    // SAFETY: init único, una CPU (ver `SingleCpu`).
    unsafe { *PMM.get() = Some(pmm) };
}

/// Acceso exclusivo al allocator.
///
/// # Panics
/// Si `init` no se ha ejecutado.
pub fn with<R>(f: impl FnOnce(&mut BitmapAllocator<'static>) -> R) -> R {
    // SAFETY: una CPU sin interrupciones; no se reentra (ver `SingleCpu`).
    let pmm = unsafe { &mut *PMM.get() };
    f(pmm.as_mut().expect("PMM no inicializado"))
}

pub fn stats() -> Stats {
    with(|p| p.stats())
}

/// Autoprueba en la máquina real (`test=pmm`): unicidad, liberación y
/// contigüidad sobre el bitmap de verdad, con el mapa de QEMU.
pub fn selftest() {
    const N: usize = 2048;
    let before = stats();
    with(|p| {
        let mut frames = [Frame::from_number(0); N];
        for (i, slot) in frames.iter_mut().enumerate() {
            let f = p.allocate().expect("sin frames");
            assert!(f.addr().is_multiple_of(FRAME_SIZE), "frame {i} no alineado");
            assert!(f.addr() >= LOW_RESERVED_END, "frame {i} en memoria baja");
            assert!(!p.is_free(f), "frame {i} sigue libre tras asignarlo");
            *slot = f;
        }
        for w in frames.windows(2) {
            assert!(w[0] < w[1], "frames no crecientes: {:?} {:?}", w[0], w[1]);
        }
        assert_eq!(p.stats().free_frames, before.free_frames - N);
        for f in frames {
            p.free(f).expect("free");
        }
        assert_eq!(p.free(frames[0]), Err(aricode_pmm::PmmError::DoubleFree));
        let big = p.allocate_contiguous(64, 16).expect("contiguo");
        assert!(big.number().is_multiple_of(16));
        p.free_contiguous(big, 64).expect("free contiguo");
    });
    assert_eq!(
        stats(),
        before,
        "las estadisticas no vuelven al estado inicial"
    );
    info!("pmm selftest ok ({N} frames, contiguo 64x alineado 16)");
}
