//! Objetos de hardware para drivers en espacio de usuario (Fase 16): líneas
//! de interrupción, regiones MMIO y buffers DMA. Un driver solo recibe lo
//! que su capability le da; el kernel no le presta nada más (sección 27).

use crate::arch::x86_64::{apic, ioapic};
use crate::memory::pmm;
use crate::object::{KernelObject, ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use alloc::collections::BTreeMap;
use alloc::sync::Weak;
use aricode_pmm::{Frame, FRAME_SIZE};
use core::sync::atomic::{AtomicU32, Ordering};
use log::warn;

/// Vectores reservados a dispositivos.
pub const DEVICE_VECTOR_FIRST: u8 = 0x30;
pub const DEVICE_VECTOR_LAST: u8 = 0x3F;

/// Origen de una línea: una GSI del I/O APIC o una entrada MSI-X (tabla
/// mapeada por el kernel, índice de entrada).
pub enum IrqSource {
    Gsi { gsi: u32, level: bool },
    Msix { table_virt: u64, entry: u32 },
}

/// Una línea de interrupción enrutada a un vector. Al soltarse el último
/// handle se enmascara y libera el vector.
pub struct IrqLine {
    pub source: IrqSource,
    pub vector: u8,
    pending: AtomicU32,
    waiters: WaitQueue,
}

impl IrqLine {
    /// Llamado desde el handler: acumula y despierta. Las líneas por nivel se
    /// enmascaran hasta que el driver las atienda (`irq_wait`).
    pub fn fire(&self) {
        if let IrqSource::Gsi { gsi, level: true } = self.source {
            ioapic::set_masked(gsi, true);
        }
        self.pending.fetch_add(1, Ordering::Release);
        self.waiters.wake_all();
    }

    /// GSI si es una línea del I/O APIC (para `object_id`).
    pub fn gsi(&self) -> u32 {
        match self.source {
            IrqSource::Gsi { gsi, .. } => gsi,
            IrqSource::Msix { entry, .. } => entry,
        }
    }

    /// Espera hasta que haya interrupciones acumuladas y las devuelve.
    pub fn wait(&self, deadline: Option<u64>) -> Option<u32> {
        let timed_out = self
            .waiters
            .wait_until_deadline(|| self.pending.load(Ordering::Acquire) > 0, deadline);
        if timed_out {
            return None;
        }
        let n = self.pending.swap(0, Ordering::AcqRel);
        if let IrqSource::Gsi { gsi, level: true } = self.source {
            ioapic::set_masked(gsi, false);
        }
        Some(n)
    }
}

impl Drop for IrqLine {
    fn drop(&mut self) {
        match self.source {
            IrqSource::Gsi { gsi, .. } => ioapic::set_masked(gsi, true),
            IrqSource::Msix { table_virt, entry } => {
                msix_write(table_virt, entry, 0, 0, MSIX_MASKED)
            }
        }
        VECTORS.lock().remove(&self.vector);
    }
}

const MSIX_MASKED: u32 = 1;

/// Escribe una entrada de la tabla MSI-X (16 bytes: addr lo, addr hi, data, control).
fn msix_write(table_virt: u64, entry: u32, addr: u64, data: u32, control: u32) {
    let base = (table_virt + 16 * entry as u64) as *mut u32;
    // SAFETY: la tabla se mapeó sin caché en `map_uc` y `entry` es una entrada
    // válida del dispositivo reclamado.
    unsafe {
        base.add(3).write_volatile(MSIX_MASKED); // enmascarar mientras se programa
        base.write_volatile(addr as u32);
        base.add(1).write_volatile((addr >> 32) as u32);
        base.add(2).write_volatile(data);
        base.add(3).write_volatile(control);
    }
}

/// Ventanas MMIO sin caché del kernel (tablas MSI-X y similares), asignadas
/// de forma incremental y nunca liberadas: se usan mientras viva el dispositivo.
static UC_NEXT: IrqLock<u64> = IrqLock::new(0xFFFF_D200_0000_0000);

fn map_uc(phys: u64, len: u64) -> Result<u64, &'static str> {
    use crate::memory::vmm;
    use aricode_x86_paging::{
        CACHE_DISABLE, GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE, WRITE_THROUGH,
    };
    let offset = phys & (PAGE_SIZE - 1);
    let start = phys - offset;
    let span = (offset + len).next_multiple_of(PAGE_SIZE);
    let mut next = UC_NEXT.lock();
    let virt = *next;
    vmm::with(|m| {
        m.map_range_4k(
            virt,
            start,
            span,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .map_err(|_| "hw: mapear ventana sin cache")?;
    *next += span;
    Ok(virt + offset)
}

/// Crea el objeto IRQ de una entrada MSI-X: asigna vector y programa la
/// entrada hacia el LAPIC de arranque.
pub fn register_msix(table_phys: u64, entry: u32) -> Result<ObjectRef, &'static str> {
    let table_virt = map_uc(table_phys, 16 * (entry as u64 + 1))?;
    let mut vectors = VECTORS.lock();
    let vector = (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST)
        .find(|v| !vectors.contains_key(v))
        .ok_or("hw: sin vectores libres")?;
    let lapic_id = apic::bsp_id() as u64;
    msix_write(
        table_virt,
        entry,
        0xFEE0_0000 | (lapic_id << 12),
        vector as u32,
        0,
    );
    let obj = KernelObject::new(ObjectKind::Irq(IrqLine {
        source: IrqSource::Msix { table_virt, entry },
        vector,
        pending: AtomicU32::new(0),
        waiters: WaitQueue::new(),
    }));
    vectors.insert(vector, alloc::sync::Arc::downgrade(&obj));
    Ok(obj)
}

/// Región MMIO de un dispositivo (física, sin caché) o un buffer DMA
/// (RAM contigua, con caché).
pub struct Region {
    pub phys: u64,
    pub len: u64,
    pub cacheable: bool,
}

/// Buffer DMA: frames contiguos que vuelven al PMM al soltarse el objeto.
pub struct DmaBuffer {
    pub region: Region,
    frames: usize,
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        pmm::with(|p| p.free_contiguous(Frame::from_addr(self.region.phys), self.frames))
            .expect("frames DMA");
    }
}

static VECTORS: IrqLock<BTreeMap<u8, Weak<KernelObject>>> = IrqLock::new(BTreeMap::new());

/// Crea el objeto IRQ de una GSI: asigna vector, enruta y registra.
pub fn register_irq(gsi: u32, level: bool, active_low: bool) -> Result<ObjectRef, &'static str> {
    let mut vectors = VECTORS.lock();
    let vector = (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST)
        .find(|v| !vectors.contains_key(v))
        .ok_or("hw: sin vectores libres")?;
    ioapic::route(gsi, vector, apic::bsp_id(), level, active_low)?;
    let obj = KernelObject::new(ObjectKind::Irq(IrqLine {
        source: IrqSource::Gsi { gsi, level },
        vector,
        pending: AtomicU32::new(0),
        waiters: WaitQueue::new(),
    }));
    vectors.insert(vector, alloc::sync::Arc::downgrade(&obj));
    Ok(obj)
}

