// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The FADT (`FACP`, ACPI 6.5 section 5.2.9, table 5.9): where the fixed
//! hardware's register blocks are (PM1 event and control, GPE0/GPE1), in
//! their legacy 32-bit form and their extended (`X_`) Generic Address form,
//! the flags (hardware-reduced) and the ACPI 5.0 sleep control and status
//! registers. Field offsets follow the table; the choice between a legacy
//! and an `X_` address follows the specification ("the 64-bit address
//! supersedes the 32-bit one"), as ACPICA's `acpi_tb_convert_fadt` does.

use crate::{validate_table, AcpiError};
use core::fmt;

/// Size of an ACPI 1.0 FADT (through `FLAGS`): shorter tables are refused.
pub const FADT_V1_LEN: usize = 116;

/// `FLAGS` bit 20, `HW_REDUCED_ACPI` (ACPI 5.0): no fixed hardware; sleep
/// goes through the sleep control and status registers.
pub const FLAG_HW_REDUCED: u32 = 1 << 20;
/// `FLAGS` bit 21, `LOW_POWER_S0_IDLE_CAPABLE` (ACPI 5.0).
pub const FLAG_LOW_POWER_S0: u32 = 1 << 21;

/// Generic Address Structure address spaces (ACPI 6.5, table 5.1).
pub const SPACE_MEMORY: u8 = 0;
pub const SPACE_IO: u8 = 1;

/// A Generic Address Structure (ACPI 6.5, 5.2.3.2): 12 bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gas {
    pub space: u8,
    pub bit_width: u8,
    pub bit_offset: u8,
    pub access_size: u8,
    pub address: u64,
}

impl Gas {
    pub const LEN: usize = 12;

    fn parse(d: &[u8]) -> Self {
        Self {
            space: d[0],
            bit_width: d[1],
            bit_offset: d[2],
            access_size: d[3],
            address: u64::from_le_bytes(d[4..12].try_into().unwrap()),
        }
    }

    pub fn is_present(&self) -> bool {
        self.address != 0
    }
}

/// Where a register block's address came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Source {
    /// Neither form gives an address.
    #[default]
    Absent,
    /// The 32-bit field (always I/O space).
    Legacy,
    /// The `X_` Generic Address.
    Extended,
}

/// Which kind of fixed-hardware block, for its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PM1 event block: always ACPI's default of 4 bytes (two 16-bit
    /// registers), as ACPICA uses (`acpi_gbl_use_default_register_widths`).
    Pm1Event,
    /// PM1 control block: always 2 bytes, likewise.
    Pm1Control,
    /// GPE block: the legacy byte length, as ACPICA's `acpi_ev_gpe_initialize`
    /// takes it; a block with length 0 does not exist.
    Gpe,
}

/// A fixed-hardware register block as the FADT describes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Block {
    pub space: u8,
    pub address: u64,
    /// Bytes used: ACPI's default for PM1 blocks, the legacy length for GPE.
    pub len: u8,
    /// The length the FADT declares, when it differs from `len`.
    pub declared_len: Option<u8>,
    /// The `X_` form's bit offset (the 32-bit form has none).
    pub bit_offset: u8,
    pub source: Source,
    /// Both forms present with different addresses (the `X_` one is used,
    /// as the specification says; ACPICA warns the same way).
    pub mismatch: bool,
    /// The `X_` form was present but unusable as I/O ports, and the valid
    /// 32-bit field was used instead (Linux's `acpi_force_32bit_fadt_addr`
    /// for "corrupted" 64-bit addresses): why the `X_` form was not.
    pub x_rejected: Option<BlockError>,
}

impl Block {
    pub fn is_present(&self) -> bool {
        self.source != Source::Absent
    }

    /// Picks a block from its 32-bit field, its `X_` form (`None` when the
    /// table is too short to hold it) and the legacy length field.
    fn select(kind: Kind, address32: u32, x: Option<Gas>, legacy_len: u8) -> Self {
        if kind == Kind::Gpe && legacy_len == 0 {
            return Block::default();
        }
        let block = |space, address, bit_offset, declared: u8, source| {
            let len = match kind {
                Kind::Pm1Event => 4,
                Kind::Pm1Control => 2,
                Kind::Gpe => legacy_len,
            };
            Block {
                space,
                address,
                len,
                declared_len: (declared != len).then_some(declared),
                bit_offset,
                source,
                mismatch: false,
                x_rejected: None,
            }
        };
        let legacy = block(SPACE_IO, address32 as u64, 0, legacy_len, Source::Legacy);
        match x {
            Some(g) if g.address != 0 => {
                let declared = if legacy_len != 0 {
                    legacy_len
                } else {
                    g.bit_width / 8
                };
                let mut b = block(g.space, g.address, g.bit_offset, declared, Source::Extended);
                b.mismatch = address32 != 0 && address32 as u64 != g.address;
                match b.io_check() {
                    Err(e) if address32 != 0 && legacy.io_check().is_ok() => Block {
                        x_rejected: Some(e),
                        ..legacy
                    },
                    _ => b,
                }
            }
            _ if address32 != 0 => legacy,
            _ => Block::default(),
        }
    }

