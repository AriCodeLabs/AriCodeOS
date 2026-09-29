//! IOMMU (Fase 33): Intel VT-d. Cada proceso que usa hardware tiene un
//! dominio de traducción propio en el que solo están mapeados, con
//! IOVA = dirección física, los buffers DMA que reservó (`dma_alloc`) o que
//! le pasaron y mapeó (`mmio_map` de un objeto DMA). Un dispositivo se asigna
//! al dominio del proceso que mapea su espacio de configuración o un BAR.
//! Cualquier otro acceso del dispositivo a memoria falla en la unidad, que
//! lo registra y avisa por interrupción; el kernel lo anuncia y lo cuenta.
//!
//! Sin tabla DMAR (máquina sin VT-d o QEMU sin `intel-iommu`) todo sigue
//! funcionando sin traducción: cada función devuelve éxito sin hacer nada.

use crate::arch::x86_64::apic;
use crate::memory::{phys, pmm, vmm};
use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aricode_acpi::Dmar;
use aricode_iommu::{
    ccmd, fectl, fsts, gcmd, iotlb, regs, Capability, ContextEntry, ExtendedCapability,
    FaultRecord, IommuError, RootEntry, SecondLevel,
};
use aricode_pmm::Frame;
use aricode_x86_paging::{OffsetMap, PAGE_SIZE};
use core::sync::atomic::{AtomicU64, Ordering};
use log::{info, warn};

/// Vector del LAPIC del BSP al que las unidades envían sus fallos.
pub const FAULT_VECTOR: u8 = 0x24;

type Tables = SecondLevel<vmm::PmmFrames, OffsetMap>;

struct Unit {
    regs: u64,
    root_table: u64,
    cap: Capability,
    ecap: ExtendedCapability,
}

impl Unit {
    fn r32(&self, off: u64) -> u32 {
        // SAFETY: registro de la ventana MMIO mapeada en `init`.
        unsafe { ((self.regs + off) as *const u32).read_volatile() }
    }
    fn r64(&self, off: u64) -> u64 {
        // SAFETY: ídem.
        unsafe { ((self.regs + off) as *const u64).read_volatile() }
    }
    fn w32(&self, off: u64, v: u32) {
        // SAFETY: ídem.
        unsafe { ((self.regs + off) as *mut u32).write_volatile(v) }
    }
    fn w64(&self, off: u64, v: u64) {
        // SAFETY: ídem.
        unsafe { ((self.regs + off) as *mut u64).write_volatile(v) }
    }