/// Entrada desde el APIC para los vectores de dispositivo.
pub fn handle_device_irq(vector: u8) {
    let line = VECTORS.lock().get(&vector).and_then(Weak::upgrade);
    match line.as_ref().map(|o| &o.kind) {
        Some(ObjectKind::Irq(l)) => l.fire(),
        _ => warn!("irq: vector {vector:#x} sin linea registrada"),
    }
}

/// Reserva un buffer DMA físicamente contiguo.
pub fn dma_alloc(len: u64) -> Result<ObjectRef, &'static str> {
    if len == 0 || len > aricode_abi::DMA_MAX_LEN {
        return Err("hw: tamano DMA invalido");
    }
    let frames = len.div_ceil(FRAME_SIZE) as usize;
    let first =
        pmm::with(|p| p.allocate_contiguous(frames, 1)).ok_or("hw: sin memoria contigua")?;
    // Poner a cero: un driver no debe ver datos de otro.
    // SAFETY: frames recién asignados, accesibles por el mapa físico.
    unsafe {
        core::ptr::write_bytes(
            crate::memory::phys::phys_to_virt(first.addr()) as *mut u8,
            0,
            frames * FRAME_SIZE as usize,
        )
    };
    Ok(KernelObject::new(ObjectKind::Dma(DmaBuffer {
        region: Region {
            phys: first.addr(),
            len: frames as u64 * FRAME_SIZE,
            cacheable: true,
        },
        frames,
    })))
}