    /// Present, in I/O space, at bit offset 0 and wholly below port 0x10000.
    fn io_check(&self) -> Result<u16, BlockError> {
        if !self.is_present() {
            return Err(BlockError::Absent);
        }
        if self.space != SPACE_IO {
            return Err(BlockError::NotIo(self.space));
        }
        if self.bit_offset != 0 {
            return Err(BlockError::BitOffset(self.bit_offset));
        }
        if self.address.saturating_add(self.len.max(1) as u64) > 0x1_0000 {
            return Err(BlockError::Beyond(self.address));
        }
        Ok(self.address as u16)
    }

    /// The block as I/O ports: the first port, if [`Self::io_check`] holds
    /// and the block is `min_len` bytes long or more, of even length when
    /// `even`.
    pub fn io_ports(&self, min_len: u8, even: bool) -> Result<u16, BlockError> {
        let port = self.io_check()?;
        if self.len < min_len || (even && !self.len.is_multiple_of(2)) {
            return Err(BlockError::Length(self.len));
        }
        Ok(port)
    }
}

/// Why a block cannot be used as I/O ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Absent,
    /// In another address space (`SPACE_MEMORY` and others): not supported.
    NotIo(u8),
    /// A register that does not start at bit 0: not supported.
    BitOffset(u8),
    /// Too short, or odd where it is split in two halves.
    Length(u8),
    /// Runs past the last I/O port.
    Beyond(u64),
    /// Reads 0xffff: nothing answers at that port.
    Floating(u16),
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            BlockError::Absent => write!(f, "absent"),
            BlockError::NotIo(s) => write!(f, "in address space {s}, not I/O"),
            BlockError::BitOffset(o) => write!(f, "at bit offset {o}"),
            BlockError::Length(l) => write!(f, "length {l} not usable"),
            BlockError::Beyond(a) => write!(f, "at {a:#x}, past the I/O ports"),
            BlockError::Floating(p) => write!(f, "{p:#x} reads 0xffff, nothing answers"),
        }
    }
}

impl fmt::Display for Block {
    /// `0x1804 X_ 2 bytes` or `0x604 2 bytes` or `none`, with what differs.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if !self.is_present() {
            return write!(f, "none");
        }
        write!(f, "{:#x}", self.address)?;
        if self.space != SPACE_IO {
            write!(f, " (space {})", self.space)?;
        }
        if self.source == Source::Extended {
            write!(f, " X_")?;
        }
        if self.mismatch {
            write!(f, " (32-bit differs)")?;
        }
        if let Some(e) = self.x_rejected {
            write!(f, " (X_ {e}: 32-bit used)")?;
        }
        write!(f, " {} bytes", self.len)?;
        if let Some(d) = self.declared_len {
            write!(f, " (FADT says {d})")?;
        }
        Ok(())
    }
}

/// What the kernel needs from the FADT to power off through S5, and what
/// `diag` reports about it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fadt {
    /// Header revision and, from ACPI 5.1, `FADT Minor Version`.
    pub revision: u8,
    pub minor_revision: u8,
    /// The table's length in bytes.
    pub length: u32,
    /// `false` if the checksum did not add up: the table is used anyway,
    /// as ACPICA does by default (it only warns), and the caller says so.
    pub checksum_ok: bool,
    pub flags: u32,
    /// Physical address of the DSDT (`X_DSDT` when present and non-zero).
    pub dsdt: u64,
    pub sci_interrupt: u16,
    /// Port of the SMI command register (0 if the system is ACPI-only).
    pub smi_cmd: u32,
    /// Value written to `smi_cmd` to switch the firmware to ACPI mode.
    pub acpi_enable: u8,
    pub pm1a_evt: Block,
    pub pm1b_evt: Block,
    pub pm1a_cnt: Block,
    pub pm1b_cnt: Block,
    pub gpe0: Block,
    pub gpe1: Block,
    /// GPE number of GPE1's first event.
    pub gpe1_base: u8,
    /// ACPI 5.0 sleep registers (hardware-reduced platforms).
    pub sleep_control: Gas,
    pub sleep_status: Gas,
}

