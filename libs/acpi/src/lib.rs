//! Static ACPI tables (section 31): no AML. Everything works on byte slices
//! the kernel gets from the physical map, so it is tested on the host with
//! synthetic tables.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    BadSignature,
    BadChecksum,
    Truncated,
    Unsupported,
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

/// Root System Description Pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsdp {
    pub revision: u8,
    pub rsdt_address: u32,
    /// Only in revision >= 2.
    pub xsdt_address: u64,
}

impl Rsdp {
    /// Maximum size to read in order to parse any revision.
    pub const MAX_LEN: usize = 36;

    pub fn parse(d: &[u8]) -> Result<Self, AcpiError> {
        if d.len() < 20 {
            return Err(AcpiError::Truncated);
        }
        if &d[..8] != b"RSD PTR " {
            return Err(AcpiError::BadSignature);
        }
        if !checksum_ok(&d[..20]) {
            return Err(AcpiError::BadChecksum);
        }
        let revision = d[15];
        let rsdt_address = u32_at(d, 16);
        if revision < 2 {
            return Ok(Self {
                revision,
                rsdt_address,
                xsdt_address: 0,
            });
        }
        if d.len() < 36 {
            return Err(AcpiError::Truncated);
        }
        let length = u32_at(d, 20) as usize;
        if length < 36 || !checksum_ok(&d[..length.min(d.len())]) {
            return Err(AcpiError::BadChecksum);
        }
        Ok(Self {
            revision,
            rsdt_address,
            xsdt_address: u64_at(d, 24),
        })
    }

    /// Address of the preferred root table and whether it is the XSDT (64 bits).
    pub fn root(&self) -> (u64, bool) {
        if self.revision >= 2 && self.xsdt_address != 0 {
            (self.xsdt_address, true)
        } else {
            (self.rsdt_address as u64, false)
        }
    }
}

/// Header common to every table (36 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
}

impl SdtHeader {
    pub const LEN: usize = 36;

    /// Header only; does not check the checksum (that needs the whole table).
    pub fn parse(d: &[u8]) -> Result<Self, AcpiError> {
        if d.len() < Self::LEN {
            return Err(AcpiError::Truncated);
        }
        let length = u32_at(d, 4);
        if (length as usize) < Self::LEN {
            return Err(AcpiError::Truncated);
        }
        Ok(Self {
            signature: d[..4].try_into().unwrap(),
            length,
            revision: d[8],
        })
    }

    pub fn signature_str(&self) -> &str {
        core::str::from_utf8(&self.signature).unwrap_or("????")
    }
}

/// Checks that `table` is a complete table with `signature` and a valid checksum.
pub fn validate_table(table: &[u8], signature: &[u8; 4]) -> Result<SdtHeader, AcpiError> {
    let h = SdtHeader::parse(table)?;
    if &h.signature != signature {
        return Err(AcpiError::BadSignature);
    }
    if table.len() < h.length as usize {
        return Err(AcpiError::Truncated);
    }
    if !checksum_ok(&table[..h.length as usize]) {
        return Err(AcpiError::BadChecksum);
    }
    Ok(h)
}

