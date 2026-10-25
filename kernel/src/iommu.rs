// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! IOMMU (Phase 33): Intel VT-d. Every process that uses hardware has its
//! own translation domain in which only the DMA buffers it allocated
//! (`dma_alloc`) or was handed and mapped (`mmio_map` of a DMA object) are
//! mapped, with IOVA = physical address. A device is assigned to the domain
//! of the process that maps its configuration space or a BAR. Any other
//! memory access by the device faults in the unit, which records it and
//! signals an interrupt; the kernel reports and counts it.
//!
//! Without a DMAR table (machine without VT-d or QEMU without `intel-iommu`)
//! everything keeps working untranslated: every function returns success
//! without doing anything.

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

/// Vector on the BSP's LAPIC to which the units send their faults.
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
        // SAFETY: register in the MMIO window mapped in `init`.
        unsafe { ((self.regs + off) as *const u32).read_volatile() }
    }
    fn r64(&self, off: u64) -> u64 {
        // SAFETY: same as above.
        unsafe { ((self.regs + off) as *const u64).read_volatile() }
    }
    fn w32(&self, off: u64, v: u32) {
        // SAFETY: same as above.
        unsafe { ((self.regs + off) as *mut u32).write_volatile(v) }
    }
    fn w64(&self, off: u64, v: u64) {
        // SAFETY: same as above.
        unsafe { ((self.regs + off) as *mut u64).write_volatile(v) }
    }

    /// Bounded wait for `reg & mask` to equal `set`.
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

    /// One-shot global command preserving the active translation.
    fn global_command(&self, bit: u32) -> bool {
        let te = self.r32(regs::GLOBAL_STATUS) & gcmd::TRANSLATION_ENABLE;
        self.w32(regs::GLOBAL_COMMAND, te | bit);
        self.wait32(regs::GLOBAL_STATUS, bit, true)
    }

    /// Invalidates the whole context cache and IOTLB (and flushes the write
    /// buffer if the unit requires it). After any table change: simple and
    /// also correct with `caching mode`.
    fn invalidate_all(&self) {
        if self.cap.write_buffer_flush() {
            self.global_command(gcmd::WRITE_BUFFER_FLUSH);
        }
        self.w64(regs::CONTEXT_COMMAND, ccmd::INVALIDATE | ccmd::GLOBAL);
        if !self.wait64_clear(regs::CONTEXT_COMMAND, ccmd::INVALIDATE) {
            warn!("iommu: context invalidation does not complete");
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
            warn!("iommu: IOTLB invalidation does not complete");
        }
    }
}

struct Domain {
    did: u16,
    tables: Tables,
    /// Mapped regions (page start -> length).
    regions: BTreeMap<u64, u64>,
    /// (unit, source id) of the assigned devices.
    devices: Vec<(usize, u16)>,
}

struct State {
    dmar: Dmar,
    units: Vec<Unit>,
    levels: u8,
    flush: Option<fn(u64)>,
    domains: BTreeMap<u32, Domain>,
    /// Single owner of each PCI function (key of the domain holding its
    /// context entry). Audit 3eb4965, finding 2: releasing a domain only
    /// removes the contexts that still belong to it.
    owners: BTreeMap<u16, u32>,
    free_dids: Vec<u16>,
    next_did: u16,
    max_did: u16,
}

static IOMMU: IrqLock<Option<State>> = IrqLock::new(None);
static FAULTS: AtomicU64 = AtomicU64::new(0);

