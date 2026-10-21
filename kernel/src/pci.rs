// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! PCIe enumeration through ECAM (Phase 17) and device claiming for user
//! drivers (Phase 18): BAR probing, MSI-X and capability hand-over.

use crate::memory::vmm;
use crate::object::{KernelObject, ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use alloc::vec::Vec;
use aricode_handles::Rights;
use aricode_pci::{
    bars, cap_name, capabilities, device_control_after_flr, flr_method, pcie_saved_controls,
    Address, Bar, Flr, Header, SavedHeader, CAP_MSI, CAP_MSIX, COMMAND_BUS_MASTER,
    COMMAND_INTX_DISABLE, COMMAND_MEMORY, FLR_WAIT_MS,
};
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
use core::sync::atomic::{AtomicBool, Ordering};
use log::{info, warn};

/// Virtual ECAM window (up to 256 MiB: 256 buses).
const ECAM_VIRT: u64 = 0xFFFF_D100_0000_0000;
const BUS_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub address: Address,
    pub header: Header,
    pub bars: Vec<(usize, Bar)>,
    pub capability_ids: Vec<u8>,
    /// Offset of the MSI-X capability in configuration space.
    pub msix_cap: Option<u8>,
    /// Offset of the MSI capability (used if there is no MSI-X).
    pub msi_cap: Option<u8>,
    /// Configuration object handed to the current driver: while it lives
    /// (some handle refers to it) the function is claimed; when the driver
    /// dies and its handles go away, init can claim it again to restart it
    /// (stage 1: service recovery).
    pub claimed: Option<alloc::sync::Weak<KernelObject>>,
    /// Function Level Reset the function advertises, if any.
    pub flr: Option<Flr>,
    /// The header an FLR clears, as the firmware left it at boot.
    pub boot_header: SavedHeader,
    /// Claims handed out so far: a claim token only acts on the claim it
    /// belongs to, never on a newer one.
    pub generation: u64,
    /// FLR started when its last driver went away (incident 0004, H2): the
    /// next claim waits it out and restores the header first.
    pub reset: Option<PendingReset>,
}

/// An FLR in progress: when it started and the PCI Express control
/// registers it clears (offset from the PCIe capability, value), saved
/// just before.
#[derive(Debug, Clone)]
pub struct PendingReset {
    started_ms: u64,
    controls: Vec<(u16, u16)>,
}

/// The claim itself, inside the configuration object `claim` hands out:
/// when the last handle to that object goes, however it goes (the driver
/// dies, closes it, or it is lost in a message nobody receives), the
/// function stops bus mastering (incident 0004, H2).
pub struct ClaimToken {
    index: usize,
    generation: u64,
}

impl Drop for ClaimToken {
    fn drop(&mut self) {
        claim_dropped(self.index, self.generation);
    }
}

/// `pci.flr=off` turns Function Level Resets off (recovery knob for real
/// hardware, ADR 0038).
static FLR_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_flr_enabled(on: bool) {
    FLR_ENABLED.store(on, Ordering::Relaxed);
}

impl PciDevice {
    /// `true` if a live driver holds the function.
    pub fn is_claimed(&self) -> bool {
        self.claimed.as_ref().is_some_and(|w| w.strong_count() > 0)
    }
}

struct Ecam {
    base: u64,
    bus_start: u8,
    bus_end: u8,
}

static ECAM: IrqLock<Option<Ecam>> = IrqLock::new(None);
static DEVICES: IrqLock<Vec<PciDevice>> = IrqLock::new(Vec::new());

fn config_ptr(ecam: &Ecam, addr: Address, off: u64) -> u64 {
    ECAM_VIRT + addr.ecam_offset(ecam.bus_start) + off
}

fn read_u32(ecam: &Ecam, addr: Address, off: u64) -> u32 {
    // SAFETY: within the ECAM window mapped in `init`; aligned access.
    unsafe { (config_ptr(ecam, addr, off) as *const u32).read_volatile() }
}

fn write_u32(ecam: &Ecam, addr: Address, off: u64, v: u32) {
    // SAFETY: same as `read_u32`; device configuration write.
    unsafe { (config_ptr(ecam, addr, off) as *mut u32).write_volatile(v) }
}