/// Addresses of the tables listed by the XSDT (64 bits) or RSDT (32 bits).
pub fn root_entries(table: &[u8], is_xsdt: bool) -> Result<Vec<u64>, AcpiError> {
    let h = validate_table(table, if is_xsdt { b"XSDT" } else { b"RSDT" })?;
    let body = &table[SdtHeader::LEN..h.length as usize];
    let width = if is_xsdt { 8 } else { 4 };
    Ok(body
        .chunks_exact(width)
        .map(|c| {
            if is_xsdt {
                u64_at(c, 0)
            } else {
                u32_at(c, 0) as u64
            }
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoApic {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptOverride {
    pub bus: u8,
    pub irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

impl InterruptOverride {
    /// Active-low polarity according to the MPS INTI flags.
    pub fn active_low(&self) -> bool {
        self.flags & 0b11 == 0b11
    }
    /// Level-triggered according to the MPS INTI flags.
    pub fn level_triggered(&self) -> bool {
        (self.flags >> 2) & 0b11 == 0b11
    }
}

/// Multiple APIC Description Table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Madt {
    pub lapic_address: u32,
    pub lapic_ids: Vec<u8>,
    pub io_apics: Vec<IoApic>,
    pub overrides: Vec<InterruptOverride>,
}

impl Madt {
    pub fn parse(table: &[u8]) -> Result<Self, AcpiError> {
        let h = validate_table(table, b"APIC")?;
        let d = &table[..h.length as usize];
        if d.len() < 44 {
            return Err(AcpiError::Truncated);
        }
        let mut madt = Madt {
            lapic_address: u32_at(d, 36),
            ..Default::default()
        };
        let mut off = 44;
        while off + 2 <= d.len() {
            let (ty, len) = (d[off], d[off + 1] as usize);
            if len < 2 || off + len > d.len() {
                return Err(AcpiError::Truncated);
            }
            let e = &d[off..off + len];
            match ty {
                0 if len >= 8 => {
                    if u32_at(e, 4) & 1 != 0 {
                        madt.lapic_ids.push(e[3]);
                    }
                }
                1 if len >= 12 => madt.io_apics.push(IoApic {
                    id: e[2],
                    address: u32_at(e, 4),
                    gsi_base: u32_at(e, 8),
                }),
                2 if len >= 10 => madt.overrides.push(InterruptOverride {
                    bus: e[2],
                    irq: e[3],
                    gsi: u32_at(e, 4),
                    flags: u16_at(e, 8),
                }),
                _ => {}
            }
            off += len;
        }
        Ok(madt)
    }

    /// GSI of an ISA IRQ, applying overrides (identity if there are none).
    pub fn isa_irq_to_gsi(&self, irq: u8) -> (u32, Option<InterruptOverride>) {
        match self.overrides.iter().find(|o| o.irq == irq) {
            Some(o) => (o.gsi, Some(*o)),
            None => (irq as u32, None),
        }
    }
}

/// MCFG entry: one ECAM region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McfgEntry {
    pub base: u64,
    pub segment: u16,
    pub bus_start: u8,
    pub bus_end: u8,
}

pub fn parse_mcfg(table: &[u8]) -> Result<Vec<McfgEntry>, AcpiError> {
    let h = validate_table(table, b"MCFG")?;
    let d = &table[..h.length as usize];
    if d.len() < 44 {
        return Err(AcpiError::Truncated);
    }
    Ok(d[44..]
        .as_chunks::<16>()
        .0
        .iter()
        .map(|e| McfgEntry {
            base: u64_at(e, 0),
            segment: u16_at(e, 8),
            bus_start: e[10],
            bus_end: e[11],
        })
        .collect())
}

/// DMAR device scope: a PCI endpoint or bridge identified by its start bus
/// and a path (device, function) per bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceScope {
    /// 1 endpoint, 2 bridge, 3 I/O APIC, 4 HPET, 5 ACPI namespace.
    pub kind: u8,
    pub start_bus: u8,
    pub path: Vec<(u8, u8)>,
}

impl DeviceScope {
    /// `true` if it describes exactly the given PCI function (endpoint or bridge
    /// directly on `start_bus`; multi-hop paths cannot be resolved without
    /// the bridge topology).
    pub fn matches(&self, bus: u8, device: u8, function: u8) -> bool {
        matches!(self.kind, 1 | 2)
            && self.start_bus == bus
            && self.path.len() == 1
            && self.path[0] == (device, function)
    }
}

/// DMA remapping unit (DRHD).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drhd {
    pub register_base: u64,
    pub segment: u16,
    /// Covers every device of the segment not listed by another unit.
    pub include_all: bool,
    pub scopes: Vec<DeviceScope>,
}

/// Reserved memory region (RMRR): physical range some devices keep
/// accessing through DMA (firmware, legacy USB, graphics) and that must
/// stay identity-mapped in their domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rmrr {
    pub segment: u16,
    pub base: u64,
    /// Inclusive.
    pub limit: u64,
    pub scopes: Vec<DeviceScope>,
}

/// DMAR table (VT-d).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dmar {
    /// Physical address bits translated by the units.
    pub host_address_width: u8,
    pub flags: u8,
    pub units: Vec<Drhd>,
    pub reserved: Vec<Rmrr>,
}