    /// Espera acotada a que `reg & mask` valga `set`.
    fn wait32(&self, reg: u64, mask: u32, set: bool) -> bool {
        for _ in 0..1_000_000 {
            if (self.r32(reg) & mask != 0) == set {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
    fn wait64_clear(&self, reg: u64, mask: u64) -> bool {
        for _ in 0..1_000_000 {
            if self.r64(reg) & mask == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Orden global de un solo disparo conservando la traducción activa.
    fn global_command(&self, bit: u32) -> bool {
        let te = self.r32(regs::GLOBAL_STATUS) & gcmd::TRANSLATION_ENABLE;
        self.w32(regs::GLOBAL_COMMAND, te | bit);
        self.wait32(regs::GLOBAL_STATUS, bit, true)
    }

    /// Invalida la caché de contextos y el IOTLB completos (y vacía el
    /// buffer de escritura si la unidad lo exige). Tras cualquier cambio de
    /// tablas: sencillo y correcto también con `caching mode`.
    fn invalidate_all(&self) {
        if self.cap.write_buffer_flush() {
            self.global_command(gcmd::WRITE_BUFFER_FLUSH);
        }
        self.w64(regs::CONTEXT_COMMAND, ccmd::INVALIDATE | ccmd::GLOBAL);
        if !self.wait64_clear(regs::CONTEXT_COMMAND, ccmd::INVALIDATE) {
            warn!("iommu: la invalidacion de contextos no termina");
        }
        self.invalidate_iotlb();
    }

    fn invalidate_iotlb(&self) {
        let reg = self.ecap.iotlb_offset() + regs::IOTLB_FROM_IRO;
        let mut v = iotlb::INVALIDATE | iotlb::GLOBAL;
        if self.cap.drain_reads() {
            v |= iotlb::DRAIN_READS;
        }
        if self.cap.drain_writes() {
            v |= iotlb::DRAIN_WRITES;
        }
        self.w64(reg, v);
        if !self.wait64_clear(reg, iotlb::INVALIDATE) {
            warn!("iommu: la invalidacion del IOTLB no termina");
        }
    }
}

struct Domain {
    did: u16,
    tables: Tables,
    /// Regiones mapeadas (inicio de página -> longitud).
    regions: BTreeMap<u64, u64>,
    /// (unidad, source id) de los dispositivos asignados.
    devices: Vec<(usize, u16)>,
}

struct State {
    dmar: Dmar,
    units: Vec<Unit>,
    levels: u8,
    flush: Option<fn(u64)>,
    domains: BTreeMap<u32, Domain>,
    /// Propietario único de cada función PCI (clave del dominio que tiene su
    /// entrada de contexto). Auditoría 3eb4965, hallazgo 2: liberar un
    /// dominio solo retira los contextos que siguen siendo suyos.
    owners: BTreeMap<u16, u32>,
    free_dids: Vec<u16>,
    next_did: u16,
    max_did: u16,
}

static IOMMU: IrqLock<Option<State>> = IrqLock::new(None);
static FAULTS: AtomicU64 = AtomicU64::new(0);

fn clflush(addr: u64) {
    // SAFETY: escribe a memoria la línea de caché de una entrada de tabla.
    unsafe { core::arch::asm!("clflush [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

fn flush_line(flush: Option<fn(u64)>, addr: u64) {
    if let Some(f) = flush {
        f(addr);
    }
}

/// Frame a cero para una tabla (raíz o de contexto).
fn zeroed_table(flush: Option<fn(u64)>) -> Option<Frame> {
    let f = pmm::with(|p| p.allocate())?;
    let virt = phys::phys_to_virt(f.addr());
    // SAFETY: frame recién asignado, accesible por el mapa físico.
    unsafe { core::ptr::write_bytes(virt as *mut u8, 0, PAGE_SIZE as usize) };
    for line in (0..PAGE_SIZE).step_by(64) {
        flush_line(flush, virt + line);
    }
    Some(f)
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut guard = IOMMU.lock();
    guard.as_mut().map(f)
}

/// Localiza las unidades de la DMAR, programa sus tablas raíz y su
/// interrupción de fallo, da un dominio identidad a los dispositivos que el
/// firmware sigue usando (RMRR y controladores gráficos, que leen el
/// framebuffer) y activa la traducción. Debe llamarse tras el APIC, el PMM y
/// la enumeración PCI, antes de que arranque cualquier driver.
pub fn init(framebuffer: &boot_protocol::FramebufferInfo) {
    let Some(dmar) = crate::acpi::with(|a| a.dmar.clone()) else {
        info!("iommu: sin tabla DMAR; el DMA de los drivers no se traduce");
        return;
    };
    let mut units = Vec::new();
    let mut levels = 4u8;
    let mut coherent = true;
    for (i, d) in dmar.units.iter().enumerate() {
        let regs = match crate::hw::map_uc(d.register_base, PAGE_SIZE) {
            Ok(v) => v,
            Err(e) => {
                warn!("iommu: unidad {i} @ {:#x}: {e}", d.register_base);
                continue;
            }
        };
        let unit = Unit {
            regs,
            root_table: 0,
            cap: Capability(0),
            ecap: ExtendedCapability(0),
        };
        let cap = Capability(unit.r64(regs::CAPABILITY));
        let ecap = ExtendedCapability(unit.r64(regs::EXTENDED_CAPABILITY));
        let Some(l) = cap.levels() else {
            warn!(
                "iommu: unidad {i}: SAGAW {:#b} sin paginacion de 3 ni 4 niveles; omitida",
                cap.sagaw()
            );
            continue;
        };
        levels = levels.min(l);
        coherent &= ecap.coherent();
        units.push(Unit { cap, ecap, ..unit });
    }
    if units.is_empty() {
        warn!("iommu: DMAR sin unidades utilizables; el DMA no se traduce");
        return;
    }
    let flush: Option<fn(u64)> = if coherent { None } else { Some(clflush) };
    let mut max_did = u16::MAX;
    for (i, u) in units.iter_mut().enumerate() {
        let Some(root) = zeroed_table(flush) else {
            warn!("iommu: sin frames para la tabla raiz");
            return;
        };
        u.root_table = root.addr();
        u.w64(regs::ROOT_TABLE_ADDRESS, root.addr());
        if !u.global_command(gcmd::SET_ROOT_TABLE_POINTER) {
            warn!("iommu: unidad {i}: no acepta la tabla raiz");
        }
        // Fallos por MSI al BSP.
        u.w32(regs::FAULT_EVENT_DATA, FAULT_VECTOR as u32);
        u.w32(
            regs::FAULT_EVENT_ADDRESS,
            0xFEE0_0000 | (apic::bsp_id() as u32) << 12,
        );
        u.w32(regs::FAULT_EVENT_UPPER_ADDRESS, 0);
        u.w32(regs::FAULT_STATUS, fsts::CLEAR_MASK);
        u.w32(regs::FAULT_EVENT_CONTROL, 0);
        max_did = max_did.min((u.cap.domains() - 1).min(u16::MAX as u32) as u16);
    }
    let reserved = dmar.reserved.len();
    let n = units.len();
    let mut state = State {
        dmar,
        units,
        levels,
        flush,
        domains: BTreeMap::new(),
        owners: BTreeMap::new(),
        free_dids: Vec::new(),
        next_did: 1,
        max_did,
    };

    // Dispositivos del firmware: los que tienen regiones reservadas y los
    // controladores gráficos (clase 03), que barren el framebuffer sin que
    // ningún driver de AriCodeOS los reclame. Un dominio identidad cada uno,
    // con clave fuera del rango de PIDs.
    let mut firmware: Vec<(u16, Vec<(u64, u64)>)> = Vec::new();
    for r in &state.dmar.reserved {
        for scope in r.scopes.iter().filter(|s| s.path.len() == 1) {
            let (dev, func) = scope.path[0];
            let sid = aricode_iommu::source_id(scope.start_bus, dev, func);
            match firmware.iter_mut().find(|(s, _)| *s == sid) {
                Some((_, ranges)) => ranges.push((r.base, r.limit + 1 - r.base)),
                None => firmware.push((sid, alloc::vec![(r.base, r.limit + 1 - r.base)])),
            }
        }
    }
    for (bus, dev, func) in crate::pci::functions_of_class(0x03) {
        let sid = aricode_iommu::source_id(bus, dev, func);
        let mut ranges = Vec::new();
        if framebuffer.address != 0 {
            ranges.push((framebuffer.address, framebuffer.size));
        }
        match firmware.iter_mut().find(|(s, _)| *s == sid) {
            Some((_, r)) => r.extend(ranges),
            None => firmware.push((sid, ranges)),
        }
    }
    let mut firmware_names = Vec::new();
    for (i, (sid, ranges)) in firmware.iter().enumerate() {
        let key = u32::MAX - i as u32;
        let mut ok = true;
        for &(base, len) in ranges {
            if let Err(e) = state.map_range(key, base, len) {
                warn!("iommu: dispositivo {sid:#06x}: region {base:#x}+{len:#x}: {e}");
                ok = false;
            }
        }
        if ok {
            match state.attach(key, *sid) {
                Ok(()) => firmware_names.push(alloc::format!(
                    "{:02x}:{:02x}.{}",
                    sid >> 8,
                    (sid >> 3) & 0x1F,
                    sid & 7
                )),
                Err(e) => warn!("iommu: dispositivo {sid:#06x}: {e}"),
            }
        }
    }

    for (i, u) in state.units.iter().enumerate() {
        u.invalidate_all();
        let active = u.global_command(gcmd::TRANSLATION_ENABLE);
        let ver = u.r32(regs::VERSION);
        info!(
            "iommu: unidad {i} @ {:#x} version {}.{}: {} bits de invitado, {} niveles, {} dominios, {} registro(s) de fallo{}{}; traduccion {}",
            state.dmar.units[i].register_base,
            (ver >> 4) & 0xF,
            ver & 0xF,
            u.cap.guest_address_width(),
            levels,
            u.cap.domains(),
            u.cap.fault_records(),
            if u.cap.caching_mode() {
                ", caching mode"
            } else {
                ""
            },
            if u.ecap.coherent() {
                ""
            } else {
                ", sin coherencia (clflush)"
            },
            if active { "activa" } else { "NO activada" }
        );
    }
    info!(
        "iommu: {n} unidad(es) activas, {reserved} region(es) reservadas (RMRR), dominio identidad para {} dispositivo(s) del firmware {:?}; un dominio por proceso con IOVA = fisica",
        firmware_names.len(),
        firmware_names
    );
    *IOMMU.lock() = Some(state);
}

/// `true` si hay traducción DMA activa.
pub fn enabled() -> bool {
    IOMMU.lock().is_some()
}

/// Las claves por encima son dominios identidad del firmware, no procesos.
const FIRMWARE_KEYS: u32 = u32::MAX / 2;

/// (unidades, dominios de procesos vivos, fallos registrados) para `system_info`.
pub fn stats() -> (u64, u64, u64) {
    let (units, domains) = with(|s| {
        (
            s.units.len() as u64,
            s.domains.keys().filter(|k| **k <= FIRMWARE_KEYS).count() as u64,
        )
    })
    .unwrap_or((0, 0));
    (units, domains, FAULTS.load(Ordering::Relaxed))
}

pub fn faults() -> u64 {
    FAULTS.load(Ordering::Relaxed)
}

impl State {
    fn domain(&mut self, pid: u32) -> Result<&mut Domain, &'static str> {
        if !self.domains.contains_key(&pid) {
            let did = match self.free_dids.pop() {
                Some(d) => d,
                None if self.next_did <= self.max_did => {
                    let d = self.next_did;
                    self.next_did += 1;
                    d
                }
                None => return Err("iommu: sin dominios libres"),
            };
            let tables = SecondLevel::new(
                vmm::PmmFrames,
                OffsetMap(phys::offset()),
                self.levels,
                self.flush,
            )
            .ok_or("iommu: sin frames para el dominio")?;
            self.domains.insert(
                pid,
                Domain {
                    did,
                    tables,
                    regions: BTreeMap::new(),
                    devices: Vec::new(),
                },
            );
        }
        Ok(self.domains.get_mut(&pid).expect("dominio recien creado"))
    }

    /// Unidades con algún dispositivo del dominio.
    fn units_of(dom: &Domain) -> Vec<usize> {
        let mut units: Vec<usize> = dom.devices.iter().map(|d| d.0).collect();
        units.sort_unstable();
        units.dedup();
        units
    }

    fn map_range(&mut self, pid: u32, phys: u64, len: u64) -> Result<(), &'static str> {
        let start = phys & !(PAGE_SIZE - 1);
        let end = (phys + len).next_multiple_of(PAGE_SIZE);
        let dom = self.domain(pid)?;
        if dom.regions.contains_key(&start) {
            return Ok(());
        }
        for page in (start..end).step_by(PAGE_SIZE as usize) {
            match dom.tables.map_4k(page, page, true) {
                Ok(()) | Err(IommuError::AlreadyMapped) => {}
                Err(IommuError::OutOfRange) => {
                    return Err("iommu: direccion fuera del ancho de la unidad")
                }
                Err(_) => return Err("iommu: sin frames para las tablas"),
            }
        }
        dom.regions.insert(start, end - start);
        for u in Self::units_of(dom) {
            self.units[u].invalidate_iotlb();
        }
        Ok(())
    }

    /// Puntero a la entrada de contexto de `source` en `unit`, creando la
    /// tabla de contexto del bus si hace falta.
    fn context_entry(&mut self, unit: usize, source: u16) -> Result<*mut u64, &'static str> {
        let flush = self.flush;
        let u = &self.units[unit];
        let bus = (source >> 8) as u64;
        let root_entry = phys::phys_to_virt(u.root_table + bus * 16) as *mut u64;
        // SAFETY: tabla raíz de la unidad, accesible por el mapa físico.
        let mut root = unsafe {
            RootEntry {
                low: root_entry.read_volatile(),
                high: root_entry.add(1).read_volatile(),
            }
        };
        if !root.present() {
            let f = zeroed_table(flush).ok_or("iommu: sin frames para la tabla de contexto")?;
            root = RootEntry::new(f.addr());
            // SAFETY: ídem; la parte alta antes que la baja (bit presente).
            unsafe {
                root_entry.add(1).write_volatile(root.high);
                root_entry.write_volatile(root.low);
            }
            flush_line(flush, root_entry as u64);
        }
        let devfn = (source & 0xFF) as u64;
        Ok(phys::phys_to_virt(root.context_table() + devfn * 16) as *mut u64)
    }
}

fn write_context(ptr: *mut u64, entry: ContextEntry, flush: Option<fn(u64)>) {
    // SAFETY: entrada de una tabla de contexto creada por `context_entry`.
    // Al quitar, primero la baja (presente a 0); al poner, la alta primero.
    unsafe {
        if entry.present() {
            ptr.add(1).write_volatile(entry.high);
            ptr.write_volatile(entry.low);
        } else {
            ptr.write_volatile(0);
            ptr.add(1).write_volatile(0);
        }
    }
    flush_line(flush, ptr as u64);
}

impl State {
    /// Asigna la función PCI `source` al dominio `key` (un PID, o una clave
    /// del firmware), creándolo si no existe. Mapea antes sus RMRR.
    fn attach(&mut self, key: u32, source: u16) -> Result<(), &'static str> {
        let (bus, dev, func) = (
            (source >> 8) as u8,
            ((source >> 3) & 0x1F) as u8,
            (source & 7) as u8,
        );
        let unit = self
            .dmar
            .unit_for(0, bus, dev, func)
            .ok_or("iommu: dispositivo fuera de toda unidad")?;
        let reserved: Vec<(u64, u64)> = self
            .dmar
            .reserved_for(0, bus, dev, func)
            .map(|r| (r.base, r.limit + 1 - r.base))
            .collect();
        for (base, len) in reserved {
            self.map_range(key, base, len)?;
        }
        if self.owners.get(&source) == Some(&key) {
            return Ok(());
        }
        let levels = self.levels;
        let flush = self.flush;
        // Todo lo que puede fallar, antes de anotar nada: la tabla de contexto
        // del bus y el dominio receptor.
        let ptr = self.context_entry(unit, source)?;
        let (did, root) = {
            let dom = self.domain(key)?;
            (dom.did, dom.tables.root_phys())
        };
        write_context(ptr, ContextEntry::new(root, did, levels), flush);
        self.units[unit].invalidate_all();
        // Un solo propietario: el dominio anterior deja de listar la función.
        let previous = self.owners.insert(source, key);
        if let Some(prev) = previous.filter(|p| *p != key) {
            if let Some(old) = self.domains.get_mut(&prev) {
                old.devices.retain(|d| *d != (unit, source));
            }
        }
        let dom = self.domains.get_mut(&key).expect("dominio recien creado");
        if !dom.devices.contains(&(unit, source)) {
            dom.devices.push((unit, source));
        }
        match previous {
            Some(prev) if prev != key && key <= FIRMWARE_KEYS => info!(
                "iommu: {bus:02x}:{dev:02x}.{func} reasignado del dominio del proceso {prev} al dominio {did} (proceso {key})"
            ),
            _ if key > FIRMWARE_KEYS => {
                info!("iommu: {bus:02x}:{dev:02x}.{func} en el dominio identidad {did} (firmware)")
            }
            _ => info!("iommu: {bus:02x}:{dev:02x}.{func} asignado al dominio {did} (proceso {key})"),
        }
        Ok(())
    }

    /// Dominio (DID) que la entrada de contexto real de `source` señala, o
    /// `None` si no está presente. Lee las tablas, no la contabilidad.
    fn context_domain(&mut self, source: u16) -> Option<u16> {
        let (bus, dev, func) = (
            (source >> 8) as u8,
            ((source >> 3) & 0x1F) as u8,
            (source & 7) as u8,
        );
        let unit = self.dmar.unit_for(0, bus, dev, func)?;
        let u = &self.units[unit];
        let root_entry = phys::phys_to_virt(u.root_table + (bus as u64) * 16) as *const u64;
        // SAFETY: tabla raíz de la unidad, accesible por el mapa físico.
        let root = RootEntry {
            low: unsafe { root_entry.read_volatile() },
            high: 0,
        };
        if !root.present() {
            return None;
        }
        let ptr =
            phys::phys_to_virt(root.context_table() + (source & 0xFF) as u64 * 16) as *const u64;
        // SAFETY: tabla de contexto creada por `context_entry`.
        let entry = unsafe {
            ContextEntry {
                low: ptr.read_volatile(),
                high: ptr.add(1).read_volatile(),
            }
        };
        entry.present().then(|| entry.domain())
    }
}

/// DID que la entrada de contexto de `source` señala ahora mismo (pruebas).
pub fn context_domain(source: u16) -> Option<u16> {
    with(|s| s.context_domain(source)).flatten()
}

/// Asigna la función PCI `source` al dominio del proceso `pid`.
pub fn attach(pid: u32, source: u16) -> Result<(), &'static str> {
    with(|s| s.attach(pid, source)).unwrap_or(Ok(()))
}

/// Mapea `[phys, phys+len)` identidad en el dominio de `pid`.
pub fn map_region(pid: u32, phys: u64, len: u64) -> Result<(), &'static str> {
    with(|s| s.map_range(pid, phys, len)).unwrap_or(Ok(()))
}

/// Retira una región de todos los dominios que la tengan (el buffer DMA se
/// libera: ningún dispositivo debe poder seguir escribiendo en él).
pub fn unmap_everywhere(phys: u64, len: u64) {
    let start = phys & !(PAGE_SIZE - 1);
    let _ = len;
    with(|s| {
        let mut affected: Vec<usize> = Vec::new();
        for dom in s.domains.values_mut() {
            if let Some(l) = dom.regions.remove(&start) {
                for page in (start..start + l).step_by(PAGE_SIZE as usize) {
                    let _ = dom.tables.unmap_4k(page);
                }
                affected.extend(State::units_of(dom));
            }
        }
        affected.sort_unstable();
        affected.dedup();
        for u in affected {
            s.units[u].invalidate_iotlb();
        }
    });
}

/// Destruye el dominio de un proceso terminado: sus dispositivos dejan de
/// poder acceder a memoria y las tablas vuelven al PMM.
pub fn release(pid: u32) {
    with(|s| {
        let Some(dom) = s.domains.remove(&pid) else {
            return;
        };
        let flush = s.flush;
        for &(unit, source) in &dom.devices {
            // Solo los contextos que siguen siendo de este dominio: una
            // función reasignada a otro proceso conserva el suyo.
            if s.owners.get(&source) != Some(&pid) {
                continue;
            }
            if let Ok(ptr) = s.context_entry(unit, source) {
                write_context(ptr, ContextEntry::NOT_PRESENT, flush);
            }
            s.owners.remove(&source);
        }
        for u in State::units_of(&dom) {
            s.units[u].invalidate_all();
        }
        let did = dom.did;
        let devices = dom.devices.len();
        let mut frames = 0;
        dom.tables.free_tables(|f| {
            frames += 1;
            pmm::with(|p| p.free(Frame::from_addr(f))).expect("tabla del dominio");
        });
        s.free_dids.push(did);
        log::debug!(
            "iommu: dominio {did} del proceso {pid} liberado ({devices} dispositivo(s), {frames} tablas)"
        );
    });
}

/// Interrupción de fallo de una unidad: anuncia y limpia cada registro.
pub fn on_fault_irq() {
    with(|s| {
        for (i, u) in s.units.iter().enumerate() {
            let status = u.r32(regs::FAULT_STATUS);
            if status & (fsts::PRIMARY_PENDING_FAULT | fsts::PRIMARY_FAULT_OVERFLOW) == 0 {
                continue;
            }
            let fro = u.cap.fault_record_offset();
            for n in 0..u.cap.fault_records() as u64 {
                let off = fro + 16 * n;
                let (low, high) = (u.r64(off), u.r64(off + 8));
                if let Some(f) = FaultRecord::decode(low, high) {
                    FAULTS.fetch_add(1, Ordering::Relaxed);
                    crate::events::record(
                        aricode_abi::sysinfo::EventKind::DmaFault,
                        0,
                        f.source_id as u64,
                        aricode_iommu::fault_reason_name(f.reason),
                    );
                    warn!(
                        "iommu: fallo DMA de {:02x}:{:02x}.{} ({} en {:#x}): {} (motivo {:#x}, unidad {i})",
                        f.bus(),
                        f.device(),
                        f.function(),
                        if f.is_read { "lectura" } else { "escritura" },
                        f.address,
                        aricode_iommu::fault_reason_name(f.reason),
                        f.reason
                    );
                    u.w32(off + 12, 1 << 31);
                }
            }
            if status & fsts::PRIMARY_FAULT_OVERFLOW != 0 {
                warn!("iommu: unidad {i}: registros de fallo desbordados");
            }
            u.w32(regs::FAULT_STATUS, status & fsts::CLEAR_MASK);
            let _ = fectl::INTERRUPT_MASK;
        }
    });
}

/// Autoprueba (`test=iommu`): un driver de prueba en ring 3 recibe el
/// virtio-blk, lee un sector en su buffer (traducido) y luego pide al
/// dispositivo escribir en memoria que no es suya: la unidad lo bloquea, lo
/// registra y el proceso lo comprueba con `system_info`.
pub fn selftest() {
    use crate::object::{KernelObject, ObjectKind};
    use crate::user::{process, programs};
    use aricode_handles::Rights;

    assert!(
        enabled(),
        "iommu: sin unidad VT-d (QEMU necesita -device intel-iommu)"
    );
    let index = crate::pci::find(0x1af4, 0x1042).expect("virtio-blk moderno en el bus PCI");
    let claimed = crate::pci::claim(index).expect("reclamar virtio-blk");
    let mut extra = alloc::vec![(KernelObject::new(ObjectKind::System), Rights::READ)];
    let mut mask = 0u64;
    for (i, slot) in claimed.handles.into_iter().enumerate() {
        if let Some(h) = slot {
            match i {
                2 => mask |= 1 << 15,
                3..=8 => mask |= 1 << (i - 3),
                _ => {}
            }
            extra.push(h);
        }
    }
    // La memoria a la que apunta el driver de prueba (0x1000, reservada) no
    // debe cambiar ni un byte.
    const FOREIGN_PHYS: u64 = 0x1000;
    let snapshot = |len: usize| -> Vec<u8> {
        // SAFETY: memoria baja reservada, accesible por el mapa físico.
        unsafe { core::slice::from_raw_parts(phys::phys_to_virt(FOREIGN_PHYS) as *const u8, len) }
            .to_vec()
    };
    let foreign_before = snapshot(512);
    let before = faults();
    let pid = process::spawn_with_handles_and_arg(
        "dma_rogue",
        programs::by_name("dma_rogue").expect("dma_rogue en el initrd"),
        extra,
        mask,
        0,
    )
    .expect("dma_rogue");
    let status = process::wait(pid);
    assert_eq!(
        status,
        process::ExitStatus::Exited(0),
        "dma_rogue: {status:?}"
    );
    let recorded = faults() - before;
    assert!(recorded >= 1, "la unidad no registro el DMA ilegal");
    assert_eq!(
        snapshot(512),
        foreign_before,
        "el dispositivo escribio en memoria fuera de su dominio"
    );
    let (_, domains, _) = stats();
    assert_eq!(
        domains, 0,
        "el dominio del driver debe liberarse con el (quedan {domains})"
    );

    // Reasignación (auditoría 3eb4965, hallazgo 2): la función pasa del
    // dominio A al B (transferencia de la capability MMIO); A termina y B
    // conserva su contexto; al terminar B el contexto desaparece.
    let (bus, dev, func) = crate::pci::functions_of_class(0x01)
        .into_iter()
        .next()
        .expect("una funcion de almacenamiento (virtio-blk)");
    let sid = aricode_iommu::source_id(bus, dev, func);
    const A: u32 = 1001;
    const B: u32 = 1002;
    attach(A, sid).expect("attach A");
    let did_a = context_domain(sid).expect("contexto de A presente");
    attach(B, sid).expect("attach B");
    let did_b = context_domain(sid).expect("contexto de B presente");
    assert_ne!(did_a, did_b, "B debe tener su propio dominio");
    release(A);
    assert_eq!(
        context_domain(sid),
        Some(did_b),
        "liberar A no debe tocar el contexto que ya es de B"
    );
    release(B);
    assert_eq!(
        context_domain(sid),
        None,
        "al liberar B el contexto se retira"
    );
    let (_, domains, _) = stats();
    assert_eq!(domains, 0);
    info!(
        "iommu selftest ok (lectura legitima traducida, DMA a memoria ajena bloqueado con {recorded} fallo(s) registrados y la memoria intacta, dominio liberado, reasignacion de {bus:02x}:{dev:02x}.{func} conservada)"
    );
}