fn read_u8(ecam: &Ecam, addr: Address, off: u64) -> u8 {
    // SAFETY: same as `read_u32`; byte access.
    unsafe { (config_ptr(ecam, addr, off) as *const u8).read_volatile() }
}

fn write_u8(ecam: &Ecam, addr: Address, off: u64, v: u8) {
    // SAFETY: same as `read_u32`; device configuration write.
    unsafe { (config_ptr(ecam, addr, off) as *mut u8).write_volatile(v) }
}

fn read_u16(ecam: &Ecam, addr: Address, off: u64) -> u16 {
    // SAFETY: same as `read_u32`; aligned 16-bit access.
    unsafe { (config_ptr(ecam, addr, off) as *const u16).read_volatile() }
}

fn write_u16(ecam: &Ecam, addr: Address, off: u64, v: u16) {
    // SAFETY: same as above.
    unsafe { (config_ptr(ecam, addr, off) as *mut u16).write_volatile(v) }
}

/// Copies a function's 256 configuration bytes (32-bit reads).
fn read_config(ecam: &Ecam, addr: Address) -> [u8; 256] {
    let mut cfg = [0u8; 256];
    for off in (0..256).step_by(4) {
        cfg[off..off + 4].copy_from_slice(&read_u32(ecam, addr, off as u64).to_le_bytes());
    }
    cfg
}

