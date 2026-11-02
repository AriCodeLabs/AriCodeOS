// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriFS: the file system of the `ARI0` partition.
//!
//! - [`v1`] (ADR 0014): 32 files of fixed capacity and a replace journal
//!   of 32 KiB. Kept to operate disks not yet migrated and to migrate them.
//! - [`v2`] (ADR 0072): files of up to 16 MiB in extents, copy-on-write
//!   replace, delete and rename, and a table kept in two copies.
//!
//! What both share is here: the block device, the errors and the CRC.
//! Every LBA is relative to the partition, and all access goes through
//! [`BlockDevice`], so both are tested on the host over a `Vec`, including
//! power cuts at every write.

#![no_std]

pub mod descriptors;
pub mod v1;
pub mod v2;

pub use v1::{AriFs, Entry, Journal, Recovery, Superblock};
pub use v1::{
    DATA_LBA, ENTRY_LEN, FILE_CAPACITY_SECTORS, JOURNAL_CAPACITY_BYTES, MAX_FILES, TABLE_LBA,
};

pub const SECTOR: usize = 512;
/// Longest file name, in both versions.
pub const NAME_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Io,
    BadMagic,
    /// A metadata CRC does not match: not mounted.
    Corrupt,
    NotFound,
    Exists,
    NoSpace,
    BadName,
    /// Read or write beyond the file capacity.
    OutOfRange,
    /// Commit of a chunked replace that did not start at zero, has gaps or
    /// does not cover the committed length (audit 6ee19fb, 1).
    BadTransaction,
}

/// 512-byte block device. `flush` makes previous writes durable before
/// returning.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError>;
    fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError>;
    fn flush(&mut self) -> Result<(), FsError>;
    /// Several consecutive sectors (`buf.len()` a multiple of `SECTOR`, up to
    /// `MAX_BATCH_SECTORS`); one at a time by default. An IPC device groups
    /// them into one request (stage 5).
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        for (i, chunk) in buf.as_chunks_mut::<SECTOR>().0.iter_mut().enumerate() {
            self.read_sector(lba + i as u64, chunk)?;
        }
        Ok(())
    }
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        for (i, chunk) in buf.as_chunks::<SECTOR>().0.iter().enumerate() {
            self.write_sector(lba + i as u64, chunk)?;
        }
        Ok(())
    }
}

/// Sectors per batch in `read_sectors`/`write_sectors`.
pub const MAX_BATCH_SECTORS: usize = 8;
pub(crate) const BATCH_BYTES: usize = MAX_BATCH_SECTORS * SECTOR;

/// Incremental CRC-32 (IEEE 802.3, reflected), without a table: the metadata
/// is small. Starts at `crc32_init()` and ends with `!`.
pub(crate) const fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    let mut i = 0;
    while i < data.len() {
        crc ^= data[i] as u32;
        let mut k = 0;
        while k < 8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            k += 1;
        }
        i += 1;
    }
    crc
}

pub fn crc32(data: &[u8]) -> u32 {
    !crc32_update(0xFFFF_FFFF, data)
}

pub(crate) fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

pub(crate) fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
}

/// Copies `out.len()` bytes from position `pos` (bytes) of the area that
/// starts at `base`: partial sectors one at a time, whole ones in batches.
pub(crate) fn read_area<D: BlockDevice>(
    dev: &mut D,
    base: u64,
    pos: usize,
    out: &mut [u8],
) -> Result<(), FsError> {
    let mut done = 0;
    let mut s = [0u8; SECTOR];
    while done < out.len() {
        let at = pos + done;
        let lba = base + (at / SECTOR) as u64;
        let in_sector = at % SECTOR;
        let left = out.len() - done;
        if in_sector == 0 && left >= SECTOR {
            let n = (left / SECTOR).min(MAX_BATCH_SECTORS) * SECTOR;
            dev.read_sectors(lba, &mut out[done..done + n])?;
            done += n;
        } else {
            dev.read_sector(lba, &mut s)?;
            let n = (SECTOR - in_sector).min(left);
            out[done..done + n].copy_from_slice(&s[in_sector..in_sector + n]);
            done += n;
        }
    }
    Ok(())
}

/// Writes `data` at position `pos` of the area that starts at `base`:
/// partial sectors are read first; whole ones go in batches.
pub(crate) fn write_area<D: BlockDevice>(
    dev: &mut D,
    base: u64,
    pos: usize,
    data: &[u8],
) -> Result<(), FsError> {
    let mut done = 0;
    let mut s = [0u8; SECTOR];
    while done < data.len() {
        let at = pos + done;
        let lba = base + (at / SECTOR) as u64;
        let in_sector = at % SECTOR;
        let left = data.len() - done;
        if in_sector == 0 && left >= SECTOR {
            let n = (left / SECTOR).min(MAX_BATCH_SECTORS) * SECTOR;
            dev.write_sectors(lba, &data[done..done + n])?;
            done += n;
        } else {
            dev.read_sector(lba, &mut s)?;
            let n = (SECTOR - in_sector).min(left);
            s[in_sector..in_sector + n].copy_from_slice(&data[done..done + n]);
            dev.write_sector(lba, &s)?;
            done += n;
        }
    }
    Ok(())
}
