//! Gestor de memoria virtual (Fase 6): tablas de páginas propias del kernel.
//!
//! Al activarse sustituye a las tablas del cargador:
//! - la imagen del kernel se mapea por secciones del linker script con W^X
//!   (.text RX, .rodata R, .data/.bss RW, todo NX salvo .text);
//! - el mapa físico lineal (`PHYS_MAP_BASE`) se reconstruye RW+NX;
//! - el framebuffer se cubre aunque quede fuera de la RAM;
//! - desaparece la identidad;
//! - una pila nueva con guard page sustituye a la del cargador;
//! - la memoria de arranque (`Boot`) vuelve al PMM.

use super::phys;
use super::pmm;
use crate::sync::IrqLock;
use aricode_pmm::{Frame, FRAME_SIZE};
use aricode_x86_paging::{
    FrameAlloc, MapError, Mapper, OffsetMap, GLOBAL, HUGE_2M, NO_EXECUTE, PAGE_SIZE, WRITABLE,
};
use boot_protocol::{BootInfo, MemoryRegionKind, KERNEL_VIRT_BASE};
use log::{debug, info};

/// Pila principal del kernel: 64 KiB justo debajo de la imagen, con una
/// guard page sin mapear por debajo.
pub const KERNEL_STACK_SIZE: u64 = 64 * 1024;
pub const KERNEL_STACK_TOP: u64 = KERNEL_VIRT_BASE - PAGE_SIZE;
pub const KERNEL_STACK_BOTTOM: u64 = KERNEL_STACK_TOP - KERNEL_STACK_SIZE;
pub const KERNEL_STACK_GUARD: u64 = KERNEL_STACK_BOTTOM - PAGE_SIZE;

/// Frames para tablas de páginas desde el PMM.
pub struct PmmFrames;

impl FrameAlloc for PmmFrames {
    fn alloc_frame(&mut self) -> Option<u64> {
        pmm::with(|p| p.allocate()).map(Frame::addr)
    }
}

pub type KernelMapper = Mapper<PmmFrames, OffsetMap>;

static VMM: IrqLock<Option<KernelMapper>> = IrqLock::new(None);

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __bss_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// Construye y activa las tablas propias. Devuelve la cima de la pila nueva.
///
/// # Panics
/// Si no hay frames suficientes o el layout del kernel no es el esperado.
pub fn init(info: &BootInfo) -> u64 {
    let offset = phys::offset();
    let limit = phys::limit();
    let mut m = Mapper::new(PmmFrames, OffsetMap(offset)).expect("vmm: sin frames para el PML4");
    // La mitad alta completa (256 PDPT, 1 MiB) existe desde el principio:
    // los procesos la comparten y ven todo mapeo futuro del kernel.
    m.preallocate_kernel_half()
        .expect("vmm: sin frames para la mitad alta");

    // Mapa físico lineal.
    m.map_range_2m(offset, 0, limit, WRITABLE | NO_EXECUTE | GLOBAL)
        .expect("vmm: mapa fisico");
    let fb = &info.framebuffer;
    if fb.address != 0 {
        let start = (fb.address & !(HUGE_2M - 1)).max(limit);
        let end = (fb.address + fb.size).next_multiple_of(HUGE_2M);
        if start < end {
            m.map_range_2m(
                offset + start,
                start,
                end - start,
                WRITABLE | NO_EXECUTE | GLOBAL,
            )
            .expect("vmm: framebuffer");
        }
    }

    // Imagen del kernel por secciones (W^X).
    // SAFETY: símbolos definidos por kernel.ld; solo se toma su dirección.
    let (text, rodata, data) = unsafe {
        (
            (sym(&__text_start), sym(&__text_end)),
            (sym(&__rodata_start), sym(&__rodata_end)),
            (sym(&__data_start), sym(&__bss_end)),
        )
    };
    let kernel_phys = |virt: u64| info.kernel_phys_base + (virt - KERNEL_VIRT_BASE);
    for (name, (start, end), flags) in [
        (".text", text, GLOBAL),
        (".rodata", rodata, GLOBAL | NO_EXECUTE),
        (".data/.bss", data, GLOBAL | NO_EXECUTE | WRITABLE),
    ] {
        // Los símbolos de inicio están alineados por el linker script; los de
        // fin marcan el último byte usado y la siguiente sección empieza en la
        // página siguiente, así que redondear hacia arriba no solapa nada.
        assert!(start.is_multiple_of(PAGE_SIZE), "{name} no alineada");
        let end = end.next_multiple_of(PAGE_SIZE);
        m.map_range_4k(start, kernel_phys(start), end - start, flags)
            .unwrap_or_else(|e| panic!("vmm: {name}: {e:?}"));
        debug!("vmm: {name:<11} {start:#x}..{end:#x} {}", describe(flags));
    }

    // Pila nueva con guard page: la guard simplemente no se mapea.
    for virt in (KERNEL_STACK_BOTTOM..KERNEL_STACK_TOP).step_by(PAGE_SIZE as usize) {
        let frame = pmm::with(|p| p.allocate()).expect("vmm: sin frames para la pila");
        m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL)
            .expect("vmm: pila");
    }
    assert_eq!(
        m.translate(KERNEL_STACK_GUARD),
        None,
        "la guard page no debe estar mapeada"
    );

    let root = m.root_phys();
    // SAFETY: las tablas nuevas mapean el código que se está ejecutando, la
    // pila actual (mapa físico) y todos los datos del kernel; `root` es un
    // PML4 completo construido arriba.
    unsafe { core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags)) };
    *VMM.lock() = Some(m);
    info!(
        "vmm: tablas propias activas (pml4 {root:#x}), W^X en la imagen, pila {:#x}..{:#x} con guard {:#x}",
        KERNEL_STACK_BOTTOM, KERNEL_STACK_TOP, KERNEL_STACK_GUARD
    );

    reclaim_boot_memory(info);
    KERNEL_STACK_TOP
}