fn clflush(addr: u64) {
    // SAFETY: writes the cache line of a table entry back to memory.
    unsafe { core::arch::asm!("clflush [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

fn flush_line(flush: Option<fn(u64)>, addr: u64) {
    if let Some(f) = flush {
        f(addr);
    }
}

/// Zeroed frame for a table (root or context).
fn zeroed_table(flush: Option<fn(u64)>) -> Option<Frame> {
    let f = pmm::with(|p| p.allocate())?;
    let virt = phys::phys_to_virt(f.addr());
    // SAFETY: freshly allocated frame, reachable through the physical map.
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

/// Locates the DMAR units, programs their root tables and fault interrupt,
/// gives an identity domain to the devices the firmware still uses (RMRR and
/// graphics controllers, which read the framebuffer) and enables
/// translation. Must be called after the APIC, the PMM and PCI enumeration,
/// before any driver starts.
pub fn init(framebuffer: &boot_protocol::FramebufferInfo) {
    let Some(dmar) = crate::acpi::with(|a| a.dmar.clone()) else {
        info!("iommu: no DMAR table; driver DMA is not translated");
        return;
    };
    let mut units = Vec::new();
    let mut levels = 4u8;
    let mut coherent = true;
    for (i, d) in dmar.units.iter().enumerate() {
        let regs = match crate::hw::map_uc(d.register_base, PAGE_SIZE) {
            Ok(v) => v,
            Err(e) => {
                warn!("iommu: unit {i} @ {:#x}: {e}", d.register_base);
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
                "iommu: unit {i}: SAGAW {:#b} without 3- or 4-level paging; skipped",
                cap.sagaw()
            );
            continue;
        };
        levels = levels.min(l);
        coherent &= ecap.coherent();
        units.push(Unit { cap, ecap, ..unit });
    }
    if units.is_empty() {
        warn!("iommu: DMAR without usable units; DMA is not translated");
        return;
    }
    let flush: Option<fn(u64)> = if coherent { None } else { Some(clflush) };
    let mut max_did = u16::MAX;
    for (i, u) in units.iter_mut().enumerate() {
        let Some(root) = zeroed_table(flush) else {
            warn!("iommu: no frames for the root table");
            return;
        };
        u.root_table = root.addr();
        u.w64(regs::ROOT_TABLE_ADDRESS, root.addr());
        if !u.global_command(gcmd::SET_ROOT_TABLE_POINTER) {
            warn!("iommu: unit {i}: root table not accepted");
        }
        // Faults via MSI to the BSP.
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

    // Firmware devices: those with reserved regions and the graphics
    // controllers (class 03), which scan the framebuffer without any
    // AriCodeOS driver claiming them. One identity domain each, with a key
    // outside the PID range.
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
                warn!("iommu: device {sid:#06x}: region {base:#x}+{len:#x}: {e}");
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
                Err(e) => warn!("iommu: device {sid:#06x}: {e}"),
            }
        }
    }

    for (i, u) in state.units.iter().enumerate() {
        u.invalidate_all();
        let active = u.global_command(gcmd::TRANSLATION_ENABLE);
        let ver = u.r32(regs::VERSION);
        info!(
            "iommu: unit {i} @ {:#x} version {}.{}: {} guest address bits, {} levels, {} domains, {} fault register(s){}{}; translation {}",
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
                ", not coherent (clflush)"
            },
            if active { "active" } else { "NOT enabled" }
        );
    }
    info!(
        "iommu: {n} active unit(s), {reserved} reserved region(s) (RMRR), identity domain for {} firmware device(s) {:?}; one domain per process with IOVA = physical",
        firmware_names.len(),
        firmware_names
    );
    *IOMMU.lock() = Some(state);
}

/// `true` if DMA translation is active.
pub fn enabled() -> bool {
    IOMMU.lock().is_some()
}

/// Keys above this are firmware identity domains, not processes.
const FIRMWARE_KEYS: u32 = u32::MAX / 2;

/// (units, live process domains, recorded faults) for `system_info`.
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
                None => return Err("iommu: no free domains"),
            };
            let tables = SecondLevel::new(
                vmm::PmmFrames,
                OffsetMap(phys::offset()),
                self.levels,
                self.flush,
            )
            .ok_or("iommu: no frames for the domain")?;
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
        Ok(self.domains.get_mut(&pid).expect("freshly created domain"))
    }

    /// Units with some device of the domain.
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
        // All or nothing: a failure leaves no leaf behind that a device
        // could still use once the caller frees the buffer (audit
        // 2026-10-25 C, M1).
        match dom.tables.map_identity(start, end, true) {
            Ok(()) => {}
            Err(IommuError::OutOfRange) => return Err("iommu: address beyond the unit width"),
            Err(_) => return Err("iommu: no frames for the tables"),
        }
        dom.regions.insert(start, end - start);
        for u in Self::units_of(dom) {
            self.units[u].invalidate_iotlb();
        }
        Ok(())
    }

    /// Pointer to the context entry of `source` in `unit`, creating the bus
    /// context table if needed.
    fn context_entry(&mut self, unit: usize, source: u16) -> Result<*mut u64, &'static str> {
        let flush = self.flush;
        let u = &self.units[unit];
        let bus = (source >> 8) as u64;
        let root_entry = phys::phys_to_virt(u.root_table + bus * 16) as *mut u64;
        // SAFETY: the unit's root table, reachable through the physical map.
        let mut root = unsafe {
            RootEntry {
                low: root_entry.read_volatile(),
                high: root_entry.add(1).read_volatile(),
            }
        };
        if !root.present() {
            let f = zeroed_table(flush).ok_or("iommu: no frames for the context table")?;
            root = RootEntry::new(f.addr());
            // SAFETY: same as above; high part before low part (present bit).
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
    // SAFETY: entry of a context table created by `context_entry`.
    // When removing, low part first (present = 0); when setting, high first.
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
    /// Assigns PCI function `source` to domain `key` (a PID, or a firmware
    /// key), creating it if it does not exist. Maps its RMRRs first.
    fn attach(&mut self, key: u32, source: u16) -> Result<(), &'static str> {
        let (bus, dev, func) = (
            (source >> 8) as u8,
            ((source >> 3) & 0x1F) as u8,
            (source & 7) as u8,
        );
        let unit = self
            .dmar
            .unit_for(0, bus, dev, func)
            .ok_or("iommu: device outside every unit")?;
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
        // Everything that can fail, before recording anything: the bus context
        // table and the receiving domain.
        let ptr = self.context_entry(unit, source)?;
        let (did, root) = {
            let dom = self.domain(key)?;
            (dom.did, dom.tables.root_phys())
        };
        write_context(ptr, ContextEntry::new(root, did, levels), flush);
        self.units[unit].invalidate_all();
        // Single owner: the previous domain stops listing the function.
        let previous = self.owners.insert(source, key);
        if let Some(prev) = previous.filter(|p| *p != key) {
            if let Some(old) = self.domains.get_mut(&prev) {
                old.devices.retain(|d| *d != (unit, source));
            }
        }
        let dom = self.domains.get_mut(&key).expect("freshly created domain");
        if !dom.devices.contains(&(unit, source)) {
            dom.devices.push((unit, source));
        }
        match previous {
            Some(prev) if prev != key && key <= FIRMWARE_KEYS => info!(
                "iommu: {bus:02x}:{dev:02x}.{func} reassigned from the domain of process {prev} to domain {did} (process {key})"
            ),
            _ if key > FIRMWARE_KEYS => {
                info!("iommu: {bus:02x}:{dev:02x}.{func} in identity domain {did} (firmware)")
            }
            _ => info!("iommu: {bus:02x}:{dev:02x}.{func} assigned to domain {did} (process {key})"),
        }
        Ok(())
    }

    /// Domain (DID) the actual context entry of `source` points to, or `None`
    /// if not present. Reads the tables, not the bookkeeping.
    fn context_domain(&mut self, source: u16) -> Option<u16> {
        let (bus, dev, func) = (
            (source >> 8) as u8,
            ((source >> 3) & 0x1F) as u8,
            (source & 7) as u8,
        );
        let unit = self.dmar.unit_for(0, bus, dev, func)?;
        let u = &self.units[unit];
        let root_entry = phys::phys_to_virt(u.root_table + (bus as u64) * 16) as *const u64;
        // SAFETY: the unit's root table, reachable through the physical map.
        let root = RootEntry {
            low: unsafe { root_entry.read_volatile() },
            high: 0,
        };
        if !root.present() {
            return None;
        }
        let ptr =
            phys::phys_to_virt(root.context_table() + (source & 0xFF) as u64 * 16) as *const u64;
        // SAFETY: context table created by `context_entry`.
        let entry = unsafe {
            ContextEntry {
                low: ptr.read_volatile(),
                high: ptr.add(1).read_volatile(),
            }
        };
        entry.present().then(|| entry.domain())
    }
}