/// Maps the ECAM and enumerates every reachable function. The list is then
/// available to `pci_claim` through `init`'s PCI root.
pub fn init() {
    let Some(entry) = crate::acpi::with(|a| a.mcfg.first().copied()) else {
        warn!("pci: no MCFG, enumeration skipped");
        return;
    };
    let buses = entry.bus_end as u64 - entry.bus_start as u64 + 1;
    vmm::with(|m| {
        m.map_range_2m(
            ECAM_VIRT,
            entry.base,
            buses * BUS_BYTES,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .expect("pci: map ECAM");
    let ecam = Ecam {
        base: entry.base,
        bus_start: entry.bus_start,
        bus_end: entry.bus_end,
    };

    let mut found = Vec::new();
    let mut pending = alloc::vec![entry.bus_start];
    let mut visited = [false; 256];
    while let Some(bus) = pending.pop() {
        if visited[bus as usize] || bus > ecam.bus_end {
            continue;
        }
        visited[bus as usize] = true;
        for device in 0..32u8 {
            let first = read_config(
                &ecam,
                Address {
                    segment: entry.segment,
                    bus,
                    device,
                    function: 0,
                },
            );
            let Some(h0) = Header::parse(&first) else {
                continue;
            };
            let functions = if h0.multifunction { 8 } else { 1 };
            for function in 0..functions {
                let address = Address {
                    segment: entry.segment,
                    bus,
                    device,
                    function,
                };
                let cfg = if function == 0 {
                    first
                } else {
                    read_config(&ecam, address)
                };
                let Some(header) = Header::parse(&cfg) else {
                    continue;
                };
                if let Some(secondary) = Header::secondary_bus(&cfg) {
                    pending.push(secondary);
                }
                let caps = capabilities(&cfg);
                found.push(PciDevice {
                    address,
                    header,
                    bars: bars(&cfg),
                    capability_ids: caps.iter().map(|c| c.id).collect(),
                    msix_cap: caps.iter().find(|c| c.id == CAP_MSIX).map(|c| c.offset),
                    msi_cap: caps.iter().find(|c| c.id == CAP_MSI).map(|c| c.offset),
                    claimed: None,
                    flr: flr_method(&cfg),
                    boot_header: SavedHeader::from_config(&cfg).expect("256 configuration bytes"),
                    generation: 0,
                    reset: None,
                });
            }
        }
    }
    for d in &found {
        let mut caps: Vec<&str> = d
            .capability_ids
            .iter()
            .map(|&id| cap_name(id))
            .filter(|n| *n != "?")
            .collect();
        if d.flr.is_some() {
            caps.push("FLR");
        }
        info!(
            "pci: {} {:04x}:{:04x} class {:02x}.{:02x} {} ({} BAR{}{})",
            d.address,
            d.header.vendor,
            d.header.device,
            d.header.class,
            d.header.subclass,
            d.header.class_name(),
            d.bars.len(),
            if d.bars.len() == 1 { "" } else { "s" },
            if caps.is_empty() {
                alloc::string::String::new()
            } else {
                alloc::format!(", {}", caps.join("/"))
            }
        );
    }
    info!(
        "pci: {} functions in segment {} (ECAM @ {:#x})",
        found.len(),
        entry.segment,
        ecam.base
    );
    *DEVICES.lock() = found;
    *ECAM.lock() = Some(ecam);
}

pub fn count() -> usize {
    DEVICES.lock().len()
}

/// Functions of the given class (`bus, device, function`).
pub fn functions_of_class(class: u8) -> Vec<(u8, u8, u8)> {
    DEVICES
        .lock()
        .iter()
        .filter(|d| d.header.class == class)
        .map(|d| (d.address.bus, d.address.device, d.address.function))
        .collect()
}

/// Index of the first unclaimed function of the given class.
pub fn find_class(class: u8, subclass: u8) -> Option<usize> {
    DEVICES
        .lock()
        .iter()
        .position(|d| d.header.class == class && d.header.subclass == subclass && !d.is_claimed())
}

/// Index of the first unclaimed `vendor:device` function.
pub fn find(vendor: u16, device: u16) -> Option<usize> {
    DEVICES
        .lock()
        .iter()
        .position(|d| d.header.vendor == vendor && d.header.device == device && !d.is_claimed())
}

/// Packed information (`aricode_abi::pci_info`) for function `index`.
pub fn info_packed(index: usize) -> Option<u64> {
    let devs = DEVICES.lock();
    let d = devs.get(index)?;
    Some(aricode_abi::pci_info::pack(
        d.header.vendor,
        d.header.device,
        d.header.class,
        d.header.subclass,
        d.address.bus,
        d.address.device,
        d.address.function,
    ))
}

/// Extended fields (`aricode_abi::pci_info::pack_ext`) for function `index`.
pub fn info_ext_packed(index: usize) -> Option<u64> {
    let devs = DEVICES.lock();
    let d = devs.get(index)?;
    Some(aricode_abi::pci_info::pack_ext(
        d.header.prog_if,
        d.header.revision,
        d.header.header_type,
    ))
}

/// Size of a memory BAR by probing (write ones, read, restore).
fn probe_bar_size(ecam: &Ecam, addr: Address, index: usize, is_64: bool) -> u64 {
    let off = 0x10 + 4 * index as u64;
    let orig_lo = read_u32(ecam, addr, off);
    write_u32(ecam, addr, off, u32::MAX);
    let probed_lo = read_u32(ecam, addr, off);
    write_u32(ecam, addr, off, orig_lo);
    if !is_64 {
        return aricode_pci::bar_size_from_probe(probed_lo, false) as u64;
    }
    let orig_hi = read_u32(ecam, addr, off + 4);
    write_u32(ecam, addr, off + 4, u32::MAX);
    let probed_hi = read_u32(ecam, addr, off + 4);
    write_u32(ecam, addr, off + 4, orig_hi);
    let mask = ((probed_hi as u64) << 32) | (probed_lo as u64 & !0xF);
    (!mask).wrapping_add(1)
}

/// Capabilities of a claimed device, in ABI order
/// (`aricode_abi::nr::PCI_CLAIM`).
pub struct Claimed {
    pub handles: [Option<(ObjectRef, Rights)>; aricode_abi::PCI_CLAIM_HANDLES],
}

/// Claims function `index` for a driver: enables memory space and turns
/// bus mastering OFF (ADR 0038: the driver turns it on with
/// `sys::pci::enable_bus_master` once it has reset its device, so a device
/// that a dead driver left running cannot write to memory reused since),
/// probes the BARs, programs MSI-X (entry 0) or, without MSI-X, MSI
/// (ADR 0022) and builds the capabilities. A function being reset is
/// waited for first.
pub fn claim(index: usize) -> Result<Claimed, &'static str> {
    loop {
        finish_reset(index)?;
        let ecam_guard = ECAM.lock();
        let ecam = ecam_guard.as_ref().ok_or("pci: no ECAM")?;
        let mut devs = DEVICES.lock();
        let d = devs.get_mut(index).ok_or("pci: index out of range")?;
        if d.reset.is_some() {
            // An FLR started after `finish_reset` looked: wait it out too.
            continue;
        }
        return claim_locked(index, ecam, d);
    }
}

fn claim_locked(index: usize, ecam: &Ecam, d: &mut PciDevice) -> Result<Claimed, &'static str> {
    if d.is_claimed() {
        return Err("pci: already claimed");
    }
    let reclaimed = d.claimed.is_some();
    let addr = d.address;

    // Command (16-bit write: the status bits are write-one-to-clear):
    // memory space on, INTx disabled, bus mastering off.
    let cmd = read_u16(ecam, addr, 0x04);
    write_u16(
        ecam,
        addr,
        0x04,
        (cmd & !COMMAND_BUS_MASTER) | COMMAND_MEMORY | COMMAND_INTX_DISABLE,
    );

    let mut handles: [Option<(ObjectRef, Rights)>; aricode_abi::PCI_CLAIM_HANDLES] =
        Default::default();
    let mmio_rights = Rights::READ.union(Rights::WRITE).union(Rights::TRANSFER);

    let source_id = Some(aricode_iommu::source_id(
        addr.bus,
        addr.device,
        addr.function,
    ));
    // 1: hardware capability (DMA).
    handles[1] = Some((
        KernelObject::new(ObjectKind::Hardware),
        Rights::MANAGE.union(Rights::TRANSFER),
    ));
    // 3..9: memory BARs with their size.
    for (bar_index, bar) in &d.bars {
        if let Bar::Memory { address, is_64, .. } = bar {
            let size = probe_bar_size(ecam, addr, *bar_index, *is_64);
            if size == 0 || *address == 0 {
                continue;
            }
            handles[3 + bar_index] = Some((
                KernelObject::new(ObjectKind::Mmio(crate::hw::Region {
                    phys: *address,
                    len: size,
                    cacheable: false,
                    source_id,
                    claim: None,
                })),
                mmio_rights,
            ));
        }
    }
    // 2: IRQ via MSI-X (entry 0) or, if the function has no MSI-X, via MSI.
    let mut irq_kind: Option<&str> = None;
    if let Some(cap) = d.msix_cap {
        let cap = cap as u64;
        let table = read_u32(ecam, addr, cap + 4);
        let bir = (table & 0x7) as usize;
        let table_offset = (table & !0x7) as u64;
        let bar_phys = d.bars.iter().find_map(|(i, b)| match b {
            Bar::Memory { address, .. } if *i == bir => Some(*address),
            _ => None,
        });
        if let Some(bar_phys) = bar_phys {
            let table_phys = bar_phys + table_offset;
            let irq = crate::hw::register_msix(table_phys, 0)?;
            // Enable MSI-X (bit 15) without masking the function (bit 14).
            let ctrl = read_u32(ecam, addr, cap) >> 16;
            write_u16(ecam, addr, cap + 2, (ctrl as u16 | 0x8000) & !0x4000);
            handles[2] = Some((
                irq,
                Rights::READ.union(Rights::WAIT).union(Rights::TRANSFER),
            ));
            irq_kind = Some("MSI-X entry 0");
        }
    }
    if irq_kind.is_none() {
        if let Some(cap) = d.msi_cap {
            let irq = crate::hw::register_msi(config_ptr(ecam, addr, 0), cap)?;
            handles[2] = Some((
                irq,
                Rights::READ.union(Rights::WAIT).union(Rights::TRANSFER),
            ));
            irq_kind = Some("MSI");
        }
    }
    // 0: configuration space (the function's ECAM page), carrying the
    // claim. Made last: nothing can fail after it, so it is never dropped
    // here, with the PCI locks held (its drop takes them).
    d.generation += 1;
    let cfg_phys = ecam.base + addr.ecam_offset(ecam.bus_start);
    let cfg = KernelObject::new(ObjectKind::Mmio(crate::hw::Region {
        phys: cfg_phys,
        len: 4096,
        cacheable: false,
        source_id,
        claim: Some(ClaimToken {
            index,
            generation: d.generation,
        }),
    }));
    d.claimed = Some(alloc::sync::Arc::downgrade(&cfg));
    handles[0] = Some((cfg, mmio_rights));
    info!(
        "pci: {} {:04x}:{:04x} {}{}{}{}",
        addr,
        d.header.vendor,
        d.header.device,
        if reclaimed {
            "claimed again (the previous driver died)"
        } else {
            "claimed"
        },
        irq_kind.map_or("", |_| " ("),
        irq_kind.unwrap_or(""),
        irq_kind.map_or("", |_| ")")
    );
    Ok(Claimed { handles })
}

fn now_ms() -> u64 {
    crate::time::uptime_us() / 1000
}

/// The configuration object of claim `generation` of function `index` is
/// gone: unless a newer claim exists, the function stops bus mastering.
/// Quiet when it already had (a reaped driver, `stop_claims_of`).
fn claim_dropped(index: usize, generation: u64) {
    let ecam_guard = ECAM.lock();
    let Some(ecam) = ecam_guard.as_ref() else {
        return;
    };
    let devs = DEVICES.lock();
    let Some(d) = devs.get(index) else { return };
    if d.generation != generation {
        return;
    }
    let cmd = read_u16(ecam, d.address, 0x04);
    if cmd & COMMAND_BUS_MASTER != 0 {
        write_u16(ecam, d.address, 0x04, cmd & !COMMAND_BUS_MASTER);
        info!("pci: {} claim released: bus mastering cleared", d.address);
    }
}

/// What `stop_claims_of` did.
#[derive(Default)]
pub struct Stopped {
    /// Functions whose bus mastering was turned off, for `reset_released`.
    pub functions: Vec<usize>,
    /// A function still reads back as bus master: the dead driver's DMA
    /// memory must never be reused.
    pub quarantine: bool,
}

/// First half of a driver's death (incident 0004, H2), before anything it
/// owned is freed: every function whose claim ends with `handles` (the
/// configuration object handed out by `claim` is held only there) stops
/// bus mastering, so the device can no longer read or write memory once
/// the dead driver's DMA buffers go back to the PMM. Without an IOMMU
/// nothing else stops it. Runs in `reap()` with interrupts disabled: one
/// configuration write per function, no waiting.
pub fn stop_claims_of(pid: u32, handles: &crate::object::Handles) -> Stopped {
    use aricode_abi::sysinfo::EventKind;
    let mut out = Stopped::default();
    let ecam_guard = ECAM.lock();
    let Some(ecam) = ecam_guard.as_ref() else {
        return out;
    };
    let devs = DEVICES.lock();
    for (cap, _) in handles.iter() {
        let obj = cap.object();
        if !matches!(&obj.kind, ObjectKind::Mmio(r) if r.claim.is_some()) {
            continue;
        }
        let Some(index) = devs.iter().position(|d| {
            d.claimed
                .as_ref()
                .is_some_and(|w| core::ptr::eq(w.as_ptr(), alloc::sync::Arc::as_ptr(obj)))
        }) else {
            continue;
        };
        if out.functions.contains(&index) {
            continue;
        }
        let addr = devs[index].address;
        let held_here = handles
            .iter()
            .filter(|(c, _)| alloc::sync::Arc::ptr_eq(c.object(), obj))
            .count();
        if alloc::sync::Arc::strong_count(obj) != held_here {
            // Someone else still holds the claim (a copy in flight, or a
            // process it was passed to): the device is theirs, and it
            // stops when their claim goes (`ClaimToken`).
            warn!(
                "pci: {addr} driver (process {pid}) gone but its claim is held elsewhere: bus mastering left on"
            );
            crate::events::record(EventKind::Fault, pid, index as u64, "claim held elsewhere");
            continue;
        }
        let cmd = read_u16(ecam, addr, 0x04);
        write_u16(ecam, addr, 0x04, cmd & !COMMAND_BUS_MASTER);
        let after = read_u16(ecam, addr, 0x04);
        if after & COMMAND_BUS_MASTER != 0 {
            warn!(
                "pci: {addr} driver (process {pid}) gone: bus mastering still on after clearing it (command {after:#06x}); its DMA memory is quarantined"
            );
            crate::events::record(EventKind::Fault, pid, after as u64, "bus master stuck on");
            out.quarantine = true;
        } else {
            info!(
                "pci: {addr} driver (process {pid}) gone: bus mastering cleared before its memory is freed (command {cmd:#06x} -> {after:#06x})"
            );
        }
        out.functions.push(index);
    }
    out
}

/// Second half, once the dead driver's capabilities are released: a
/// function stopped by `stop_claims_of` that nobody has claimed since and
/// that advertises FLR is reset, so the next driver finds it in its
/// power-on state. Only the reset is started here (interrupts are
/// disabled); the next `claim` waits out `FLR_WAIT_MS` and restores the
/// header. A function with transactions still pending after
/// `PENDING_WAIT_US` is not reset (bus mastering is already off).
pub fn reset_released(pid: u32, stopped: &[usize]) {
    use aricode_abi::sysinfo::EventKind;
    const PENDING_WAIT_US: u64 = 1_000;
    let ecam_guard = ECAM.lock();
    let Some(ecam) = ecam_guard.as_ref() else {
        return;
    };
    let mut devs = DEVICES.lock();
    for &index in stopped {
        let Some(d) = devs.get_mut(index) else {
            continue;
        };
        let Some(flr) = d.flr else { continue };
        if d.is_claimed() || d.reset.is_some() {
            continue;
        }
        let addr = d.address;
        if !FLR_ENABLED.load(Ordering::Relaxed) {
            info!("pci: {addr} function level reset skipped (pci.flr=off)");
            continue;
        }
        let (status, pending) = flr.transactions_pending();
        let deadline = crate::time::uptime_us() + PENDING_WAIT_US;
        while read_u8(ecam, addr, status as u64) & pending != 0 {
            if crate::time::uptime_us() >= deadline {
                break;
            }
            core::hint::spin_loop();
        }
        if read_u8(ecam, addr, status as u64) & pending != 0 {
            warn!(
                "pci: {addr} function level reset skipped: transactions still pending after {PENDING_WAIT_US} us (bus mastering is off)"
            );
            crate::events::record(
                EventKind::Fault,
                pid,
                index as u64,
                "flr skipped: tx pending",
            );
            continue;
        }
        let mut controls = Vec::new();
        if let Flr::Pcie { cap } = flr {
            let cap = cap as u16;
            let caps = read_u16(ecam, addr, cap as u64 + 2);
            for &off in pcie_saved_controls(caps) {
                controls.push((off, read_u16(ecam, addr, (cap + off) as u64)));
            }
        }
        let reg = flr.control() as u64;
        write_u16(ecam, addr, reg, flr.initiate(read_u16(ecam, addr, reg)));
        d.reset = Some(PendingReset {
            started_ms: now_ms(),
            controls,
        });
        info!("pci: {addr} function level reset started (its driver is gone)");
    }
}

/// Before a function is claimed again: waits until an FLR started by
/// `reset_released` has had `FLR_WAIT_MS` and the function answers
/// configuration reads again (bounded, sleeping without locks), then puts
/// back the boot-time header and the saved PCI Express controls.
fn finish_reset(index: usize) -> Result<(), &'static str> {
    const READY_WAIT_MS: u64 = 1_000;
    let mut ready_deadline = None;
    loop {
        let (wait, addr) = {
            let devs = DEVICES.lock();
            let d = devs.get(index).ok_or("pci: index out of range")?;
            match &d.reset {
                None => return Ok(()),
                Some(r) => (
                    (r.started_ms + FLR_WAIT_MS).saturating_sub(now_ms()),
                    d.address,
                ),
            }
        };
        if wait > 0 {
            crate::task::scheduler::sleep_ms(wait);
            continue;
        }
        let vendor = {
            let ecam_guard = ECAM.lock();
            let ecam = ecam_guard.as_ref().ok_or("pci: no ECAM")?;
            read_u16(ecam, addr, 0)
        };
        if vendor != aricode_pci::VENDOR_NONE {
            break;
        }
        let deadline = *ready_deadline.get_or_insert(now_ms() + READY_WAIT_MS);
        if now_ms() >= deadline {
            warn!("pci: {addr} does not answer {READY_WAIT_MS} ms after its function level reset");
            crate::events::record(
                aricode_abi::sysinfo::EventKind::Fault,
                0,
                index as u64,
                "flr: function not ready",
            );
            return Err("pci: function not ready after its reset");
        }
        crate::task::scheduler::sleep_ms(10);
    }
    let ecam_guard = ECAM.lock();
    let ecam = ecam_guard.as_ref().ok_or("pci: no ECAM")?;
    let mut devs = DEVICES.lock();
    let d = devs.get_mut(index).ok_or("pci: index out of range")?;
    if let Some(r) = d.reset.take() {
        let h = d.boot_header;
        for (&off, value) in SavedHeader::BAR_OFFSETS.iter().zip(h.bars) {
            write_u32(ecam, d.address, off as u64, value);
        }
        for (&off, value) in SavedHeader::BYTE_OFFSETS.iter().zip(h.bytes) {
            write_u8(ecam, d.address, off as u64, value);
        }
        if let Some(Flr::Pcie { cap }) = d.flr {
            for (off, value) in r.controls {
                let value = if off == 0x08 {
                    device_control_after_flr(value)
                } else {
                    value
                };
                write_u16(ecam, d.address, (cap as u16 + off) as u64, value);
            }
        }
        info!(
            "pci: {} header restored after the function level reset",
            d.address
        );
    }
    Ok(())
}

