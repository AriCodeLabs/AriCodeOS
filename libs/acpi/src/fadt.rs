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

/// A fixed-hardware register block as the FADT describes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Block {
    pub space: u8,
    pub address: u64,
    /// Bytes in the block: the legacy length field or, if that is zero, the
    /// `X_` form's bit width.
    pub len: u8,
    pub source: Source,
    /// Both forms present with different addresses (the `X_` one is used,
    /// as the specification says; ACPICA warns the same way).
    pub mismatch: bool,
}

impl Block {
    pub fn is_present(&self) -> bool {
        self.source != Source::Absent
    }

    /// Picks the address of a block from its 32-bit field, its `X_` form
    /// (`None` when the table is too short to hold it) and the legacy
    /// length field.
    fn select(address32: u32, x: Option<Gas>, len: u8) -> Self {
        match x {
            Some(g) if g.address != 0 => Block {
                space: g.space,
                address: g.address,
                len: if len != 0 { len } else { g.bit_width / 8 },
                source: Source::Extended,
                mismatch: address32 != 0 && address32 as u64 != g.address,
            },
            _ if address32 != 0 => Block {
                space: SPACE_IO,
                address: address32 as u64,
                len,
                source: Source::Legacy,
                mismatch: false,
            },
            _ => Block::default(),
        }
    }

    /// The block as I/O ports: the first port, if the block is in I/O
    /// space, `min_len` bytes long or more, of even length when `even`, and
    /// wholly below port 0x10000.
    pub fn io_ports(&self, min_len: u8, even: bool) -> Result<u16, BlockError> {
        if !self.is_present() {
            return Err(BlockError::Absent);
        }
        if self.space != SPACE_IO {
            return Err(BlockError::NotIo(self.space));
        }
        if self.len < min_len || (even && !self.len.is_multiple_of(2)) {
            return Err(BlockError::Length(self.len));
        }
        if self.address.saturating_add(self.len as u64) > 0x1_0000 {
            return Err(BlockError::Beyond(self.address));
        }
        Ok(self.address as u16)
    }
}

/// Why a block cannot be used as I/O ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Absent,
    /// In another address space (`SPACE_MEMORY` and others): not supported.
    NotIo(u8),
    /// Too short, or odd where it is split in two halves.
    Length(u8),
    /// Runs past the last I/O port.
    Beyond(u64),
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            BlockError::Absent => write!(f, "absent"),
            BlockError::NotIo(s) => write!(f, "in address space {s}, not I/O"),
            BlockError::Length(l) => write!(f, "length {l} not usable"),
            BlockError::Beyond(a) => write!(f, "at {a:#x}, past the I/O ports"),
        }
    }
}

impl fmt::Display for Block {
    /// `0x1804 X_` or `0x604` or `none`, with the length and a mismatch.
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
        write!(f, " {} bytes", self.len)
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

/// Parses the FADT. Refuses a table shorter than ACPI 1.0's (116 bytes);
/// fields past the table's end (an older revision) read as absent.
pub fn parse_fadt(table: &[u8]) -> Result<Fadt, AcpiError> {
    let h = validate_table(table, b"FACP")?;
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
        flags,
        dsdt,
        sci_interrupt: u16_at(46),
        smi_cmd: u32_at(48),
        acpi_enable: u8_at(52),
        pm1a_evt: Block::select(u32_at(56), gas_at(148), u8_at(88)),
        pm1b_evt: Block::select(u32_at(60), gas_at(160), u8_at(88)),
        pm1a_cnt: Block::select(u32_at(64), gas_at(172), u8_at(89)),
        pm1b_cnt: Block::select(u32_at(68), gas_at(184), u8_at(89)),
        gpe0: Block::select(u32_at(80), gas_at(220), u8_at(92)),
        gpe1: Block::select(u32_at(84), gas_at(232), u8_at(93)),
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
        assert_eq!(f.dsdt, 0x7FE0_0000);
        assert_eq!((f.sci_interrupt, f.smi_cmd, f.acpi_enable), (9, 0xB2, 0xF1));
        assert_eq!(
            f.pm1a_cnt,
            Block {
                space: SPACE_IO,
                address: 0x604,
                len: 2,
                source: Source::Legacy,
                mismatch: false
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
    /// reported, an `X_` block with no legacy length takes its bit width,
    /// and the hardware-reduced flag and sleep registers are read.
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
        put(&mut body, 220, &gas(SPACE_MEMORY, 128, 0xFE00_0060));
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
        // No legacy event length: the bit width gives it.
        assert_eq!(f.pm1a_evt.len, 4);
        assert_eq!(f.gpe0.len, 16);
        assert_eq!(
            f.gpe0.io_ports(2, true),
            Err(BlockError::NotIo(SPACE_MEMORY))
        );
        assert_eq!(f.sleep_control.address, 0x1830);
        assert_eq!(f.sleep_status.address, 0x1831);
        assert_eq!(
            alloc::format!("{} / {}", f.pm1a_cnt, f.pm1b_cnt),
            "0x1804 X_ 2 bytes / 0x1808 X_ (32-bit differs) 2 bytes"
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

    /// Short, mislabelled, truncated or corrupt tables are refused.
    #[test]
    fn malformed_tables_refused() {
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
        // A bad checksum.
        let mut t = table(b"FACP", &body);
        t[60] ^= 1;
        assert_eq!(parse_fadt(&t), Err(AcpiError::BadChecksum));
    }

    #[test]
    fn io_ports_bounds() {
        let b = |address: u64, len: u8| Block {
            space: SPACE_IO,
            address,
            len,
            source: Source::Extended,
            mismatch: false,
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