/// DID the context entry of `source` points to right now (tests).
pub fn context_domain(source: u16) -> Option<u16> {
    with(|s| s.context_domain(source)).flatten()
}

/// Assigns PCI function `source` to the domain of process `pid`.
pub fn attach(pid: u32, source: u16) -> Result<(), &'static str> {
    with(|s| s.attach(pid, source)).unwrap_or(Ok(()))
}

/// Identity-maps `[phys, phys+len)` in the domain of `pid`. Returns whether
/// this call added it (`false`: the domain had it, or there is no IOMMU).
pub fn map_region(pid: u32, phys: u64, len: u64) -> Result<bool, &'static str> {
    with(|s| {
        let start = phys & !(PAGE_SIZE - 1);
        let had = s
            .domains
            .get(&pid)
            .is_some_and(|d| d.regions.contains_key(&start));
        s.map_range(pid, phys, len).map(|()| !had)
    })
    .unwrap_or(Ok(false))
}

/// Removes the region starting at `phys` from the domain of `pid` only:
/// the process unmapped a client's buffer, which its devices must no
/// longer reach (audit 2026-10-25 C, L11). Other domains keep it.
pub fn unmap_region(pid: u32, phys: u64) {
    let start = phys & !(PAGE_SIZE - 1);
    with(|s| {
        let Some(dom) = s.domains.get_mut(&pid) else {
            return;
        };
        let Some(len) = dom.regions.remove(&start) else {
            return;
        };
        for page in (start..start + len).step_by(PAGE_SIZE as usize) {
            let _ = dom.tables.unmap_4k(page);
        }
        for u in State::units_of(dom) {
            s.units[u].invalidate_iotlb();
        }
    });
}