/// Command register and the boot-time header fields an FLR clears, of
/// function `index` as they read now (`test=pci_reap`).
fn command_and_header(index: usize) -> (u16, SavedHeader) {
    let ecam_guard = ECAM.lock();
    let ecam = ecam_guard.as_ref().expect("ECAM");
    let addr = DEVICES.lock()[index].address;
    let mut cfg = [0u8; 0x40];
    for off in (0..0x40).step_by(4) {
        cfg[off..off + 4].copy_from_slice(&read_u32(ecam, addr, off as u64).to_le_bytes());
    }
    (
        read_u16(ecam, addr, 0x04),
        SavedHeader::from_config(&cfg).expect("0x40 bytes"),
    )
}

/// Self-test (`test=pci_reap`, incident 0004, H2): a driver turns bus
/// mastering on and dies holding a claimed function and a DMA buffer it
/// keeps only through its mapping. The function must lose bus mastering
/// when the driver is reaped (before the buffer is freed: the scenario
/// checks the order of the log lines), be reset if it advertises FLR, and
/// be claimable again with its boot-time header and bus mastering off (the
/// supervisor's restart path). Runs on the virtio-blk and on the first
/// function that advertises FLR (the scenario adds one). This checks the
/// kernel's bookkeeping and the command register; it cannot show a device
/// that keeps writing, which QEMU's devices do not do.
pub fn reap_selftest() {
    use crate::user::{process, programs};

    let blk = find_class(0x01, 0x00).expect("virtio-blk on the PCI bus");
    let flr = DEVICES
        .lock()
        .iter()
        .position(|d| d.flr.is_some() && !d.is_claimed())
        .expect("a function that advertises FLR (scenario device)");
    for index in [blk, flr] {
        let boot = DEVICES.lock()[index].boot_header;
        let claimed = claim(index).expect("claim");
        let (cmd, _) = command_and_header(index);
        assert!(
            cmd & COMMAND_BUS_MASTER == 0,
            "claim leaves bus mastering off"
        );
        let [cfg, hw, ..] = claimed.handles;
        let pid = process::spawn_with_handles(
            "pci_reaped",
            programs::by_name("pci_reaped").expect("pci_reaped in the initrd"),
            alloc::vec![cfg.expect("configuration"), hw.expect("hardware")],
        )
        .expect("pci_reaped");
        let status = process::wait(pid);
        assert!(
            matches!(status, process::ExitStatus::Killed(_)),
            "pci_reaped must die by its fault: {status:?}"
        );
        let (cmd, _) = command_and_header(index);
        let (claimed_now, reset) = {
            let devs = DEVICES.lock();
            (devs[index].is_claimed(), devs[index].reset.is_some())
        };
        assert!(
            !claimed_now,
            "the function is free once its driver is reaped"
        );
        assert!(
            cmd & COMMAND_BUS_MASTER == 0,
            "bus mastering must be off once the driver is reaped (command {cmd:#06x})"
        );
        assert_eq!(
            reset,
            index == flr,
            "an FLR is started exactly for the function that advertises it"
        );
        let again = claim(index).expect("claim again after the driver died");
        let (cmd, header) = command_and_header(index);
        assert!(
            cmd & COMMAND_BUS_MASTER == 0,
            "the new claim leaves bus mastering off for the new driver"
        );
        assert_eq!(header, boot, "the boot-time header is back");
        // Dropping the claim here (no process) goes through `ClaimToken`.
        drop(again);
    }
    let addr = |i: usize| DEVICES.lock()[i].address;
    info!(
        "pci reap selftest ok ({} and {}: bus mastering cleared on driver reap, {} reset with FLR, both claimed again with their boot header and bus mastering off)",
        addr(blk),
        addr(flr),
        addr(flr)
    );
}