impl Fadt {
    pub fn hw_reduced(&self) -> bool {
        self.flags & FLAG_HW_REDUCED != 0
    }
}

/// Parses the FADT. Refuses a table that is not `FACP`, shorter than ACPI
/// 1.0's (116 bytes) or longer than the slice; a bad checksum is accepted
/// and said (`checksum_ok`). Fields past the table's end (an older
/// revision) read as absent.
pub fn parse_fadt(table: &[u8]) -> Result<Fadt, AcpiError> {
    let (h, checksum_ok) = match validate_table(table, b"FACP") {
        Ok(h) => (h, true),
        // Signature and length were checked before the checksum.
        Err(AcpiError::BadChecksum) => (crate::SdtHeader::parse(table)?, false),
        Err(e) => return Err(e),
    };
    let len = h.length as usize;
    if len < FADT_V1_LEN {
        return Err(AcpiError::Truncated);
    }
    let t = &table[..len];
    let u8_at = |o: usize| t[o];
    let u16_at = |o: usize| u16::from_le_bytes([t[o], t[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes(t[o..o + 4].try_into().unwrap());
    let gas_at = |o: usize| (o + Gas::LEN <= len).then(|| Gas::parse(&t[o..o + Gas::LEN]));
    let mut dsdt = u32_at(40) as u64;
    if len >= 148 {
        let x = u64::from_le_bytes(t[140..148].try_into().unwrap());
        if x != 0 {
            dsdt = x;
        }
    }
    let flags = u32_at(112);
    Ok(Fadt {
        revision: h.revision,
        minor_revision: if len > 131 { u8_at(131) } else { 0 },
        length: h.length,
        checksum_ok,
        flags,
        dsdt,
        sci_interrupt: u16_at(46),
        smi_cmd: u32_at(48),
        acpi_enable: u8_at(52),
        pm1a_evt: Block::select(Kind::Pm1Event, u32_at(56), gas_at(148), u8_at(88)),
        pm1b_evt: Block::select(Kind::Pm1Event, u32_at(60), gas_at(160), u8_at(88)),
        pm1a_cnt: Block::select(Kind::Pm1Control, u32_at(64), gas_at(172), u8_at(89)),
        pm1b_cnt: Block::select(Kind::Pm1Control, u32_at(68), gas_at(184), u8_at(89)),
        gpe0: Block::select(Kind::Gpe, u32_at(80), gas_at(220), u8_at(92)),
        gpe1: Block::select(Kind::Gpe, u32_at(84), gas_at(232), u8_at(93)),
        gpe1_base: u8_at(94),
        sleep_control: gas_at(244).unwrap_or_default(),
        sleep_status: gas_at(256).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::table;
    use alloc::vec;
    use alloc::vec::Vec;

    fn put(b: &mut [u8], off: usize, v: &[u8]) {
        b[off - 36..off - 36 + v.len()].copy_from_slice(v)
    }

    fn gas(space: u8, bits: u8, address: u64) -> Vec<u8> {
        let mut g = vec![space, bits, 0, 2];
        g.extend_from_slice(&address.to_le_bytes());
        g
    }

    /// A legacy (ACPI 1.0 sized) FADT as QEMU's i440fx gives: ports only.
    #[test]
    fn legacy_ports_only() {
        let mut body = vec![0u8; FADT_V1_LEN - 36];
        put(&mut body, 40, &0x7FE0_0000u32.to_le_bytes());
        put(&mut body, 46, &9u16.to_le_bytes());
        put(&mut body, 48, &0xB2u32.to_le_bytes());
        put(&mut body, 52, &[0xF1]);
        put(&mut body, 56, &0x600u32.to_le_bytes());
        put(&mut body, 64, &0x604u32.to_le_bytes());
        put(&mut body, 80, &0xAFE0u32.to_le_bytes());
        put(&mut body, 88, &[4, 2, 0, 4, 4, 0]);
        let f = parse_fadt(&table(b"FACP", &body)).unwrap();
        assert!(f.checksum_ok);
        assert_eq!(f.dsdt, 0x7FE0_0000);
        assert_eq!((f.sci_interrupt, f.smi_cmd, f.acpi_enable), (9, 0xB2, 0xF1));
        assert_eq!(
            f.pm1a_cnt,
            Block {
                space: SPACE_IO,
                address: 0x604,
                len: 2,
                source: Source::Legacy,
                ..Block::default()
            }
        );
        assert_eq!(f.pm1a_evt.io_ports(4, true), Ok(0x600));
        assert_eq!(f.gpe0.io_ports(2, true), Ok(0xAFE0));
        assert!(!f.pm1b_cnt.is_present() && !f.gpe1.is_present());
        assert_eq!(f.pm1b_cnt.io_ports(2, false), Err(BlockError::Absent));
        assert!(!f.hw_reduced());
        assert!(!f.sleep_control.is_present());
        assert_eq!(f.minor_revision, 0);
    }

    /// An ACPI 6 FADT: the `X_` forms win, a differing 32-bit field is
    /// reported, PM1 blocks take ACPI's default widths, a GPE block exists
    /// only with a legacy length, and the hardware-reduced flag and sleep
    /// registers are read.
    #[test]
    fn extended_blocks_and_flags() {
        let mut body = vec![0u8; 276 - 36];
        put(&mut body, 64, &0x1804u32.to_le_bytes());
        put(&mut body, 68, &0x9999u32.to_le_bytes());
        put(&mut body, 89, &[2]);
        put(
            &mut body,
            112,
            &(FLAG_HW_REDUCED | FLAG_LOW_POWER_S0).to_le_bytes(),
        );
        put(&mut body, 131, &[3]);
        put(&mut body, 148, &gas(SPACE_IO, 32, 0x1800));
        put(&mut body, 172, &gas(SPACE_IO, 16, 0x1804));
        put(&mut body, 184, &gas(SPACE_IO, 16, 0x1808));
        put(&mut body, 220, &gas(SPACE_IO, 128, 0x1860));
        put(&mut body, 232, &gas(SPACE_MEMORY, 64, 0xFE00_0000));
        put(&mut body, 93, &[8]);
        put(&mut body, 244, &gas(SPACE_IO, 8, 0x1830));
        put(&mut body, 256, &gas(SPACE_IO, 8, 0x1831));
        let f = parse_fadt(&table(b"FACP", &body)).unwrap();
        assert_eq!(f.length, 276);
        assert_eq!(f.minor_revision, 3);
        assert!(f.hw_reduced());
        assert_eq!(f.pm1a_cnt.source, Source::Extended);
        assert!(!f.pm1a_cnt.mismatch);
        assert_eq!(f.pm1b_cnt.address, 0x1808);
        assert!(f.pm1b_cnt.mismatch);
        assert_eq!((f.pm1a_evt.len, f.pm1a_evt.declared_len), (4, None));
        // GPE0 has no legacy length: no block, as ACPICA.
        assert!(!f.gpe0.is_present());
        assert_eq!(f.gpe1.len, 8);
        assert_eq!(
            f.gpe1.io_ports(2, true),
            Err(BlockError::NotIo(SPACE_MEMORY))
        );
        assert_eq!(f.sleep_control.address, 0x1830);
        assert_eq!(f.sleep_status.address, 0x1831);
        assert_eq!(
            alloc::format!("{} / {}", f.pm1a_cnt, f.pm1b_cnt),
            "0x1804 X_ 2 bytes / 0x1808 X_ (32-bit differs) 2 bytes"
        );
    }

    /// An unusable `X_` form (memory space, a garbage high dword, a bit
    /// offset) with a valid 32-bit field falls back to the 32-bit port and
    /// says why; without a 32-bit field the `X_` form stays, to be refused
    /// with its reason. A PM1 control length of 0 is taken as 2 and said.
    #[test]
    fn unusable_x_forms_fall_back_to_32_bits() {
        let mut body = vec![0u8; 244 - 36];
        put(&mut body, 64, &0x1804u32.to_le_bytes());
        put(&mut body, 68, &0x1808u32.to_le_bytes());
        put(&mut body, 172, &gas(SPACE_MEMORY, 16, 0xFE00_1804));
        put(&mut body, 184, &gas(SPACE_IO, 16, 0x1_0000_1808));
        let mut offset = gas(SPACE_IO, 32, 0x1800);
        offset[2] = 8;
        put(&mut body, 148, &offset);
        put(&mut body, 160, &gas(SPACE_MEMORY, 32, 0xFE00_1800));
        let f = parse_fadt(&table(b"FACP", &body)).unwrap();
        assert_eq!(f.pm1a_cnt.address, 0x1804);
        assert_eq!(f.pm1a_cnt.source, Source::Legacy);
        assert_eq!(f.pm1a_cnt.x_rejected, Some(BlockError::NotIo(SPACE_MEMORY)));
        assert_eq!(f.pm1a_cnt.declared_len, Some(0));
        assert_eq!(f.pm1a_cnt.io_ports(2, false), Ok(0x1804));
        assert_eq!(f.pm1b_cnt.address, 0x1808);
        assert_eq!(
            f.pm1b_cnt.x_rejected,
            Some(BlockError::Beyond(0x1_0000_1808))
        );
        // No 32-bit PM1a event field: the X_ form with its bit offset stays.
        assert_eq!(f.pm1a_evt.source, Source::Extended);
        assert_eq!(f.pm1a_evt.io_ports(4, true), Err(BlockError::BitOffset(8)));
        assert_eq!(
            f.pm1b_evt.io_ports(4, true),
            Err(BlockError::NotIo(SPACE_MEMORY))
        );
        assert_eq!(
            alloc::format!("{}", f.pm1a_cnt),
            "0x1804 (X_ in address space 0, not I/O: 32-bit used) 2 bytes (FADT says 0)"
        );
    }

    /// X_DSDT wins when present; an ACPI 2.0 table (244 bytes) has no
    /// sleep registers.
    #[test]
    fn dsdt_and_revision_2() {
        let mut body = vec![0u8; 244 - 36];
        put(&mut body, 40, &0x7FE0_0000u32.to_le_bytes());
        put(&mut body, 140, &0x1_2345_0000u64.to_le_bytes());
        let f = parse_fadt(&table(b"FACP", &body)).unwrap();
        assert_eq!(f.dsdt, 0x1_2345_0000);
        assert!(!f.sleep_control.is_present());
    }

    /// Every length from ACPI 1.0's to ACPI 6's parses without reading past
    /// the table: a field that does not fit whole reads as absent.
    #[test]
    fn every_length_between_versions() {
        for len in FADT_V1_LEN..=280 {
            let mut body = vec![0u8; len - 36];
            // Every X_ form an I/O address, as far as the table reaches.
            for (off, port) in [
                (148, 0x1800u64),
                (172, 0x1804),
                (220, 0x1860),
                (244, 0x1830),
            ] {
                if off + 12 <= len {
                    put(&mut body, off, &gas(SPACE_IO, 16, port));
                }
            }
            if 93 <= len {
                put(&mut body, 92, &[16]);
            }
            let f = parse_fadt(&table(b"FACP", &body)).unwrap();
            assert_eq!(f.length as usize, len);
            assert_eq!(f.pm1a_evt.is_present(), len >= 160, "{len}");
            assert_eq!(f.pm1a_cnt.is_present(), len >= 184, "{len}");
            assert_eq!(f.gpe0.is_present(), len >= 232, "{len}");
            assert_eq!(f.sleep_control.is_present(), len >= 256, "{len}");
            assert_eq!(f.minor_revision, 0);
        }
    }

    /// Short, mislabelled or truncated tables are refused; a bad checksum is
    /// accepted and said, as ACPICA only warns.
    #[test]
    fn malformed_tables() {
        let body = vec![0u8; FADT_V1_LEN - 36];
        // Shorter than ACPI 1.0's FADT.
        assert_eq!(
            parse_fadt(&table(b"FACP", &body[..72 - 36])),
            Err(AcpiError::Truncated)
        );
        assert_eq!(
            parse_fadt(&table(b"FACP", &body[..20])),
            Err(AcpiError::Truncated)
        );
        assert!(parse_fadt(&table(b"APIC", &body)).is_err());
        // The header claims more than the slice holds.
        let t = table(b"FACP", &body);
        assert_eq!(parse_fadt(&t[..t.len() - 1]), Err(AcpiError::Truncated));
        // A bad checksum, and a bad checksum on a short table.
        let mut t = table(b"FACP", &body);
        t[64] ^= 4;
        let f = parse_fadt(&t).unwrap();
        assert!(!f.checksum_ok);
        assert_eq!(f.pm1a_cnt.address, 4);
        let mut t = table(b"FACP", &body[..40]);
        t[50] ^= 1;
        assert_eq!(parse_fadt(&t), Err(AcpiError::Truncated));
    }

    #[test]
    fn io_ports_bounds() {
        let b = |address: u64, len: u8| Block {
            space: SPACE_IO,
            address,
            len,
            source: Source::Extended,
            ..Block::default()
        };
        assert_eq!(b(0xFFFE, 2).io_ports(2, false), Ok(0xFFFE));
        assert_eq!(
            b(0xFFFE, 4).io_ports(2, false),
            Err(BlockError::Beyond(0xFFFE))
        );
        assert_eq!(
            b(0x1_0000_0000, 2).io_ports(2, false),
            Err(BlockError::Beyond(0x1_0000_0000))
        );
        assert_eq!(
            b(0x1860, 0x21).io_ports(2, true),
            Err(BlockError::Length(0x21))
        );
        assert_eq!(b(0x1804, 1).io_ports(2, false), Err(BlockError::Length(1)));
    }
}