/// Removes a region from every domain that has it (the DMA buffer is being
/// freed: no device must be able to keep writing to it).
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

/// Destroys the domain of a finished process: its devices can no longer
/// access memory and the tables go back to the PMM.
pub fn release(pid: u32) {
    with(|s| {
        let Some(dom) = s.domains.remove(&pid) else {
            return;
        };
        let flush = s.flush;
        for &(unit, source) in &dom.devices {
            // Only the contexts that still belong to this domain: a function
            // reassigned to another process keeps its own.
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
            pmm::with(|p| p.free(Frame::from_addr(f))).expect("domain table");
        });
        s.free_dids.push(did);
        log::debug!(
            "iommu: domain {did} of process {pid} freed ({devices} device(s), {frames} tables)"
        );
    });
}

/// Fault interrupt of a unit: reports and clears each record.
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
                    // Charged to the share of the process whose domain has
                    // the device: a driver making it fault in a loop
                    // overwrites only its own events (incident 0040).
                    let owner = s.owners.get(&f.source_id).copied().unwrap_or(0);
                    crate::events::record_caused(
                        owner,
                        aricode_abi::sysinfo::EventKind::DmaFault,
                        owner,
                        f.source_id as u64,
                        aricode_iommu::fault_reason_name(f.reason),
                    );
                    warn!(
                        "iommu: DMA fault from {:02x}:{:02x}.{} ({} at {:#x}): {} (reason {:#x}, unit {i})",
                        f.bus(),
                        f.device(),
                        f.function(),
                        if f.is_read { "read" } else { "write" },
                        f.address,
                        aricode_iommu::fault_reason_name(f.reason),
                        f.reason
                    );
                    u.w32(off + 12, 1 << 31);
                }
            }
            if status & fsts::PRIMARY_FAULT_OVERFLOW != 0 {
                warn!("iommu: unit {i}: fault registers overflowed");
            }
            u.w32(regs::FAULT_STATUS, status & fsts::CLEAR_MASK);
            let _ = fectl::INTERRUPT_MASK;
        }
    });
}

