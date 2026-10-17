// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Locating and parsing the static ACPI tables through the physical map
//! (`libs/acpi`). No AML (section 31).

use crate::memory::phys;
use crate::sync::IrqLock;
use alloc::vec::Vec;
use aricode_acpi::{root_entries, Dmar, Fadt, Madt, McfgEntry, Rsdp, SdtHeader};
use boot_protocol::BootInfo;
use log::{info, warn};

#[derive(Default)]
pub struct AcpiInfo {
    pub madt: Madt,
    pub mcfg: Vec<McfgEntry>,
    /// DMA remapping units (VT-d), if the platform declares them.
    pub dmar: Option<Dmar>,
    /// FADT fields for powering off, and the `\_S5_` sleep types found in
    /// the DSDT (`None`: no ACPI power-off on this machine).
    pub fadt: Option<Fadt>,
    pub s5: Option<(u8, u8)>,
}

static ACPI: IrqLock<Option<AcpiInfo>> = IrqLock::new(None);

/// Slice of `len` bytes at physical `phys`, if it lies within the linear map.
fn phys_slice(addr: u64, len: usize) -> Option<&'static [u8]> {
    if addr == 0 || addr.checked_add(len as u64)? > phys::limit() {
        return None;
    }
    // SAFETY: range within the linear physical map; the ACPI tables are
    // read-only and live in memory the kernel never frees.
    Some(unsafe { core::slice::from_raw_parts(phys::phys_to_virt(addr) as *const u8, len) })
}

/// Whole table (header + body) at `addr`.
fn table_at(addr: u64) -> Option<(&'static [u8], SdtHeader)> {
    let head = SdtHeader::parse(phys_slice(addr, SdtHeader::LEN)?).ok()?;
    let full = phys_slice(addr, head.length as usize)?;
    Some((full, head))
}

pub fn init(info: &BootInfo) {
    let Some(rsdp_bytes) = phys_slice(info.rsdp_address, Rsdp::MAX_LEN) else {
        warn!("acpi: no RSDP");
        return;
    };
    let rsdp = match Rsdp::parse(rsdp_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!("acpi: invalid RSDP ({e:?})");
            return;
        }
    };
    let (root, is_xsdt) = rsdp.root();
    let Some((root_table, _)) = table_at(root) else {
        warn!("acpi: root table unreachable at {root:#x}");
        return;
    };
    let entries = match root_entries(root_table, is_xsdt) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "acpi: invalid {} ({e:?})",
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
                Err(e) => warn!("acpi: invalid MADT ({e:?})"),
            },
            b"MCFG" => match aricode_acpi::parse_mcfg(table) {
                Ok(m) => acpi.mcfg = m,
                Err(e) => warn!("acpi: invalid MCFG ({e:?})"),
            },
            b"DMAR" => match Dmar::parse(table) {
                Ok(d) => acpi.dmar = Some(d),
                Err(e) => warn!("acpi: invalid DMAR ({e:?})"),
            },
            b"FACP" => match aricode_acpi::parse_fadt(table) {
                Ok(f) => acpi.fadt = Some(f),
                Err(e) => warn!("acpi: invalid FADT ({e:?})"),
            },
            b"SSDT" if acpi.s5.is_none() => acpi.s5 = aricode_acpi::find_s5(table),
            _ => {}
        }
    }
    // \_S5_ normally lives in the DSDT, which only the FADT points to.
    if let Some(f) = acpi.fadt {
        if let Some((dsdt, _)) = table_at(f.dsdt) {
            if let Some(s5) = aricode_acpi::find_s5(dsdt) {
                acpi.s5 = Some(s5);
            }
        }
        match acpi.s5 {
            Some((a, b)) => info!(
                "acpi: power-off through S5 (SLP_TYP {a}/{b}, PM1a {:#x}, PM1b {:#x})",
                f.pm1a_cnt, f.pm1b_cnt
            ),
            None => warn!("acpi: no \\_S5_ package; shutdown will only halt the CPU"),
        }
    }
    info!(
        "acpi: RSDP rev {} -> {} with {} tables [{}]",
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
            "acpi: MCFG ecam @ {:#x} segment {} buses {}..={}",
            e.base, e.segment, e.bus_start, e.bus_end
        );
    }
    if let Some(d) = &acpi.dmar {
        info!(
            "acpi: DMAR {} bits, {} unit(s) {:?}, {} reserved region(s)",
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

/// Access to the ACPI information (empty if it could not be read).
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
        fadt: None,
        s5: None,
    };
    let guard = ACPI.lock();
    f(guard.as_ref().unwrap_or(&EMPTY))
}