/// Banco de pruebas (`test=drivers`): IRQ0 del PIT, DMA y framebuffer como
/// MMIO, todo atendido por drivers de usuario.
pub fn selftest(info: &boot_protocol::BootInfo) {
    use crate::arch::x86_64::pit;
    use crate::user::{process, programs};
    use alloc::vec;
    use aricode_handles::Rights;

    // 1. Un driver de usuario atiende IRQ0 (PIT a 100 Hz) y reserva DMA.
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(0));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    let irq = register_irq(gsi, level, low).expect("irq0");
    let hardware = KernelObject::new(ObjectKind::Hardware);
    pit::start_periodic(100);
    let pid = process::spawn_with_handles(
        "pit_driver",
        programs::by_name("pit_driver").expect("pit_driver"),
        vec![
            (irq.clone(), Rights::READ.union(Rights::WAIT)),
            (hardware, Rights::MANAGE),
        ],
    )
    .expect("pit_driver");
    let status = process::wait(pid);
    // Silenciar la fuente antes de soltar la línea: primero se enmascara la
    // GSI, luego se para el PIT y se deja un tick para que cualquier
    // interrupción ya en vuelo llegue mientras la línea sigue registrada.
    ioapic::set_masked(gsi, true);
    pit::stop();
    crate::task::scheduler::sleep_ms(crate::time::MS_PER_TICK * 2);
    let fired = match &irq.kind {
        ObjectKind::Irq(l) => l.pending.load(Ordering::Relaxed),
        _ => 0,
    };
    drop(irq);
    assert_eq!(
        status,
        process::ExitStatus::Exited(0),
        "pit_driver: {status:?}"
    );
    assert!(
        VECTORS.lock().is_empty(),
        "el vector debe liberarse al soltar la linea"
    );

    // 2. Un driver de usuario pinta el framebuffer a través de una capability MMIO.
    let fb = info.framebuffer;
    if fb.address != 0 {
        let region = KernelObject::new(ObjectKind::Mmio(Region {
            phys: fb.address,
            len: fb.size,
            cacheable: false,
        }));
        let pid = process::spawn_with_handles_and_arg(
            "fb_driver",
            programs::by_name("fb_driver").expect("fb_driver"),
            vec![(region, Rights::READ.union(Rights::WRITE))],
            fb.stride as u64,
            0,
        )
        .expect("fb_driver");
        let status = process::wait(pid);
        assert_eq!(
            status,
            process::ExitStatus::Exited(0),
            "fb_driver: {status:?}"
        );
        // SAFETY: framebuffer accesible por el mapa físico.
        let pixel = unsafe {
            (crate::memory::phys::phys_to_virt(fb.address) as *const u32).read_volatile()
        };
        assert_eq!(pixel, 0x00FF_8800, "el driver debia pintar el pixel (0,0)");
    }
    log::info!(
        "drivers selftest ok (IRQ0 atendida en ring 3, {fired} pendientes al cerrar, DMA reservado y mapeado, framebuffer pintado desde ring 3)"
    );
}
