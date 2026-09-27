//! Localización y parseo de las tablas ACPI estáticas a través del mapa
//! físico (`libs/acpi`). Sin AML (sección 31).

use crate::memory::phys;
use crate::sync::IrqLock;
use alloc::vec::Vec;
use aricode_acpi::{root_entries, Dmar, Madt, McfgEntry, Rsdp, SdtHeader};
use boot_protocol::BootInfo;
use log::{info, warn};

#[derive(Default)]
pub struct AcpiInfo {
    pub madt: Madt,
    pub mcfg: Vec<McfgEntry>,
    /// Unidades de remapeo DMA (VT-d), si la plataforma las declara.
    pub dmar: Option<Dmar>,
}

static ACPI: IrqLock<Option<AcpiInfo>> = IrqLock::new(None);

/// Slice de `len` bytes en la física `phys`, si cae dentro del mapa lineal.
fn phys_slice(addr: u64, len: usize) -> Option<&'static [u8]> {
    if addr == 0 || addr.checked_add(len as u64)? > phys::limit() {
        return None;
    }
    // SAFETY: rango dentro del mapa físico lineal; las tablas ACPI son de
    // solo lectura y viven en memoria que el kernel nunca libera.
    Some(unsafe { core::slice::from_raw_parts(phys::phys_to_virt(addr) as *const u8, len) })
}

/// Tabla completa (cabecera + cuerpo) en `addr`.
fn table_at(addr: u64) -> Option<(&'static [u8], SdtHeader)> {
    let head = SdtHeader::parse(phys_slice(addr, SdtHeader::LEN)?).ok()?;
    let full = phys_slice(addr, head.length as usize)?;
    Some((full, head))
}

pub fn init(info: &BootInfo) {
    let Some(rsdp_bytes) = phys_slice(info.rsdp_address, Rsdp::MAX_LEN) else {
        warn!("acpi: sin RSDP");
        return;
    };
    let rsdp = match Rsdp::parse(rsdp_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!("acpi: RSDP invalido ({e:?})");
            return;
        }
    };
    let (root, is_xsdt) = rsdp.root();
    let Some((root_table, _)) = table_at(root) else {
        warn!("acpi: tabla raiz inaccesible en {root:#x}");
        return;
    };
    let entries = match root_entries(root_table, is_xsdt) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "acpi: {} invalida ({e:?})",
                if is_xsdt { "XSDT" } else { "RSDT" }
            );
            return;
        }
    };
    let mut acpi = AcpiInfo::default();
    let mut names: Vec<alloc::string::String> = Vec::new();
    for addr in entries {
        let Some((table, head)) = table_at(addr) else {
            continue;
        };
        names.push(alloc::string::String::from(head.signature_str()));
        match &head.signature {
            b"APIC" => match Madt::parse(table) {
                Ok(m) => acpi.madt = m,
                Err(e) => warn!("acpi: MADT invalida ({e:?})"),
            },
            b"MCFG" => match aricode_acpi::parse_mcfg(table) {
                Ok(m) => acpi.mcfg = m,
                Err(e) => warn!("acpi: MCFG invalida ({e:?})"),
            },
            b"DMAR" => match Dmar::parse(table) {
                Ok(d) => acpi.dmar = Some(d),
                Err(e) => warn!("acpi: DMAR invalida ({e:?})"),
            },
            _ => {}
        }
    }
    info!(
        "acpi: RSDP rev {} -> {} con {} tablas [{}]",
        rsdp.revision,
        if is_xsdt { "XSDT" } else { "RSDT" },
        names.len(),
        names.join(" ")
    );
    info!(
        "acpi: MADT lapic @ {:#x}, {} cpu(s), {} ioapic(s) {:?}, overrides {:?}",
        acpi.madt.lapic_address,
        acpi.madt.lapic_ids.len(),
        acpi.madt.io_apics.len(),
        acpi.madt
            .io_apics
            .iter()
            .map(|i| (i.id, i.address, i.gsi_base))
            .collect::<Vec<_>>(),
        acpi.madt
            .overrides
            .iter()
            .map(|o| (o.irq, o.gsi, o.flags))
            .collect::<Vec<_>>()
    );
    for e in &acpi.mcfg {
        info!(
            "acpi: MCFG ecam @ {:#x} segmento {} buses {}..={}",
            e.base, e.segment, e.bus_start, e.bus_end
        );
    }
    if let Some(d) = &acpi.dmar {
        info!(
            "acpi: DMAR {} bits, {} unidad(es) {:?}, {} region(es) reservadas",
            d.host_address_width,
            d.units.len(),
            d.units
                .iter()
                .map(|u| (u.register_base, u.include_all))
                .collect::<Vec<_>>(),
            d.reserved.len()
        );
    }
    *ACPI.lock() = Some(acpi);
}

/// Acceso a la información ACPI (vacía si no se pudo leer).
pub fn with<R>(f: impl FnOnce(&AcpiInfo) -> R) -> R {
    static EMPTY: AcpiInfo = AcpiInfo {
        madt: Madt {
            lapic_address: 0,
            lapic_ids: Vec::new(),
            io_apics: Vec::new(),
            overrides: Vec::new(),
        },
        mcfg: Vec::new(),
        dmar: None,
    };
    let guard = ACPI.lock();
    f(guard.as_ref().unwrap_or(&EMPTY))
}