fn parse_scopes(mut d: &[u8]) -> Result<Vec<DeviceScope>, AcpiError> {
    let mut scopes = Vec::new();
    while !d.is_empty() {
        if d.len() < 6 {
            return Err(AcpiError::Truncated);
        }
        let len = d[1] as usize;
        if len < 6 || len > d.len() || !(len - 6).is_multiple_of(2) {
            return Err(AcpiError::Truncated);
        }
        let path = d[6..len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| (p[0], p[1]))
            .collect();
        scopes.push(DeviceScope {
            kind: d[0],
            start_bus: d[5],
            path,
        });
        d = &d[len..];
    }
    Ok(scopes)
}

impl Dmar {
    pub fn parse(table: &[u8]) -> Result<Self, AcpiError> {
        let h = validate_table(table, b"DMAR")?;
        let d = &table[..h.length as usize];
        if d.len() < 48 {
            return Err(AcpiError::Truncated);
        }
        let mut dmar = Dmar {
            host_address_width: d[36] + 1,
            flags: d[37],
            ..Default::default()
        };
        let mut off = 48;
        while off + 4 <= d.len() {
            let (ty, len) = (u16_at(d, off), u16_at(d, off + 2) as usize);
            if len < 4 || off + len > d.len() {
                return Err(AcpiError::Truncated);
            }
            let e = &d[off..off + len];
            match ty {
                0 if len >= 16 => dmar.units.push(Drhd {
                    include_all: e[4] & 1 != 0,
                    segment: u16_at(e, 6),
                    register_base: u64_at(e, 8),
                    scopes: parse_scopes(&e[16..])?,
                }),
                1 if len >= 24 => dmar.reserved.push(Rmrr {
                    segment: u16_at(e, 6),
                    base: u64_at(e, 8),
                    limit: u64_at(e, 16),
                    scopes: parse_scopes(&e[24..])?,
                }),
                _ => {}
            }
            off += len;
        }
        Ok(dmar)
    }

    /// Index of the unit translating the given PCI function: the one listing
    /// it in its scope or, if none, the one covering the whole segment.
    pub fn unit_for(&self, segment: u16, bus: u8, device: u8, function: u8) -> Option<usize> {
        self.units
            .iter()
            .position(|u| {
                u.segment == segment && u.scopes.iter().any(|s| s.matches(bus, device, function))
            })
            .or_else(|| {
                self.units
                    .iter()
                    .position(|u| u.segment == segment && u.include_all)
            })
    }

