// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The machine's make and model from the firmware's SMBIOS tables (DMTF
//! DSP0134), read only, for `diag`: which machine a report came from, so a
//! device study starts from the right manuals. The serial number and the
//! UUID are never read: a report may leave the machine.

#![no_std]

/// Where an SMBIOS entry point may sit on a PC: the legacy BIOS area,
/// on 16-byte boundaries (DSP0134, 5.2.1).
pub const LEGACY_AREA: (u64, usize) = (0xF_0000, 0x1_0000);

/// The structure table an entry point names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub table: u64,
    pub len: u32,
    pub major: u8,
    pub minor: u8,
}

fn sums_to_zero(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// Finds an entry point in `area` (the legacy area's bytes): the 64-bit
/// `_SM3_` one first, then the 32-bit `_SM_` one, each with its checksum.
pub fn find_entry(area: &[u8]) -> Option<Entry> {
    let at = |i: usize, n: usize| area.get(i..i + n);
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    for i in (0..area.len()).step_by(16) {
        if let Some(e) = at(i, 24).filter(|e| &e[..5] == b"_SM3_") {
            let len = e[6] as usize;
            if (24..=32).contains(&len) && at(i, len).is_some_and(sums_to_zero) {
                let table = u64::from_le_bytes(e[16..24].try_into().ok()?);
                return Some(Entry {
                    table,
                    len: u32_at(e, 12),
                    major: e[7],
                    minor: e[8],
                });
            }
        }
    }
    for i in (0..area.len()).step_by(16) {
        if let Some(e) = at(i, 31).filter(|e| &e[..4] == b"_SM_") {
            let len = e[5] as usize;
            if (31..=32).contains(&len)
                && sums_to_zero(&e[..len.min(31)])
                && &e[16..21] == b"_DMI_"
                && sums_to_zero(&e[16..31])
            {
                return Some(Entry {
                    table: u32_at(e, 24) as u64,
                    len: u16::from_le_bytes([e[22], e[23]]) as u32,
                    major: e[6],
                    minor: e[7],
                });
            }
        }
    }
    None
}

/// The system information structure (type 1): maker, product (Lenovo puts
/// the machine type here), version (Lenovo's model name) and family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct System<'a> {
    pub maker: &'a str,
    pub product: &'a str,
    pub version: &'a str,
    pub family: &'a str,
}

/// The `n`th string (from 1) after a structure's formatted area; empty for
/// 0 or one not there.
fn string(strings: &[u8], n: u8) -> &str {
    if n == 0 {
        return "";
    }
    strings
        .split(|&b| b == 0)
        .take_while(|s| !s.is_empty())
        .nth(n as usize - 1)
        .and_then(|s| core::str::from_utf8(s).ok())
        .map(str::trim)
        .unwrap_or("")
}

/// The type 1 structure of a structure table.
pub fn system(table: &[u8]) -> Option<System<'_>> {
    let mut i = 0;
    while i + 4 <= table.len() {
        let (kind, len) = (table[i], table[i + 1] as usize);
        if len < 4 || i + len > table.len() {
            return None;
        }
        let formatted = &table[i..i + len];
        // The strings end with two zero bytes.
        let rest = &table[i + len..];
        let end = rest.windows(2).position(|w| w == [0, 0])? + 2;
        let strings = &rest[..end];
        if kind == 1 {
            let field = |k: usize| formatted.get(k).copied().unwrap_or(0);
            return Some(System {
                maker: string(strings, field(4)),
                product: string(strings, field(5)),
                version: string(strings, field(6)),
                family: string(strings, field(0x1A)),
            });
        }
        if kind == 127 {
            return None;
        }
        i += len + end;
    }
    None
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn with_sum(mut b: Vec<u8>, at: usize, range: core::ops::Range<usize>) -> Vec<u8> {
        b[at] = 0;
        let s = b[range].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        b[at] = 0u8.wrapping_sub(s);
        b
    }

    #[test]
    fn a_lenovo_type_1_without_its_serial() {
        // Type 0 (BIOS) first, then type 1, then the end.
        let mut t = Vec::new();
        t.extend_from_slice(&[0, 0x18, 0, 0]);
        t.extend_from_slice(&[1; 0x14]);
        t.extend_from_slice(b"LENOVO\0N24ET\0\0");
        let mut one = [0u8; 0x1B];
        one[0] = 1;
        one[1] = 0x1B;
        one[4] = 1;
        one[5] = 2;
        one[6] = 3;
        one[7] = 4; // the serial: never read
        one[0x1A] = 5;
        t.extend_from_slice(&one);
        t.extend_from_slice(b"LENOVO\0 20L50000XX \0ThinkPad T480\0PF1ABCDE\0ThinkPad T480\0\0");
        t.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let s = system(&t).unwrap();
        assert_eq!(
            s,
            System {
                maker: "LENOVO",
                product: "20L50000XX",
                version: "ThinkPad T480",
                family: "ThinkPad T480",
            }
        );
        // A structure that runs past the table is refused.
        assert_eq!(system(&t[..10]), None);
    }

    #[test]
    fn entry_points_in_the_legacy_area() {
        let mut area = std::vec![0u8; 0x100];
        let mut e = std::vec![0u8; 31];
        e[..4].copy_from_slice(b"_SM_");
        e[5] = 31;
        e[6] = 2;
        e[7] = 7;
        e[16..21].copy_from_slice(b"_DMI_");
        e[22..24].copy_from_slice(&0x0456u16.to_le_bytes());
        e[24..28].copy_from_slice(&0x000E_1000u32.to_le_bytes());
        let e = with_sum(e, 21, 16..31);
        let e = with_sum(e, 4, 0..31);
        area[0x40..0x40 + 31].copy_from_slice(&e);
        assert_eq!(
            find_entry(&area),
            Some(Entry {
                table: 0xE_1000,
                len: 0x456,
                major: 2,
                minor: 7
            })
        );
        // A bad checksum is no entry point.
        area[0x40 + 10] ^= 1;
        assert_eq!(find_entry(&area), None);
        // The 64-bit one.
        let mut e3 = std::vec![0u8; 24];
        e3[..5].copy_from_slice(b"_SM3_");
        e3[6] = 24;
        e3[7] = 3;
        e3[8] = 2;
        e3[12..16].copy_from_slice(&0x800u32.to_le_bytes());
        e3[16..24].copy_from_slice(&0x7_8000_0000u64.to_le_bytes());
        let e3 = with_sum(e3, 5, 0..24);
        area[0x80..0x80 + 24].copy_from_slice(&e3);
        assert_eq!(
            find_entry(&area).map(|e| (e.table, e.major)),
            Some((0x7_8000_0000, 3))
        );
    }
}