/// Devuelve al PMM las regiones `Boot` del mapa UEFI (cargador, boot
/// services y las tablas de AriBoot, ya inactivas) salvo las páginas que el
/// kernel sigue usando (`BootInfo::kernel_ranges`: imagen, pila inicial,
/// `BootInfo`, initrd y cmdline), que vuelven a reservarse. El buffer del
/// propio mapa de memoria se libera también y no se vuelve a leer.
fn reclaim_boot_memory(info: &BootInfo) {
    let before = pmm::stats().free_bytes();
    // SAFETY: mapa físico activo (reconstruido arriba) y buffer intacto.
    for d in unsafe { info.memory_regions() }.filter(|d| d.kind() == MemoryRegionKind::Boot) {
        // La memoria baja sigue reservada (trampolín SMP, dispositivos heredados).
        let start = d.phys_start.max(pmm::LOW_RESERVED_END);
        if start < d.end() {
            pmm::with(|p| p.free_range(start, d.end()));
        }
    }
    let mut kept = 0u64;
    for (start, end) in info.kernel_ranges() {
        let end = end.next_multiple_of(PAGE_SIZE);
        pmm::with(|p| p.reserve_range(start, end));
        kept += end.saturating_sub(start);
    }
    let s = pmm::stats();
    info!(
        "vmm: {} MiB de memoria de arranque devueltos al PMM ({} KiB conservados para el kernel, {} MiB libres)",
        s.free_bytes().saturating_sub(before) >> 20,
        kept >> 10,
        s.free_bytes() >> 20
    );
}

fn describe(flags: u64) -> &'static str {
    match (flags & WRITABLE != 0, flags & NO_EXECUTE == 0) {
        (false, true) => "R-X",
        (false, false) => "R--",
        (true, false) => "RW-",
        (true, true) => "RWX",
    }
}

/// Acceso exclusivo a las tablas activas.
///
/// # Panics
/// Si `init` no se ha ejecutado.
pub fn with<R>(f: impl FnOnce(&mut KernelMapper) -> R) -> R {
    let mut guard = VMM.lock();
    f(guard.as_mut().expect("VMM no inicializado"))
}

/// Invalida la traducción de `virt` en esta CPU y, por IPI, en las demás
/// (Fase 32). Para páginas de la mitad del kernel que otra CPU puede haber
/// usado. Debe llamarse con interrupciones habilitadas y sin locks tomados.
pub fn flush_page(virt: u64) {
    // SAFETY: invalidación de una entrada de TLB; sin efectos en memoria.
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
    crate::arch::x86_64::smp::flush_others(Some(virt));
}

/// `true` si `addr` cae en la guard page de la pila del kernel.
pub fn in_stack_guard(addr: u64) -> bool {
    (KERNEL_STACK_GUARD..KERNEL_STACK_BOTTOM).contains(&addr)
}

/// Autoprueba (`test=vmm`): traducciones coherentes con lo que se mapeó,
/// mapeo y desmapeo de una página nueva, y errores esperados.
pub fn selftest() {
    let _probe_virt = with(|m| {
        // .text es ejecutable y no escribible; .data al revés.
        // SAFETY: solo direcciones de símbolos.
        let (text, data) = unsafe { (sym(&__text_start), sym(&__data_start)) };
        let (_, tf) = m.translate(text).expect("text mapeado");
        assert!(
            tf & NO_EXECUTE == 0 && tf & WRITABLE == 0,
            ".text debe ser R-X"
        );
        let (_, df) = m.translate(data).expect("data mapeado");
        assert!(
            df & NO_EXECUTE != 0 && df & WRITABLE != 0,
            ".data debe ser RW-"
        );
        // El mapa físico traduce por desplazamiento.
        let probe = 0x20_0000u64;
        assert_eq!(
            m.translate(phys::phys_to_virt(probe)).map(|t| t.0),
            Some(probe)
        );
        // La identidad ya no existe.
        assert_eq!(
            m.translate(probe),
            None,
            "la identidad debe haber desaparecido"
        );
        assert_eq!(m.translate(KERNEL_STACK_GUARD), None);
        // Mapear, escribir, leer y desmapear una página nueva.
        let frame = pmm::with(|p| p.allocate()).expect("frame");
        let virt = 0xFFFF_9000_0000_0000u64;
        m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE)
            .expect("map");
        // SAFETY: página recién mapeada RW, exclusiva de esta prueba.
        unsafe {
            (virt as *mut u64).write_volatile(0x5EA5_0DE5);
            assert_eq!(
                (phys::phys_to_virt(frame.addr()) as *const u64).read_volatile(),
                0x5EA5_0DE5
            );
        }
        assert_eq!(
            m.map_4k(virt, frame.addr(), 0),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(m.unmap_4k(virt), Ok(frame.addr()));
        pmm::with(|p| p.free(frame)).expect("free");
        assert_eq!(m.translate(virt), None);
        let _ = FRAME_SIZE;
        virt
    });
    // Fuera del lock del VMM: la invalidación avisa a las demás CPUs.
    flush_page(0xFFFF_9000_0000_0000u64);
    info!("vmm selftest ok (W^X, mapa fisico, sin identidad, map/unmap)");
}