/// Self-test (`test=iommu`): a ring 3 test driver receives the virtio-blk,
/// reads a sector into its (translated) buffer and then asks the device to
/// write to memory that is not its own: the unit blocks it, records it and
/// the process checks it with `system_info`.
pub fn selftest() {
    use crate::object::{KernelObject, ObjectKind};
    use crate::user::{process, programs};
    use aricode_handles::Rights;

    assert!(
        enabled(),
        "iommu: no VT-d unit (QEMU needs -device intel-iommu)"
    );
    let index = crate::pci::find(0x1af4, 0x1042).expect("modern virtio-blk on the PCI bus");
    let claimed = crate::pci::claim(index).expect("claim virtio-blk");
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
    // A client's buffer, handed over last: the test driver maps it, lets the
    // device read into it, unmaps it (the device still reaches it while the
    // handle is held), closes the handle and tries again, which must fault
    // and leave the 0xA5 it wrote (audit 2026-10-25 C, L11).
    let client = crate::hw::dma_alloc(4096).expect("client buffer");
    let client_phys = match &client.kind {
        ObjectKind::Dma(d) => d.region.phys,
        _ => unreachable!("dma_alloc makes DMA objects"),
    };
    extra.push((client.clone(), Rights::READ.union(Rights::WRITE)));
    // The memory the test driver targets (0x1000, reserved) must not change
    // by a single byte.
    const FOREIGN_PHYS: u64 = 0x1000;
    let snapshot = |len: usize| -> Vec<u8> {
        // SAFETY: reserved low memory, reachable through the physical map.
        unsafe { core::slice::from_raw_parts(phys::phys_to_virt(FOREIGN_PHYS) as *const u8, len) }
            .to_vec()
    };
    let foreign_before = snapshot(512);
    let before = faults();
    let pid = process::spawn_with_handles_and_arg(
        "dma_rogue",
        programs::by_name("dma_rogue").expect("dma_rogue in the initrd"),
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
    assert!(recorded >= 1, "the unit did not record the illegal DMA");
    assert_eq!(
        snapshot(512),
        foreign_before,
        "the device wrote to memory outside its domain"
    );
    // SAFETY: the client buffer, still held by `client`.
    let tail = |off: u64| unsafe {
        (phys::phys_to_virt(client_phys + off + 510) as *const [u8; 2]).read()
    };
    assert_eq!(
        tail(1024),
        [0x55, 0xAA],
        "the device must reach a client buffer whose handle its driver holds"
    );
    assert_eq!(
        tail(2048),
        [0xA5, 0xA5],
        "the device wrote to a client buffer its driver had given back"
    );
    drop(client);
    let (_, domains, _) = stats();
    assert_eq!(
        domains, 0,
        "the driver domain must be freed with it ({domains} left)"
    );

    // Reassignment (audit 3eb4965, finding 2): the function moves from
    // domain A to B (transfer of the MMIO capability); A finishes and B keeps
    // its context; when B finishes the context goes away.
    let (bus, dev, func) = crate::pci::functions_of_class(0x01)
        .into_iter()
        .next()
        .expect("a storage function (virtio-blk)");
    let sid = aricode_iommu::source_id(bus, dev, func);
    const A: u32 = 1001;
    const B: u32 = 1002;
    attach(A, sid).expect("attach A");
    let did_a = context_domain(sid).expect("context of A present");
    attach(B, sid).expect("attach B");
    let did_b = context_domain(sid).expect("context of B present");
    assert_ne!(did_a, did_b, "B must have its own domain");
    release(A);
    assert_eq!(
        context_domain(sid),
        Some(did_b),
        "releasing A must not touch the context that now belongs to B"
    );
    release(B);
    assert_eq!(context_domain(sid), None, "releasing B removes the context");
    let (_, domains, _) = stats();
    assert_eq!(domains, 0);
    info!(
        "iommu selftest ok (legitimate read translated, DMA to foreign memory and to an unmapped client buffer blocked with {recorded} fault(s) recorded and memory intact, domain freed, reassignment of {bus:02x}:{dev:02x}.{func} kept)"
    );
}