    /// Reserved regions that apply to the given PCI function.
    pub fn reserved_for(
        &self,
        segment: u16,
        bus: u8,
        device: u8,
        function: u8,
    ) -> impl Iterator<Item = &Rmrr> + '_ {
        self.reserved.iter().filter(move |r| {
            r.segment == segment && r.scopes.iter().any(|s| s.matches(bus, device, function))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Sets byte `at` so that `t[..len]` sums to zero.
    fn with_checksum(mut t: Vec<u8>, at: usize, len: usize) -> Vec<u8> {
        t[at] = 0;
        let sum = t[..len].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        t[at] = 0u8.wrapping_sub(sum);
        t
    }

    fn table(sig: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(sig);
        t.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
        t.push(1); // revision
        t.push(0); // checksum
        t.extend_from_slice(b"ARICOD"); // oem id
        t.extend_from_slice(b"ARICODEO"); // oem table id
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(b"ARIC");
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(body);
        let len = t.len();
        with_checksum(t, 9, len)
    }

    #[test]
    fn rsdp_v1_and_v2() {
        let mut v1 = Vec::new();
        v1.extend_from_slice(b"RSD PTR ");
        v1.push(0);
        v1.extend_from_slice(b"ARICOD");
        v1.push(0); // revision
        v1.extend_from_slice(&0x1234u32.to_le_bytes());
        let v1 = with_checksum(v1, 8, 20);
        let r = Rsdp::parse(&v1).unwrap();
        assert_eq!(r.root(), (0x1234, false));

        let mut v2 = v1.clone();
        v2[15] = 2;
        v2.extend_from_slice(&36u32.to_le_bytes());
        v2.extend_from_slice(&0xFFFF_0000u64.to_le_bytes());
        v2.extend_from_slice(&[0, 0, 0, 0]); // ext checksum + reserved
        let v2 = with_checksum(v2, 8, 20);
        let v2 = with_checksum(v2, 32, 36);
        let r = Rsdp::parse(&v2).unwrap();
        assert_eq!(r.root(), (0xFFFF_0000, true));

        let mut bad = v1.clone();
        bad[16] ^= 1;
        assert_eq!(Rsdp::parse(&bad), Err(AcpiError::BadChecksum));
        assert_eq!(Rsdp::parse(b"RSD PTR"), Err(AcpiError::Truncated));
    }

    #[test]
    fn xsdt_entries_and_validation() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x1000u64.to_le_bytes());
        body.extend_from_slice(&0x2000u64.to_le_bytes());
        let x = table(b"XSDT", &body);
        assert_eq!(root_entries(&x, true).unwrap(), vec![0x1000, 0x2000]);
        assert_eq!(root_entries(&x, false), Err(AcpiError::BadSignature));
        let mut bad = x.clone();
        bad[40] ^= 1;
        assert_eq!(root_entries(&bad, true), Err(AcpiError::BadChecksum));
        assert_eq!(validate_table(&x[..20], b"XSDT"), Err(AcpiError::Truncated));
        let h = SdtHeader::parse(&x).unwrap();
        assert_eq!(h.signature_str(), "XSDT");
    }

