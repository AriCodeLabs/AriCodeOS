//! Tablas ACPI estáticas (sección 31): sin AML. Todo trabaja sobre slices de
//! bytes que el kernel obtiene del mapa físico, así que se prueba en host con
//! tablas sintéticas.

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
    /// Solo en revisión >= 2.
    pub xsdt_address: u64,
}

impl Rsdp {
    /// Tamaño máximo que hay que leer para parsear cualquier revisión.
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

    /// Dirección de la tabla raíz preferida y si es XSDT (64 bits).
    pub fn root(&self) -> (u64, bool) {
        if self.revision >= 2 && self.xsdt_address != 0 {
            (self.xsdt_address, true)
        } else {
            (self.rsdt_address as u64, false)
        }
    }
}

/// Cabecera común de toda tabla (36 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
}

impl SdtHeader {
    pub const LEN: usize = 36;

    /// Solo la cabecera; no comprueba el checksum (hace falta la tabla entera).
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

/// Comprueba que `table` es una tabla completa con `signature` y checksum válido.
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

/// Direcciones de las tablas listadas por la XSDT (64 bits) o RSDT (32 bits).
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
    /// Polaridad activa en bajo según los flags MPS INTI.
    pub fn active_low(&self) -> bool {
        self.flags & 0b11 == 0b11
    }
    /// Disparo por nivel según los flags MPS INTI.
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

    /// GSI de una IRQ ISA, aplicando overrides (identidad si no hay).
    pub fn isa_irq_to_gsi(&self, irq: u8) -> (u32, Option<InterruptOverride>) {
        match self.overrides.iter().find(|o| o.irq == irq) {
            Some(o) => (o.gsi, Some(*o)),
            None => (irq as u32, None),
        }
    }
}

/// Entrada de la MCFG: una región ECAM.
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Fija el byte `at` para que `t[..len]` sume cero.
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
        v2.extend_from_slice(&[0, 0, 0, 0]); // ext checksum + reservado
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
                                                     // LAPIC id 0 habilitado, LAPIC id 1 deshabilitado
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
        // Entrada desconocida (tipo 9) que debe ignorarse
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
        let mut body = vec![0u8; 8]; // reservado
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
}
