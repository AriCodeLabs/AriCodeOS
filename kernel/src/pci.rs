//! PCIe enumeration through ECAM (Phase 17) and device claiming for user
//! drivers (Phase 18): BAR probing, MSI-X and capability hand-over.

use crate::memory::vmm;
use crate::object::{KernelObject, ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use alloc::vec::Vec;
use aricode_handles::Rights;
use aricode_pci::{bars, cap_name, capabilities, Address, Bar, Header, CAP_MSI, CAP_MSIX};
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
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
                });
            }
        }
    }
    for d in &found {
        let caps: Vec<&str> = d
            .capability_ids
            .iter()
            .map(|&id| cap_name(id))
            .filter(|n| *n != "?")
            .collect();
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

/// Claims function `index` for a driver: enables memory and bus master,
/// probes the BARs, programs MSI-X (entry 0) or, without MSI-X, MSI
/// (ADR 0022) and builds the capabilities.
pub fn claim(index: usize) -> Result<Claimed, &'static str> {
    let ecam_guard = ECAM.lock();
    let ecam = ecam_guard.as_ref().ok_or("pci: no ECAM")?;
    let mut devs = DEVICES.lock();
    let d = devs.get_mut(index).ok_or("pci: index out of range")?;
    if d.is_claimed() {
        return Err("pci: already claimed");
    }
    let reclaimed = d.claimed.is_some();
    let addr = d.address;

    // Command: memory space (bit 1) + bus master (bit 2); INTx disabled (bit 10).
    let cmd = read_u32(ecam, addr, 0x04);
    write_u32(
        ecam,
        addr,
        0x04,
        (cmd & 0xFFFF_0000) | (cmd & 0xFFFF) | 0x0006 | 0x0400,
    );

    let mut handles: [Option<(ObjectRef, Rights)>; aricode_abi::PCI_CLAIM_HANDLES] =
        Default::default();
    let mmio_rights = Rights::READ.union(Rights::WRITE).union(Rights::TRANSFER);

    let source_id = Some(aricode_iommu::source_id(
        addr.bus,
        addr.device,
        addr.function,
    ));
    // 0: configuration space (the function's ECAM page).
    let cfg_phys = ecam.base + addr.ecam_offset(ecam.bus_start);
    let cfg = KernelObject::new(ObjectKind::Mmio(crate::hw::Region {
        phys: cfg_phys,
        len: 4096,
        cacheable: false,
        source_id,
    }));
    d.claimed = Some(alloc::sync::Arc::downgrade(&cfg));
    handles[0] = Some((cfg, mmio_rights));
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