    #[test]
    fn madt_entries_and_isa_override() {
        let mut body = Vec::new();
        body.extend_from_slice(&0xFEE0_0000u32.to_le_bytes()); // lapic
        body.extend_from_slice(&1u32.to_le_bytes()); // flags
                                                     // LAPIC id 0 enabled, LAPIC id 1 disabled
        body.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]);
        body.extend_from_slice(&[0, 8, 1, 1, 0, 0, 0, 0]);
        // I/O APIC id 0 @ 0xFEC00000, gsi 0
        body.extend_from_slice(&[1, 12, 0, 0]);
        body.extend_from_slice(&0xFEC0_0000u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        // Override irq 0 -> gsi 2, flags 0
        body.extend_from_slice(&[2, 10, 0, 0]);
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        // Override irq 9 -> gsi 9, level/low (0xF)
        body.extend_from_slice(&[2, 10, 0, 9]);
        body.extend_from_slice(&9u32.to_le_bytes());
        body.extend_from_slice(&0x000Fu16.to_le_bytes());
        // Unknown entry (type 9) that must be ignored
        body.extend_from_slice(&[9, 4, 0, 0]);
        let m = Madt::parse(&table(b"APIC", &body)).unwrap();
        assert_eq!(m.lapic_address, 0xFEE0_0000);
        assert_eq!(m.lapic_ids, vec![0]);
        assert_eq!(
            m.io_apics,
            vec![IoApic {
                id: 0,
                address: 0xFEC0_0000,
                gsi_base: 0
            }]
        );
        assert_eq!(m.isa_irq_to_gsi(0).0, 2);
        assert_eq!(m.isa_irq_to_gsi(1), (1, None));
        let (gsi, ov) = m.isa_irq_to_gsi(9);
        assert_eq!(gsi, 9);
        assert!(ov.unwrap().level_triggered() && ov.unwrap().active_low());
        assert!(!m.overrides[0].level_triggered());
        let mut trunc = table(b"APIC", &body);
        let claimed = trunc.len() as u32 + 8;
        trunc[4..8].copy_from_slice(&claimed.to_le_bytes());
        assert!(Madt::parse(&trunc).is_err());
    }

    #[test]
    fn mcfg_entries() {
        let mut body = vec![0u8; 8]; // reserved
        body.extend_from_slice(&0xB000_0000u64.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&[0, 255]);
        body.extend_from_slice(&[0u8; 4]);
        let e = parse_mcfg(&table(b"MCFG", &body)).unwrap();
        assert_eq!(
            e,
            vec![McfgEntry {
                base: 0xB000_0000,
                segment: 0,
                bus_start: 0,
                bus_end: 255
            }]
        );
        assert_eq!(
            parse_mcfg(&table(b"APIC", &body)),
            Err(AcpiError::BadSignature)
        );
    }

    #[test]
    fn dmar_units_reserved_regions_and_scopes() {
        // Scope: endpoint on bus 0, device 2 function 0 (8 bytes).
        let scope = |kind: u8, bus: u8, dev: u8, func: u8| vec![kind, 8, 0, 0, 0, bus, dev, func];
        let mut body = vec![39u8, 1]; // HAW - 1 = 39 -> 40 bits; flags
        body.extend_from_slice(&[0; 10]);
        // DRHD dedicated to 00:02.0 at 0xfed91000.
        let mut drhd0 = vec![0u8, 0, 0, 0, 0, 0, 0, 0];
        drhd0.extend_from_slice(&0xFED9_1000u64.to_le_bytes());
        drhd0.extend_from_slice(&scope(1, 0, 2, 0));
        let n0 = drhd0.len() as u16;
        drhd0[2..4].copy_from_slice(&n0.to_le_bytes());
        // DRHD INCLUDE_PCI_ALL at 0xfed90000.
        let mut drhd1 = vec![0u8, 0, 16, 0, 1, 0, 0, 0];
        drhd1.extend_from_slice(&0xFED9_0000u64.to_le_bytes());
        // RMRR 0xdead0000..=0xdeadffff for 00:1d.0.
        let mut rmrr = vec![1u8, 0, 0, 0, 0, 0, 0, 0];
        rmrr.extend_from_slice(&0xDEAD_0000u64.to_le_bytes());
        rmrr.extend_from_slice(&0xDEAD_FFFFu64.to_le_bytes());
        rmrr.extend_from_slice(&scope(1, 0, 0x1d, 0));
        let n1 = rmrr.len() as u16;
        rmrr[2..4].copy_from_slice(&n1.to_le_bytes());
        body.extend_from_slice(&drhd0);
        body.extend_from_slice(&drhd1);
        body.extend_from_slice(&rmrr);
        let t = table(b"DMAR", &body);
        let d = Dmar::parse(&t).unwrap();
        assert_eq!(d.host_address_width, 40);
        assert_eq!(d.flags, 1);
        assert_eq!(d.units.len(), 2);
        assert_eq!(d.units[0].register_base, 0xFED9_1000);
        assert!(!d.units[0].include_all);
        assert_eq!(d.units[0].scopes[0].path, vec![(2, 0)]);
        assert!(d.units[1].include_all);
        assert_eq!(d.unit_for(0, 0, 2, 0), Some(0), "explicit scope");
        assert_eq!(d.unit_for(0, 0, 3, 0), Some(1), "INCLUDE_PCI_ALL");
        assert_eq!(d.unit_for(1, 0, 3, 0), None, "other segment");
        assert_eq!(d.reserved.len(), 1);
        assert_eq!(d.reserved[0].base, 0xDEAD_0000);
        assert_eq!(d.reserved[0].limit, 0xDEAD_FFFF);
        assert_eq!(d.reserved_for(0, 0, 0x1d, 0).count(), 1);
        assert_eq!(d.reserved_for(0, 0, 2, 0).count(), 0);

        // Truncated scope: error, no out-of-bounds read.
        let mut bad_body = body.clone();
        let n = bad_body.len();
        bad_body[n - 7] = 9; // length of the last scope > what is left
        let bad = table(b"DMAR", &bad_body);
        assert_eq!(Dmar::parse(&bad), Err(AcpiError::Truncated));
        assert_eq!(
            Dmar::parse(&table(b"DMAR", &[0; 4])),
            Err(AcpiError::Truncated)
        );
    }
}
