//! Enumeración PCIe por ECAM (Fase 17). El bus se recorre desde el bus
//! inicial de la MCFG siguiendo los puentes.

use crate::memory::vmm;
use alloc::vec::Vec;
use aricode_pci::{bars, cap_name, capabilities, Address, Bar, Header};
use aricode_x86_paging::{CACHE_DISABLE, GLOBAL, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
use log::{info, warn};

/// Ventana virtual del ECAM (hasta 256 MiB: 256 buses).
const ECAM_VIRT: u64 = 0xFFFF_D100_0000_0000;
const BUS_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub address: Address,
    pub header: Header,
    pub bars: Vec<(usize, Bar)>,
    pub capability_ids: Vec<u8>,
}

struct Ecam {
    bus_start: u8,
    bus_end: u8,
}

/// Copia los 256 bytes de configuración de una función (lecturas de 32 bits).
fn read_config(ecam: &Ecam, addr: Address) -> [u8; 256] {
    let base = ECAM_VIRT + addr.ecam_offset(ecam.bus_start);
    let mut cfg = [0u8; 256];
    for off in (0..256).step_by(4) {
        // SAFETY: dentro de la ventana ECAM mapeada en `init`; lectura de
        // 32 bits alineada, como exige el espacio de configuración.
        let v = unsafe { ((base + off as u64) as *const u32).read_volatile() };
        cfg[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    cfg
}

/// Mapea el ECAM y enumera todas las funciones alcanzables. El device manager
/// (Fase 18) consumirá esta lista para repartir capabilities a los drivers.
pub fn init() -> Vec<PciDevice> {
    let Some(entry) = crate::acpi::with(|a| a.mcfg.first().copied()) else {
        warn!("pci: sin MCFG, enumeracion omitida");
        return Vec::new();
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
    .expect("pci: mapear ECAM");
    let ecam = Ecam {
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
                found.push(PciDevice {
                    address,
                    header,
                    bars: bars(&cfg),
                    capability_ids: capabilities(&cfg).iter().map(|c| c.id).collect(),
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
            "pci: {} {:04x}:{:04x} clase {:02x}.{:02x} {} ({} BAR{}{})",
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
        "pci: {} funciones en el segmento {} (ECAM @ {:#x})",
        found.len(),
        entry.segment,
        entry.base
    );
    found
}
