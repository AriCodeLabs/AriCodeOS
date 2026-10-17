// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! GPT (UEFI 2.x, §5.3): reading of header and entries with CRC32
//! verification, and writing for the xtask test disk.

#![no_std]

#[cfg(feature = "write")]
extern crate std;

pub const SECTOR: usize = 512;
const SIGNATURE: &[u8; 8] = b"EFI PART";
pub const ENTRY_LEN: usize = 128;

/// CRC-32 (IEEE 802.3, reflected), the one GPT uses.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GptError {
    BadSignature,
    BadHeaderCrc,
    BadEntriesCrc,
    Truncated,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub current_lba: u64,
    pub backup_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk_guid: [u8; 16],
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_len: u32,
    pub entries_crc: u32,
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

impl Header {
    /// Parses a header (one sector) and verifies its CRC.
    pub fn parse(sector: &[u8]) -> Result<Self, GptError> {
        if sector.len() < 92 {
            return Err(GptError::Truncated);
        }
        if &sector[..8] != SIGNATURE {
            return Err(GptError::BadSignature);
        }
        let header_len = u32_at(sector, 12) as usize;
        if header_len < 92 || header_len > sector.len() {
            return Err(GptError::Truncated);
        }
        let stored = u32_at(sector, 16);
        let mut copy = [0u8; SECTOR];
        copy[..header_len].copy_from_slice(&sector[..header_len]);
        copy[16..20].fill(0);
        if crc32(&copy[..header_len]) != stored {
            return Err(GptError::BadHeaderCrc);
        }
        let entry_len = u32_at(sector, 84);
        if entry_len as usize != ENTRY_LEN {
            return Err(GptError::Unsupported);
        }
        let mut disk_guid = [0u8; 16];
        disk_guid.copy_from_slice(&sector[56..72]);
        Ok(Self {
            current_lba: u64_at(sector, 24),
            backup_lba: u64_at(sector, 32),
            first_usable: u64_at(sector, 40),
            last_usable: u64_at(sector, 48),
            disk_guid,
            entries_lba: u64_at(sector, 72),
            entry_count: u32_at(sector, 80),
            entry_len,
            entries_crc: u32_at(sector, 88),
        })
    }

    /// Bytes taken by the entry table (a multiple of the sector size).
    pub fn entries_bytes(&self) -> usize {
        (self.entry_count as usize * ENTRY_LEN).next_multiple_of(SECTOR)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    pub type_guid: [u8; 16],
    pub guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    /// UTF-16LE name converted to ASCII (non-ASCII characters become `?`).
    pub name: [u8; 36],
    pub name_len: usize,
}

impl Partition {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }
    pub fn sectors(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }
}

/// Non-empty entries of the table, verifying their CRC against the header.
pub fn partitions(
    header: &Header,
    table: &[u8],
    out: &mut [Option<Partition>],
) -> Result<usize, GptError> {
    let len = header.entry_count as usize * ENTRY_LEN;
    if table.len() < len {
        return Err(GptError::Truncated);
    }
    if crc32(&table[..len]) != header.entries_crc {
        return Err(GptError::BadEntriesCrc);
    }
    let mut n = 0;
    for e in table[..len].as_chunks::<ENTRY_LEN>().0 {
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        if n >= out.len() {
            break;
        }
        let mut p = Partition {
            type_guid: e[..16].try_into().unwrap(),
            guid: e[16..32].try_into().unwrap(),
            first_lba: u64_at(e, 32),
            last_lba: u64_at(e, 40),
            name: [0; 36],
            name_len: 0,
        };
        for pair in e[56..128].as_chunks::<2>().0 {
            let c = u16::from_le_bytes([pair[0], pair[1]]);
            if c == 0 {
                break;
            }
            p.name[p.name_len] = if c < 128 { c as u8 } else { b'?' };
            p.name_len += 1;
        }
        out[n] = Some(p);
        n += 1;
    }
    Ok(n)
}

/// Type GUID of the AriCodeOS partitions.
pub const ARICODEOS_DATA_GUID: [u8; 16] = [
    0x41, 0x52, 0x49, 0x43, 0x4F, 0x44, 0x45, 0x4F, 0x53, 0x2D, 0x44, 0x41, 0x54, 0x41, 0x30, 0x31,
];

/// Writes a complete GPT disk (protective MBR, headers, table) with the
/// given partitions over `disk` (a multiple of the sector size). Returns the
/// usable LBAs `[first, last]`.
#[cfg(feature = "write")]
pub fn write_disk(
    disk: &mut [u8],
    parts: &[(&str, u64, u64, [u8; 16])],
) -> Result<(u64, u64), GptError> {
    let sectors = (disk.len() / SECTOR) as u64;
    const ENTRIES: u32 = 128;
    let table_sectors = (ENTRIES as usize * ENTRY_LEN / SECTOR) as u64; // 32
    if sectors < 2 * (1 + table_sectors) + 2 {
        return Err(GptError::Truncated);
    }
    let first_usable = 2 + table_sectors;
    let last_usable = sectors - 2 - table_sectors;
    // Protective MBR.
    let mbr = &mut disk[..SECTOR];
    mbr.fill(0);
    mbr[446] = 0;
    mbr[446 + 4] = 0xEE;
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&(sectors.min(0xFFFF_FFFF) as u32 - 1).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    // Entry table.
    let mut table = std::vec![0u8; ENTRIES as usize * ENTRY_LEN];
    for (i, (name, first, last, type_guid)) in parts.iter().enumerate() {
        if *first < first_usable || *last > last_usable || first > last {
            return Err(GptError::Unsupported);
        }
        let e = &mut table[i * ENTRY_LEN..(i + 1) * ENTRY_LEN];
        e[..16].copy_from_slice(type_guid);
        e[16..32].copy_from_slice(&[0x10 + i as u8; 16]);
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        for (j, c) in name.bytes().take(35).enumerate() {
            e[56 + 2 * j] = c;
        }
    }
    let entries_crc = crc32(&table);
    let header = |current: u64, backup: u64, entries_lba: u64| -> std::vec::Vec<u8> {
        let mut h = std::vec![0u8; SECTOR];
        h[..8].copy_from_slice(SIGNATURE);
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&current.to_le_bytes());
        h[32..40].copy_from_slice(&backup.to_le_bytes());
        h[40..48].copy_from_slice(&first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&[0xA5; 16]);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&ENTRIES.to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_LEN as u32).to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let c = crc32(&h[..92]);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        h
    };
    let primary = header(1, sectors - 1, 2);
    let backup = header(sectors - 1, 1, sectors - 1 - table_sectors);
    disk[SECTOR..2 * SECTOR].copy_from_slice(&primary);
    disk[2 * SECTOR..2 * SECTOR + table.len()].copy_from_slice(&table);
    let backup_table = (sectors - 1 - table_sectors) as usize * SECTOR;
    disk[backup_table..backup_table + table.len()].copy_from_slice(&table);
    let backup_hdr = (sectors - 1) as usize * SECTOR;
    disk[backup_hdr..backup_hdr + SECTOR].copy_from_slice(&backup);
    Ok((first_usable, last_usable))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[cfg(feature = "write")]
    #[test]
    fn write_then_read_back() {
        let mut disk = std::vec![0u8; 512 * 256];
        let (first, last) =
            write_disk(&mut disk, &[("ARI", 40, 200, ARICODEOS_DATA_GUID)]).unwrap();
        assert_eq!((first, last), (34, 256 - 34));
        let h = Header::parse(&disk[512..1024]).unwrap();
        assert_eq!(
            (h.current_lba, h.backup_lba, h.entries_lba, h.entry_count),
            (1, 255, 2, 128)
        );
        let table = &disk[1024..1024 + h.entries_bytes()];
        let mut out = [None; 4];
        assert_eq!(partitions(&h, table, &mut out), Ok(1));
        let p = out[0].unwrap();
        assert_eq!((p.first_lba, p.last_lba, p.sectors()), (40, 200, 161));
        assert_eq!(p.name_str(), "ARI");
        assert_eq!(p.type_guid, ARICODEOS_DATA_GUID);
        // The backup header is valid too.
        let b = Header::parse(&disk[255 * 512..]).unwrap();
        assert_eq!(b.current_lba, 255);
        // Protective MBR.
        assert_eq!(disk[446 + 4], 0xEE);
        assert_eq!(&disk[510..512], &[0x55, 0xAA]);
    }

    #[cfg(feature = "write")]
    #[test]
    fn corruption_is_detected() {
        let mut disk = std::vec![0u8; 512 * 256];
        write_disk(&mut disk, &[("A", 34, 100, [1; 16])]).unwrap();
        let mut hdr: [u8; 512] = disk[512..1024].try_into().unwrap();
        hdr[40] ^= 1;
        assert_eq!(Header::parse(&hdr).err(), Some(GptError::BadHeaderCrc));
        hdr[0] = b'X';
        assert_eq!(Header::parse(&hdr).err(), Some(GptError::BadSignature));
        let h = Header::parse(&disk[512..1024]).unwrap();
        let mut table = disk[1024..1024 + h.entries_bytes()].to_vec();
        table[32] ^= 1;
        let mut out = [None; 1];
        assert_eq!(
            partitions(&h, &table, &mut out),
            Err(GptError::BadEntriesCrc)
        );
        assert!(
            write_disk(&mut disk, &[("A", 1, 100, [1; 16])]).is_err(),
            "LBA outside the usable range"
        );
    }
}
