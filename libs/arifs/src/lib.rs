// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriFS v1 (Phase 20 and stage 2, ADR 0014): a flat directory of up to 32
//! fixed-capacity files, without fragmentation, with checksums on the
//! metadata, an atomic replace journal and a `flush` contract.
//!
//! ```text
//! LBA 0        superblock (512 bytes): magic, version, sectors, next
//!              data LBA, CRC32
//! LBA 1..4     table: 32 entries of 64 bytes (name, LBA, capacity,
//!              size, per-entry CRC32)
//! LBA 5        journal header: state, file, length, data CRC32,
//!              sequence, header CRC32
//! LBA 6..69    journal data (the capacity of one file)
//! LBA 70..     data: fixed-capacity blocks allocated incrementally
//! ```
//!
//! Persistence contract:
//!
//! - a 512-byte sector is written whole or not at all (an assumption about
//!   the medium); nothing is assumed about ordering between sectors except after `flush`;
//! - `write` writes in place: after a power cut each sector holds the old
//!   or the new contents, and the size only grows once the data is there;
//! - `replace` is atomic: after a power cut the file holds either the whole
//!   previous contents or the whole new ones. The data and its CRC go to the
//!   journal first, then it is marked pending, applied and cleared, with `flush`
//!   between phases; `open` redoes a valid pending replace and discards a
//!   broken header (the replace was never committed);
//! - `open` rejects (`Corrupt`) a superblock or an in-use entry whose CRC
//!   does not match: a file system that cannot be trusted is not mounted.
//!
//! LBAs are relative to the partition. All access goes through
//! [`BlockDevice`], so the library is tested on the host over a `Vec`,
//! including power cuts at every write of the replace.

#![no_std]

pub mod descriptors;

pub const SECTOR: usize = 512;
const MAGIC: &[u8; 8] = b"ARIFS\x00\x00\x02";
const JOURNAL_MAGIC: &[u8; 8] = b"ARIJRNL\x00";
pub const MAX_FILES: usize = 32;
pub const ENTRY_LEN: usize = 64;
pub const NAME_LEN: usize = 32;
pub const TABLE_LBA: u64 = 1;
pub const TABLE_SECTORS: u64 = (MAX_FILES * ENTRY_LEN / SECTOR) as u64;
/// Capacity per file: 64 sectors (32 KiB).
pub const FILE_CAPACITY_SECTORS: u32 = 64;
pub const JOURNAL_LBA: u64 = TABLE_LBA + TABLE_SECTORS;
pub const JOURNAL_DATA_LBA: u64 = JOURNAL_LBA + 1;
pub const DATA_LBA: u64 = JOURNAL_DATA_LBA + FILE_CAPACITY_SECTORS as u64;
/// Maximum size of an atomic replace: the journal data area.
pub const JOURNAL_CAPACITY_BYTES: usize = FILE_CAPACITY_SECTORS as usize * SECTOR;

const JOURNAL_CLEAR: u32 = 0;
const JOURNAL_PENDING: u32 = 1;

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
const BATCH_BYTES: usize = MAX_BATCH_SECTORS * SECTOR;

/// Incremental CRC-32 (IEEE 802.3, reflected), without a table: the metadata
/// is small. Starts at `crc32_init()` and ends with `!`.
const fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
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

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub total_sectors: u64,
    /// Next free LBA for data.
    pub data_next: u64,
}

impl Superblock {
    fn encode(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(MAGIC);
        s[8..16].copy_from_slice(&self.total_sectors.to_le_bytes());
        s[16..24].copy_from_slice(&self.data_next.to_le_bytes());
        let crc = crc32(&s[..SECTOR - 4]);
        s[SECTOR - 4..].copy_from_slice(&crc.to_le_bytes());
        s
    }
    fn decode(s: &[u8; SECTOR]) -> Result<Self, FsError> {
        if &s[..8] != MAGIC {
            return Err(FsError::BadMagic);
        }
        if u32_at(s, SECTOR - 4) != crc32(&s[..SECTOR - 4]) {
            return Err(FsError::Corrupt);
        }
        Ok(Self {
            total_sectors: u64_at(s, 8),
            data_next: u64_at(s, 16),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub name: [u8; NAME_LEN],
    pub name_len: u8,
    pub start_lba: u64,
    pub capacity_sectors: u32,
    pub size: u32,
}

impl Entry {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }
    pub fn capacity_bytes(&self) -> u32 {
        self.capacity_sectors * SECTOR as u32
    }
    /// The CRC (bytes 36..40) covers the rest of the entry.
    fn encode(&self) -> [u8; ENTRY_LEN] {
        let mut e = [0u8; ENTRY_LEN];
        e[..NAME_LEN].copy_from_slice(&self.name);
        e[32] = self.name_len;
        e[33] = 1; // in use
        e[40..48].copy_from_slice(&self.start_lba.to_le_bytes());
        e[48..52].copy_from_slice(&self.capacity_sectors.to_le_bytes());
        e[52..56].copy_from_slice(&self.size.to_le_bytes());
        let crc = Self::checksum(&e);
        e[36..40].copy_from_slice(&crc.to_le_bytes());
        e
    }
    fn checksum(e: &[u8]) -> u32 {
        !crc32_update(crc32_update(0xFFFF_FFFF, &e[..36]), &e[40..ENTRY_LEN])
    }
    /// `Ok(None)` if the entry is free; `Corrupt` if it is in use and the
    /// CRC does not match.
    fn decode(e: &[u8]) -> Result<Option<Self>, FsError> {
        if e[33] == 0 {
            return Ok(None);
        }
        if u32_at(e, 36) != Self::checksum(e) {
            return Err(FsError::Corrupt);
        }
        let mut name = [0u8; NAME_LEN];
        name.copy_from_slice(&e[..NAME_LEN]);
        Ok(Some(Self {
            name,
            name_len: e[32].min(NAME_LEN as u8),
            start_lba: u64_at(e, 40),
            capacity_sectors: u32_at(e, 48),
            size: u32_at(e, 52),
        }))
    }
}

/// Replace journal header (one sector). Opaque outside the library: only
/// `stage_replace` returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journal {
    state: u32,
    index: u32,
    len: u32,
    data_crc: u32,
    sequence: u64,
}

impl Journal {
    const CLEAR: Journal = Journal {
        state: JOURNAL_CLEAR,
        index: 0,
        len: 0,
        data_crc: 0,
        sequence: 0,
    };

    fn encode(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(JOURNAL_MAGIC);
        s[8..12].copy_from_slice(&self.state.to_le_bytes());
        s[12..16].copy_from_slice(&self.index.to_le_bytes());
        s[16..20].copy_from_slice(&self.len.to_le_bytes());
        s[20..24].copy_from_slice(&self.data_crc.to_le_bytes());
        s[24..32].copy_from_slice(&self.sequence.to_le_bytes());
        let crc = crc32(&s[..SECTOR - 4]);
        s[SECTOR - 4..].copy_from_slice(&crc.to_le_bytes());
        s
    }

    /// `None` if the header is not valid (magic or CRC): equivalent to clean.
    fn decode(s: &[u8; SECTOR]) -> Option<Self> {
        if &s[..8] != JOURNAL_MAGIC || u32_at(s, SECTOR - 4) != crc32(&s[..SECTOR - 4]) {
            return None;
        }
        Some(Self {
            state: u32_at(s, 8),
            index: u32_at(s, 12),
            len: u32_at(s, 16),
            data_crc: u32_at(s, 20),
            sequence: u64_at(s, 24),
        })
    }
}

/// Iteration over the in-use entries of a table (with index).
mod alloc_free {
    use super::{Entry, MAX_FILES};
    pub struct Entries<'a>(pub &'a [Option<Entry>; MAX_FILES]);
    impl<'a> Entries<'a> {
        pub fn iter(&self) -> impl Iterator<Item = (usize, &'a Entry)> + '_ {
            self.0
                .iter()
                .enumerate()
                .filter_map(|(i, e)| e.as_ref().map(|e| (i, e)))
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
}

/// What `open` found in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Clean journal.
    Clean,
    /// A committed replace that was not fully applied was redone.
    Replayed { index: usize },
    /// Broken header or data not matching its CRC: the replace was never
    /// committed and was discarded.
    Discarded,
}

/// A file system opened on a device.
pub struct AriFs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
    table: [Option<Entry>; MAX_FILES],
    sequence: u64,
    recovery: Recovery,
    /// Chunked replace in progress: file and next expected offset. Only what
    /// this transaction wrote gets committed.
    staged: Option<(usize, u32)>,
}

impl<D: BlockDevice> AriFs<D> {
    /// Opens an existing file system (reads superblock and table, checks
    /// the CRCs and redoes or discards a pending replace).
    pub fn open(mut dev: D) -> Result<Self, FsError> {
        let mut s = [0u8; SECTOR];
        dev.read_sector(0, &mut s)?;
        let sb = Superblock::decode(&s)?;
        let mut table = [None; MAX_FILES];
        for t in 0..TABLE_SECTORS {
            dev.read_sector(TABLE_LBA + t, &mut s)?;
            for (i, e) in s.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
                table[t as usize * (SECTOR / ENTRY_LEN) + i] = Entry::decode(e)?;
            }
        }
        Self::check_layout(&sb, &table)?;
        let mut fs = Self {
            dev,
            sb,
            table,
            sequence: 0,
            recovery: Recovery::Clean,
            staged: None,
        };
        fs.recover()?;
        Ok(fs)
    }

    /// The metadata must be mutually consistent, not just carry a CRC:
    /// every file inside the allocated area and not overlapping another
    /// (audit 6ee19fb, finding 2).
    fn check_layout(sb: &Superblock, table: &[Option<Entry>; MAX_FILES]) -> Result<(), FsError> {
        if sb.data_next < DATA_LBA || sb.data_next > sb.total_sectors {
            return Err(FsError::Corrupt);
        }
        let used: alloc_free::Entries<'_> = alloc_free::Entries(table);
        for (i, a) in used.iter() {
            let a_end = a.start_lba + a.capacity_sectors as u64;
            if a.capacity_sectors == 0 || a.start_lba < DATA_LBA || a_end > sb.data_next {
                return Err(FsError::Corrupt);
            }
            if a.size > a.capacity_bytes() {
                return Err(FsError::Corrupt);
            }
            for (j, b) in used.iter() {
                if j > i
                    && a.start_lba < b.start_lba + b.capacity_sectors as u64
                    && b.start_lba < a_end
                {
                    return Err(FsError::Corrupt);
                }
            }
        }
        Ok(())
    }

    /// Creates an empty file system on `total_sectors` sectors.
    pub fn format(mut dev: D, total_sectors: u64) -> Result<Self, FsError> {
        if total_sectors < DATA_LBA + FILE_CAPACITY_SECTORS as u64 {
            return Err(FsError::NoSpace);
        }
        let sb = Superblock {
            total_sectors,
            data_next: DATA_LBA,
        };
        dev.write_sector(0, &sb.encode())?;
        let zero = [0u8; SECTOR];
        for t in 0..TABLE_SECTORS {
            dev.write_sector(TABLE_LBA + t, &zero)?;
        }
        dev.write_sector(JOURNAL_LBA, &Journal::CLEAR.encode())?;
        dev.flush()?;
        Ok(Self {
            dev,
            sb,
            table: [None; MAX_FILES],
            sequence: 0,
            recovery: Recovery::Clean,
            staged: None,
        })
    }

    pub fn superblock(&self) -> Superblock {
        self.sb
    }

    /// Result of the recovery on open.
    pub fn recovery(&self) -> Recovery {
        self.recovery
    }

    pub fn entries(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.table
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| (i, e)))
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.entries()
            .find(|(_, e)| e.name_str() == name)
            .map(|(i, _)| i)
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.table.get(index).and_then(|e| e.as_ref())
    }

    fn write_table_entry(&mut self, index: usize) -> Result<(), FsError> {
        let per_sector = SECTOR / ENTRY_LEN;
        let lba = TABLE_LBA + (index / per_sector) as u64;
        let mut s = [0u8; SECTOR];
        self.dev.read_sector(lba, &mut s)?;
        let off = (index % per_sector) * ENTRY_LEN;
        let bytes = match self.table[index] {
            Some(e) => e.encode(),
            None => [0u8; ENTRY_LEN],
        };
        s[off..off + ENTRY_LEN].copy_from_slice(&bytes);
        self.dev.write_sector(lba, &s)
    }

    /// Creates an empty file with the usual fixed capacity. Returns its index.
    pub fn create(&mut self, name: &str) -> Result<usize, FsError> {
        self.create_sized(name, FILE_CAPACITY_SECTORS)
    }

    /// Like `create`, with capacity `sectors` (a multiple of
    /// `FILE_CAPACITY_SECTORS`): large files, such as installed packages
    /// (ADR 0017). `replace` only supports up to the journal capacity
    /// (`FILE_CAPACITY_SECTORS`).
    pub fn create_sized(&mut self, name: &str, sectors: u32) -> Result<usize, FsError> {
        if sectors == 0 || !sectors.is_multiple_of(FILE_CAPACITY_SECTORS) {
            return Err(FsError::OutOfRange);
        }
        if !valid_name(name) {
            return Err(FsError::BadName);
        }
        if self.find(name).is_some() {
            return Err(FsError::Exists);
        }
        let index = self
            .table
            .iter()
            .position(Option::is_none)
            .ok_or(FsError::NoSpace)?;
        let start = self.sb.data_next;
        if start + sectors as u64 > self.sb.total_sectors {
            return Err(FsError::NoSpace);
        }
        let mut entry = Entry {
            name: [0; NAME_LEN],
            name_len: name.len() as u8,
            start_lba: start,
            capacity_sectors: sectors,
            size: 0,
        };
        entry.name[..name.len()].copy_from_slice(name.as_bytes());
        self.table[index] = Some(entry);
        self.sb.data_next = start + sectors as u64;
        // Superblock first, then a barrier: the medium may reorder sectors
        // until `flush`, and an entry published over unreserved space would
        // leave two overlapping files when it is allocated again
        // (audit 6ee19fb, finding 2). A cut between both leaves reserved
        // space without an entry, which breaks nothing.
        self.dev.write_sector(0, &self.sb.encode())?;
        self.dev.flush()?;
        self.write_table_entry(index)?;
        Ok(index)
    }

    /// Reads up to `buf.len()` bytes from `offset`. Returns the bytes read
    /// (0 at end of file).
    pub fn read(&mut self, index: usize, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        if offset >= e.size {
            return Ok(0);
        }
        let len = buf.len().min((e.size - offset) as usize);
        self.copy_from_device(e.start_lba, offset as usize, &mut buf[..len])?;
        Ok(len)
    }

    /// Copies `out.len()` bytes from position `pos` (bytes) of the area that
    /// starts at `base`: partial sectors one at a time, whole ones in batches.
    fn copy_from_device(&mut self, base: u64, pos: usize, out: &mut [u8]) -> Result<(), FsError> {
        let mut done = 0;
        let mut s = [0u8; SECTOR];
        while done < out.len() {
            let at = pos + done;
            let lba = base + (at / SECTOR) as u64;
            let in_sector = at % SECTOR;
            let left = out.len() - done;
            if in_sector == 0 && left >= SECTOR {
                let n = (left / SECTOR).min(MAX_BATCH_SECTORS) * SECTOR;
                self.dev.read_sectors(lba, &mut out[done..done + n])?;
                done += n;
            } else {
                self.dev.read_sector(lba, &mut s)?;
                let n = (SECTOR - in_sector).min(left);
                out[done..done + n].copy_from_slice(&s[in_sector..in_sector + n]);
                done += n;
            }
        }
        Ok(())
    }

    /// Writes `data` at position `pos` of the area that starts at `base`:
    /// partial sectors are read first; whole ones go in batches.
    fn copy_to_device(&mut self, base: u64, pos: usize, data: &[u8]) -> Result<(), FsError> {
        let mut done = 0;
        let mut s = [0u8; SECTOR];
        while done < data.len() {
            let at = pos + done;
            let lba = base + (at / SECTOR) as u64;
            let in_sector = at % SECTOR;
            let left = data.len() - done;
            if in_sector == 0 && left >= SECTOR {
                let n = (left / SECTOR).min(MAX_BATCH_SECTORS) * SECTOR;
                self.dev.write_sectors(lba, &data[done..done + n])?;
                done += n;
            } else {
                self.dev.read_sector(lba, &mut s)?;
                let n = (SECTOR - in_sector).min(left);
                s[in_sector..in_sector + n].copy_from_slice(&data[done..done + n]);
                self.dev.write_sector(lba, &s)?;
                done += n;
            }
        }
        Ok(())
    }

    /// Writes `data` at `offset` (no gaps: `offset <= size`) within the
    /// capacity, in place. Updates the size after the data. Not durable
    /// until `flush`.
    pub fn write(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        if offset > e.size {
            return Err(FsError::OutOfRange);
        }
        let end = offset as usize + data.len();
        if end > e.capacity_bytes() as usize {
            return Err(FsError::OutOfRange);
        }
        self.copy_to_device(e.start_lba, offset as usize, data)?;
        if end as u32 > e.size {
            // The data must be durable before publishing the size that
            // covers it: without a barrier, a cut could keep the size and
            // lose the data (audit 6ee19fb, finding 2).
            self.dev.flush()?;
            let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
            entry.size = end as u32;
            self.write_table_entry(index)?;
        }
        Ok(data.len())
    }

    /// Atomically replaces the whole file contents with `data` (replace
    /// journal): after a cut the file holds the previous contents or the
    /// new ones, never a mix. Durable on return.
    pub fn replace(&mut self, index: usize, data: &[u8]) -> Result<(), FsError> {
        self.replace_chunk(index, 0, data)?;
        self.replace_commit(index, data.len() as u32)
    }

    /// One chunk of the new contents into the journal data area, without
    /// committing: nothing changes until `replace_commit`. Several chunks (in
    /// any order, within the journal and file capacity) make up a replace
    /// larger than one IPC request (ADR 0018).
    pub fn replace_chunk(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<(), FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        let end = offset as usize + data.len();
        if end > e.capacity_bytes() as usize || end > JOURNAL_CAPACITY_BYTES {
            return Err(FsError::OutOfRange);
        }
        // A transaction starts at zero and continues without gaps: that way
        // the commit can only cover bytes it wrote, never leftovers of
        // another replace (audit 6ee19fb, finding 1).
        if offset == 0 {
            self.staged = Some((index, 0));
        }
        match self.staged {
            Some((i, next)) if i == index && next == offset => {}
            _ => return Err(FsError::BadTransaction),
        }
        self.copy_to_device(JOURNAL_DATA_LBA, offset as usize, data)?;
        self.staged = Some((index, end as u32));
        Ok(())
    }

    /// Discards the chunked replace in progress, if any (the owner went
    /// away or took too long): the next chunk must start at zero.
    pub fn abandon_replace(&mut self) {
        self.staged = None;
    }

    /// Commits the first `len` bytes in the journal as the new contents of
    /// the file and applies them (idempotent: `open` repeats it if
    /// needed).
    pub fn replace_commit(&mut self, index: usize, len: u32) -> Result<(), FsError> {
        let journal = self.commit_header(index, len)?;
        self.apply_journal(&journal)
    }

    /// The first two phases of `replace`: leaves a committed replace in the
    /// journal without applying it. It is what stays on disk after a cut
    /// right after the commit; xtask uses it to build recovery test disks
    /// and the next `open` redoes it.
    pub fn stage_replace(&mut self, index: usize, data: &[u8]) -> Result<Journal, FsError> {
        self.replace_chunk(index, 0, data)?;
        self.commit_header(index, data.len() as u32)
    }

    /// Makes the journal data durable and writes the pending header with
    /// its CRC (read from the journal itself: it is what will be applied).
    fn commit_header(&mut self, index: usize, len: u32) -> Result<Journal, FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        if len > e.capacity_bytes() || len as usize > JOURNAL_CAPACITY_BYTES {
            return Err(FsError::OutOfRange);
        }
        if self.staged != Some((index, len)) {
            return Err(FsError::BadTransaction);
        }
        self.staged = None;
        self.dev.flush()?;
        let data_crc = self.journal_crc(len)?;
        self.sequence += 1;
        let journal = Journal {
            state: JOURNAL_PENDING,
            index: index as u32,
            len,
            data_crc,
            sequence: self.sequence,
        };
        self.dev.write_sector(JOURNAL_LBA, &journal.encode())?;
        self.dev.flush()?;
        Ok(journal)
    }

    /// Copies the journal data into the file, sets the size and clears the
    /// header, with `flush` between both phases.
    fn apply_journal(&mut self, journal: &Journal) -> Result<(), FsError> {
        let index = journal.index as usize;
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        let sectors = (journal.len as usize).div_ceil(SECTOR);
        let mut batch = [0u8; BATCH_BYTES];
        let mut done = 0;
        while done < sectors {
            let n = (sectors - done).min(MAX_BATCH_SECTORS);
            let buf = &mut batch[..n * SECTOR];
            self.dev.read_sectors(JOURNAL_DATA_LBA + done as u64, buf)?;
            self.dev.write_sectors(e.start_lba + done as u64, buf)?;
            done += n;
        }
        let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
        entry.size = journal.len;
        self.write_table_entry(index)?;
        self.dev.flush()?;
        // Clearing does not need `flush`: if it is lost, the next `open`
        // redoes an already applied replace (idempotent). Each `flush` is an
        // fsync of the medium and dominates the cost of the replace (stage 5).
        self.dev.write_sector(JOURNAL_LBA, &Journal::CLEAR.encode())
    }

    /// Redoes a committed replace (valid pending header, data matching its
    /// CRC) or discards one that was never committed.
    fn recover(&mut self) -> Result<(), FsError> {
        let mut s = [0u8; SECTOR];
        self.dev.read_sector(JOURNAL_LBA, &mut s)?;
        let Some(journal) = Journal::decode(&s) else {
            self.recovery = Recovery::Discarded;
            return self.clear_journal();
        };
        self.sequence = journal.sequence;
        if journal.state != JOURNAL_PENDING {
            return Ok(());
        }
        let index = journal.index as usize;
        let fits = self
            .entry(index)
            .is_some_and(|e| journal.len <= e.capacity_bytes());
        if !fits || !self.journal_data_matches(&journal)? {
            self.recovery = Recovery::Discarded;
            return self.clear_journal();
        }
        self.apply_journal(&journal)?;
        self.recovery = Recovery::Replayed { index };
        Ok(())
    }

    fn journal_data_matches(&mut self, journal: &Journal) -> Result<bool, FsError> {
        Ok(self.journal_crc(journal.len)? == journal.data_crc)
    }

    /// CRC-32 of the first `len` bytes of the journal data area.
    fn journal_crc(&mut self, len: u32) -> Result<u32, FsError> {
        let mut crc = 0xFFFF_FFFFu32;
        let mut batch = [0u8; BATCH_BYTES];
        let mut left = len as usize;
        let mut lba = JOURNAL_DATA_LBA;
        while left > 0 {
            let sectors = left.div_ceil(SECTOR).min(MAX_BATCH_SECTORS);
            let buf = &mut batch[..sectors * SECTOR];
            self.dev.read_sectors(lba, buf)?;
            let n = left.min(buf.len());
            crc = crc32_update(crc, &buf[..n]);
            left -= n;
            lba += sectors as u64;
        }
        Ok(!crc)
    }

    fn clear_journal(&mut self) -> Result<(), FsError> {
        self.dev
            .write_sector(JOURNAL_LBA, &Journal::CLEAR.encode())?;
        self.dev.flush()
    }

    /// Truncates to zero bytes.
    pub fn truncate(&mut self, index: usize) -> Result<(), FsError> {
        let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
        entry.size = 0;
        self.write_table_entry(index)
    }

    /// Makes previous writes durable (`write`, `create`, `truncate`).
    pub fn flush(&mut self) -> Result<(), FsError> {
        self.dev.flush()
    }

    pub fn into_device(self) -> D {
        self.dev
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    struct MemDisk(Vec<u8>);
    impl BlockDevice for MemDisk {
        fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
            let o = lba as usize * SECTOR;
            buf.copy_from_slice(self.0.get(o..o + SECTOR).ok_or(FsError::Io)?);
            Ok(())
        }
        fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
            let o = lba as usize * SECTOR;
            self.0
                .get_mut(o..o + SECTOR)
                .ok_or(FsError::Io)?
                .copy_from_slice(buf);
            Ok(())
        }
        fn flush(&mut self) -> Result<(), FsError> {
            Ok(())
        }
    }

    fn disk(sectors: usize) -> MemDisk {
        MemDisk(std::vec![0u8; sectors * SECTOR])
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn format_create_write_read_and_reopen() {
        let fs = AriFs::format(disk(200), 200).unwrap();
        let mut fs = fs;
        let i = fs.create("hola.txt").unwrap();
        assert_eq!(fs.write(i, 0, b"Hola desde AriFS\n").unwrap(), 17);
        assert_eq!(fs.entry(i).unwrap().size, 17);
        let mut buf = [0u8; 64];
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 17);
        assert_eq!(&buf[..17], b"Hola desde AriFS\n");
        assert_eq!(fs.read(i, 5, &mut buf).unwrap(), 12);
        assert_eq!(&buf[..12], b"desde AriFS\n");
        assert_eq!(fs.read(i, 17, &mut buf).unwrap(), 0, "end of file");
        // Reopen from the device: table and superblock persisted.
        let dev = fs.into_device();
        let mut fs = AriFs::open(dev).unwrap();
        assert_eq!(fs.recovery(), Recovery::Clean);
        assert_eq!(fs.find("hola.txt"), Some(i));
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 17);
        assert_eq!(
            fs.superblock().data_next,
            DATA_LBA + FILE_CAPACITY_SECTORS as u64
        );
    }

    #[test]
    fn writes_across_sectors_and_append() {
        let mut fs = AriFs::format(disk(200), 200).unwrap();
        let i = fs.create("big").unwrap();
        let data: Vec<u8> = (0..1500u32).map(|x| (x % 251) as u8).collect();
        assert_eq!(fs.write(i, 0, &data).unwrap(), 1500);
        assert_eq!(fs.write(i, 1500, b"tail").unwrap(), 4);
        let mut buf = std::vec![0u8; 2000];
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 1504);
        assert_eq!(&buf[..1500], &data[..]);
        assert_eq!(&buf[1500..1504], b"tail");
        // Partial overwrite in the middle.
        assert_eq!(fs.write(i, 510, b"XXXX").unwrap(), 4);
        assert_eq!(fs.read(i, 508, &mut buf[..8]).unwrap(), 8);
        assert_eq!(&buf[2..6], b"XXXX");
        assert_eq!(fs.entry(i).unwrap().size, 1504);
        fs.truncate(i).unwrap();
        assert_eq!(fs.entry(i).unwrap().size, 0);
        fs.flush().unwrap();
    }

    #[test]
    fn errors() {
        let mut fs = AriFs::format(disk(200), 200).unwrap();
        assert_eq!(fs.create("con espacio"), Err(FsError::BadName));
        assert_eq!(fs.create("/abs"), Err(FsError::BadName));
        assert_eq!(fs.create("dir/"), Err(FsError::BadName));
        assert_eq!(fs.create("a//b"), Err(FsError::BadName));
        assert!(fs.create("app/notas").is_ok(), "private directory prefix");
        assert_eq!(fs.create(""), Err(FsError::BadName));
        let i = fs.create("a").unwrap();
        assert_eq!(fs.create("a"), Err(FsError::Exists));
        assert_eq!(fs.write(i, 10, b"x"), Err(FsError::OutOfRange), "gap");
        let too_big = std::vec![0u8; FILE_CAPACITY_SECTORS as usize * SECTOR + 1];
        assert_eq!(fs.write(i, 0, &too_big), Err(FsError::OutOfRange));
        assert_eq!(fs.replace(i, &too_big), Err(FsError::OutOfRange));
        assert_eq!(fs.read(99, 0, &mut [0u8; 4]), Err(FsError::NotFound));
        assert_eq!(fs.replace(99, b"x"), Err(FsError::NotFound));
        assert_eq!(fs.find("zzz"), None);
        // Disk full: no room for more 64-sector files.
        let mut fs = AriFs::format(disk(2000), 2000).unwrap();
        fs.create("a").unwrap();
        let mut created = 1;
        while fs.create(&std::format!("f{created}")).is_ok() {
            created += 1;
        }
        assert!(created > 5 && created <= MAX_FILES);
        assert_eq!(
            created as u64,
            (2000 - DATA_LBA) / FILE_CAPACITY_SECTORS as u64
        );
        assert!(
            fs.replace(0, b"aun cabe").is_ok(),
            "the journal does not depend on data space"
        );
        assert_eq!(AriFs::open(disk(10)).err(), Some(FsError::BadMagic));
        assert_eq!(AriFs::format(disk(5), 5).err(), Some(FsError::NoSpace));
    }

    #[test]
    fn replace_is_visible_and_durable() {
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("notas").unwrap();
        fs.write(i, 0, b"version anterior de las notas").unwrap();
        let long: Vec<u8> = (0..1500u32).map(|x| (x % 7) as u8 + b'a').collect();
        fs.replace(i, &long).unwrap();
        let mut buf = std::vec![0u8; 2000];
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 1500);
        assert_eq!(&buf[..1500], &long[..]);
        // Shorter than the previous one: the size shrinks.
        fs.replace(i, b"corto").unwrap();
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"corto");
        let mut fs = AriFs::open(fs.into_device()).unwrap();
        assert_eq!(fs.recovery(), Recovery::Clean);
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"corto");
        // Empty works too.
        fs.replace(i, b"").unwrap();
        assert_eq!(fs.entry(i).unwrap().size, 0);
    }

    /// Disk with a volatile cache and a scheduled cut: writes stay in the
    /// cache until `flush`; from write `cut_at` on everything returns `Io` and,
    /// on "power off", the cache is lost (`lose_cache`) or kept (the medium
    /// managed to write it). Both extremes bound any lost subset because
    /// every phase of the replace ends in `flush`.
    struct CuttingDisk {
        persisted: Vec<u8>,
        cache: Vec<(u64, [u8; SECTOR])>,
        writes: usize,
        cut_at: usize,
        cut: bool,
    }

    impl CuttingDisk {
        fn new(base: &[u8], cut_at: usize) -> Self {
            Self {
                persisted: base.to_vec(),
                cache: Vec::new(),
                writes: 0,
                cut_at,
                cut: false,
            }
        }
        fn power_off(mut self, lose_cache: bool) -> MemDisk {
            if !lose_cache {
                for (lba, s) in &self.cache {
                    let o = *lba as usize * SECTOR;
                    self.persisted[o..o + SECTOR].copy_from_slice(s);
                }
            }
            MemDisk(self.persisted)
        }
    }

    impl BlockDevice for CuttingDisk {
        fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
            if let Some((_, s)) = self.cache.iter().rev().find(|(l, _)| *l == lba) {
                buf.copy_from_slice(s);
                return Ok(());
            }
            let o = lba as usize * SECTOR;
            buf.copy_from_slice(self.persisted.get(o..o + SECTOR).ok_or(FsError::Io)?);
            Ok(())
        }
        fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
            if self.writes >= self.cut_at {
                self.cut = true;
                return Err(FsError::Io);
            }
            self.writes += 1;
            self.cache.push((lba, *buf));
            Ok(())
        }
        fn flush(&mut self) -> Result<(), FsError> {
            if self.cut {
                return Err(FsError::Io);
            }
            for (lba, s) in self.cache.drain(..) {
                let o = lba as usize * SECTOR;
                self.persisted[o..o + SECTOR].copy_from_slice(&s);
            }
            Ok(())
        }
    }

    fn content(fs: &mut AriFs<MemDisk>, i: usize) -> Vec<u8> {
        let mut buf = std::vec![0u8; 40_000];
        let n = fs.read(i, 0, &mut buf).unwrap();
        buf.truncate(n);
        buf
    }

    #[test]
    fn replace_survives_a_cut_at_every_write() {
        let old: Vec<u8> = (0..1300u32).map(|x| (x % 13) as u8 + b'A').collect();
        let new: Vec<u8> = (0..2100u32).map(|x| (x % 17) as u8 + b'a').collect();
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("notas").unwrap();
        fs.write(i, 0, &old).unwrap();
        let base = fs.into_device().0;
        // No cut: counts the writes of a complete replace.
        let mut fs = AriFs::open(CuttingDisk::new(&base, usize::MAX)).unwrap();
        fs.replace(i, &new).unwrap();
        let total = fs.into_device().writes;
        assert!(
            total > 8,
            "the replace writes data, header, file, table and cleanup"
        );
        let mut replayed = 0;
        for cut_at in 0..=total {
            for lose_cache in [true, false] {
                let mut fs = AriFs::open(CuttingDisk::new(&base, cut_at)).unwrap();
                let result = fs.replace(i, &new);
                assert_eq!(result.is_ok(), cut_at >= total, "cut at {cut_at}");
                let mut fs = AriFs::open(fs.into_device().power_off(lose_cache)).unwrap();
                let got = content(&mut fs, i);
                match fs.recovery() {
                    Recovery::Replayed { index } => {
                        assert_eq!(index, i);
                        assert_eq!(got, new, "cut at {cut_at}: redone");
                        replayed += 1;
                    }
                    Recovery::Discarded => assert_eq!(got, old, "cut at {cut_at}: discarded"),
                    Recovery::Clean => assert!(
                        got == old || got == new,
                        "cut at {cut_at} (cache lost {lose_cache}): mixed contents"
                    ),
                }
                // After recovery, the system keeps serving.
                fs.replace(i, b"despues").unwrap();
                assert_eq!(content(&mut fs, i), b"despues");
                let mut fs = AriFs::open(fs.into_device()).unwrap();
                assert_eq!(fs.recovery(), Recovery::Clean);
                assert_eq!(content(&mut fs, i), b"despues");
            }
        }
        assert!(
            replayed > 0,
            "some cut leaves a committed replace half done"
        );
    }

    #[test]
    fn chunked_replace_is_atomic_too() {
        let old: Vec<u8> = (0..900u32).map(|x| (x % 11) as u8 + b'A').collect();
        let new: Vec<u8> = (0..9000u32).map(|x| (x % 19) as u8 + b'a').collect();
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("pkg").unwrap();
        fs.write(i, 0, &old).unwrap();
        // 4 KiB chunks, like the IPC client, and a partial last one.
        for (k, chunk) in new.chunks(4096).enumerate() {
            fs.replace_chunk(i, (k * 4096) as u32, chunk).unwrap();
            assert_eq!(
                content(&mut fs, i),
                old,
                "nothing changes before the commit"
            );
        }
        fs.replace_commit(i, new.len() as u32).unwrap();
        assert_eq!(content(&mut fs, i), new);
        assert_eq!(
            fs.replace_chunk(i, JOURNAL_CAPACITY_BYTES as u32 - 1, b"xx"),
            Err(FsError::OutOfRange)
        );
        // Cut at every write of the chunked replace: old or new.
        let base = fs.into_device().0;
        let mut fs = AriFs::open(CuttingDisk::new(&base, usize::MAX)).unwrap();
        for (k, chunk) in old.chunks(4096).enumerate() {
            fs.replace_chunk(i, (k * 4096) as u32, chunk).unwrap();
        }
        fs.replace_commit(i, old.len() as u32).unwrap();
        let total = fs.into_device().writes;
        for cut_at in 0..=total {
            let mut fs = AriFs::open(CuttingDisk::new(&base, cut_at)).unwrap();
            let mut result = Ok(());
            for (k, chunk) in old.chunks(4096).enumerate() {
                result = result.and_then(|()| fs.replace_chunk(i, (k * 4096) as u32, chunk));
            }
            let result = result.and_then(|()| fs.replace_commit(i, old.len() as u32));
            assert_eq!(result.is_ok(), cut_at >= total, "cut at {cut_at}");
            let mut fs = AriFs::open(fs.into_device().power_off(true)).unwrap();
            let got = content(&mut fs, i);
            assert!(got == old || got == new, "cut at {cut_at}: mixed contents");
        }
    }

    /// Disk that counts requests: batches reduce the calls and the contents
    /// do not change compared to going sector by sector.
    struct CountingDisk {
        inner: MemDisk,
        calls: usize,
    }
    impl BlockDevice for CountingDisk {
        fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
            self.calls += 1;
            self.inner.read_sector(lba, buf)
        }
        fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
            self.calls += 1;
            self.inner.write_sector(lba, buf)
        }
        fn flush(&mut self) -> Result<(), FsError> {
            Ok(())
        }
        fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
            self.calls += 1;
            let o = lba as usize * SECTOR;
            buf.copy_from_slice(&self.inner.0[o..o + buf.len()]);
            Ok(())
        }
        fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
            self.calls += 1;
            let o = lba as usize * SECTOR;
            self.inner.0[o..o + buf.len()].copy_from_slice(buf);
            Ok(())
        }
    }

    #[test]
    fn batched_io_matches_sector_io_with_fewer_calls() {
        let data: Vec<u8> = (0..9000u32).map(|x| (x % 241) as u8).collect();
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("lotes").unwrap();
        // Misaligned at both ends: partial sectors at the start and at the end.
        fs.write(i, 0, &data[..7]).unwrap();
        fs.write(i, 7, &data[7..]).unwrap();
        let mut back = std::vec![0u8; 9000];
        assert_eq!(fs.read(i, 0, &mut back).unwrap(), 9000);
        assert_eq!(back, data);
        assert_eq!(fs.read(i, 513, &mut back[..1000]).unwrap(), 1000);
        assert_eq!(&back[..1000], &data[513..1513]);
        let mut counting = CountingDisk {
            inner: fs.into_device(),
            calls: 0,
        };
        counting.calls = 0;
        let mut fs = AriFs::open(counting).unwrap();
        let mut out = std::vec![0u8; 4096];
        let before = fs.dev.calls;
        assert_eq!(fs.read(i, 0, &mut out).unwrap(), 4096);
        assert_eq!(fs.dev.calls - before, 1, "aligned 4 KiB: a single request");
        let before = fs.dev.calls;
        fs.replace(i, &data[..4096]).unwrap();
        let used = fs.dev.calls - before;
        assert!(used <= 8, "4 KiB replace in {used} requests");
        assert_eq!(content_of_counting(&mut fs, i), &data[..4096]);
    }

    fn content_of_counting(fs: &mut AriFs<CountingDisk>, i: usize) -> Vec<u8> {
        let mut buf = std::vec![0u8; 40_000];
        let n = fs.read(i, 0, &mut buf).unwrap();
        buf.truncate(n);
        buf
    }

    /// Disk with a write cache that, on a cut, keeps only the pending
    /// sectors `keep` chooses: the `BlockDevice` contract does not promise
    /// ordering before `flush` (audit 6ee19fb, finding 2).
    struct SubsetDisk {
        persisted: Vec<u8>,
        pending: Vec<(u64, [u8; SECTOR])>,
    }

    impl SubsetDisk {
        fn new(base: &[u8]) -> Self {
            Self {
                persisted: base.to_vec(),
                pending: Vec::new(),
            }
        }
        fn power_off(mut self, keep: impl Fn(u64) -> bool) -> MemDisk {
            for (lba, s) in &self.pending {
                if keep(*lba) {
                    let o = *lba as usize * SECTOR;
                    self.persisted[o..o + SECTOR].copy_from_slice(s);
                }
            }
            MemDisk(self.persisted)
        }
    }

    impl BlockDevice for SubsetDisk {
        fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
            if let Some((_, s)) = self.pending.iter().rev().find(|(l, _)| *l == lba) {
                buf.copy_from_slice(s);
                return Ok(());
            }
            let o = lba as usize * SECTOR;
            buf.copy_from_slice(&self.persisted[o..o + SECTOR]);
            Ok(())
        }
        fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
            self.pending.push((lba, *buf));
            Ok(())
        }
        fn flush(&mut self) -> Result<(), FsError> {
            for (lba, s) in self.pending.drain(..) {
                let o = lba as usize * SECTOR;
                self.persisted[o..o + SECTOR].copy_from_slice(&s);
            }
            Ok(())
        }
    }

    fn is_table(lba: u64) -> bool {
        (TABLE_LBA..TABLE_LBA + TABLE_SECTORS).contains(&lba)
    }

    #[test]
    fn reordered_metadata_never_overlaps_files() {
        let mut fs = AriFs::format(disk(400), 400).unwrap();
        fs.create("a").unwrap();
        let base = fs.into_device().0;
        // Cut after `create` keeping only the table (the entry) and losing
        // everything else pending: the reservation was already in the
        // superblock thanks to the barrier, so nothing overlaps.
        for keep_table_only in [true, false] {
            let mut fs = AriFs::open(SubsetDisk::new(&base)).unwrap();
            let b = fs.create("b").unwrap();
            let first = fs.entry(b).unwrap().start_lba;
            let dev = fs.into_device().power_off(|lba| {
                if keep_table_only {
                    is_table(lba)
                } else {
                    lba == 0
                }
            });
            let mut fs = AriFs::open(dev).unwrap();
            let c = fs.create("c").unwrap();
            let second = fs.entry(c).unwrap().start_lba;
            assert_ne!(
                second, first,
                "space reallocated (table only: {keep_table_only})"
            );
            let mut used: Vec<(u64, u64)> = fs
                .entries()
                .map(|(_, e)| (e.start_lba, e.start_lba + e.capacity_sectors as u64))
                .collect();
            used.sort_unstable();
            for w in used.windows(2) {
                assert!(w[0].1 <= w[1].0, "overlapping files: {used:?}");
            }
        }
        // A size published without its data: the `write` barrier prevents it.
        let mut fs = AriFs::open(SubsetDisk::new(&base)).unwrap();
        let a = fs.find("a").unwrap();
        fs.write(a, 0, b"NEW").unwrap();
        let mut fs = AriFs::open(fs.into_device().power_off(is_table)).unwrap();
        let mut back = [0u8; 8];
        assert_eq!(fs.read(a, 0, &mut back).unwrap(), 3);
        assert_eq!(&back[..3], b"NEW", "size without data");
        // Metadata with a correct CRC but overlapping: not mounted.
        let mut fs = AriFs::format(disk(400), 400).unwrap();
        let x = fs.create("x").unwrap();
        fs.create("y").unwrap();
        let start = fs.entry(x).unwrap().start_lba;
        let mut img = fs.into_device().0;
        let mut overlapped = Entry {
            name: [0; NAME_LEN],
            name_len: 1,
            start_lba: start,
            capacity_sectors: FILE_CAPACITY_SECTORS,
            size: 0,
        };
        overlapped.name[0] = b'y';
        let o = TABLE_LBA as usize * SECTOR + ENTRY_LEN;
        img[o..o + ENTRY_LEN].copy_from_slice(&overlapped.encode());
        assert_eq!(AriFs::open(MemDisk(img)).err(), Some(FsError::Corrupt));
    }

    #[test]
    fn chunked_commit_only_covers_its_own_bytes() {
        let mut fs = AriFs::format(disk(400), 400).unwrap();
        let a = fs.create("app_a/notas").unwrap();
        let b = fs.create("app_b/notas").unwrap();
        fs.write(b, 0, b"propio").unwrap();
        fs.replace(a, b"XYNTHETIC_PRIVATE_DATA").unwrap();
        // B sends one byte and commits 22: it cannot take leftovers of A.
        fs.replace_chunk(b, 0, b"Y").unwrap();
        assert_eq!(fs.replace_commit(b, 22), Err(FsError::BadTransaction));
        assert_eq!(content(&mut fs, b), b"propio");
        // A chunk that does not start at zero, or with a gap, is rejected too.
        assert_eq!(fs.replace_chunk(b, 5, b"gap"), Err(FsError::BadTransaction));
        fs.replace_chunk(b, 0, b"abc").unwrap();
        assert_eq!(fs.replace_chunk(b, 4, b"d"), Err(FsError::BadTransaction));
        // Committing another file with this file's transaction: rejected.
        fs.replace_chunk(b, 0, b"abc").unwrap();
        assert_eq!(fs.replace_commit(a, 3), Err(FsError::BadTransaction));
        // The correct transaction keeps working and is consumed.
        fs.replace_chunk(b, 0, b"abc").unwrap();
        fs.replace_chunk(b, 3, b"de").unwrap();
        fs.replace_commit(b, 5).unwrap();
        assert_eq!(content(&mut fs, b), b"abcde");
        assert_eq!(fs.replace_commit(b, 5), Err(FsError::BadTransaction));
        // Abandoning a half-done transaction leaves the journal free for another.
        fs.replace_chunk(b, 0, b"a medias").unwrap();
        fs.abandon_replace();
        assert_eq!(fs.replace_commit(b, 8), Err(FsError::BadTransaction));
        fs.replace(a, b"otra").unwrap();
        assert_eq!(content(&mut fs, a), b"otra");
    }

    #[test]
    fn sized_files_hold_more_than_the_journal() {
        let mut fs = AriFs::format(disk(600), 600).unwrap();
        assert_eq!(fs.create_sized("x", 65), Err(FsError::OutOfRange));
        assert_eq!(fs.create_sized("x", 0), Err(FsError::OutOfRange));
        let i = fs
            .create_sized("apps/grande", 2 * FILE_CAPACITY_SECTORS)
            .unwrap();
        let data: Vec<u8> = (0..40_000u32).map(|x| (x % 253) as u8).collect();
        assert_eq!(fs.write(i, 0, &data).unwrap(), 40_000);
        assert_eq!(
            fs.replace(i, &data),
            Err(FsError::OutOfRange),
            "the journal does not cover more than 64 sectors"
        );
        let mut fs = AriFs::open(fs.into_device()).unwrap();
        assert_eq!(content(&mut fs, i), data);
        let j = fs.create("normal").unwrap();
        assert_eq!(
            fs.entry(j).unwrap().start_lba,
            DATA_LBA + 2 * FILE_CAPACITY_SECTORS as u64
        );
    }

    #[test]
    fn staged_replace_is_replayed_on_open() {
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("README").unwrap();
        fs.write(i, 0, b"antes").unwrap();
        fs.stage_replace(i, b"recuperado tras un corte").unwrap();
        assert_eq!(content_of(&mut fs, i), b"antes", "not applied yet");
        let mut fs = AriFs::open(fs.into_device()).unwrap();
        assert_eq!(fs.recovery(), Recovery::Replayed { index: i });
        assert_eq!(content_of(&mut fs, i), b"recuperado tras un corte");
    }

    fn content_of(fs: &mut AriFs<MemDisk>, i: usize) -> Vec<u8> {
        content(fs, i)
    }

    #[test]
    fn corruption_is_detected_and_torn_journal_discarded() {
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("a").unwrap();
        fs.write(i, 0, b"intacto").unwrap();
        let base = fs.into_device().0;
        // Superblock tampered with.
        let mut img = base.clone();
        img[9] ^= 0x01;
        assert_eq!(AriFs::open(MemDisk(img)).err(), Some(FsError::Corrupt));
        // In-use entry tampered with (one byte of the name).
        let mut img = base.clone();
        img[TABLE_LBA as usize * SECTOR] ^= 0x20;
        assert_eq!(AriFs::open(MemDisk(img)).err(), Some(FsError::Corrupt));
        // Free entry with garbage: still free (not in use).
        let mut img = base.clone();
        img[TABLE_LBA as usize * SECTOR + ENTRY_LEN + 3] = 0x55;
        assert!(AriFs::open(MemDisk(img)).is_ok());
        // Broken journal header (CRC): discarded, the file does not change.
        let mut img = base.clone();
        img[JOURNAL_LBA as usize * SECTOR + 8] = JOURNAL_PENDING as u8;
        let mut fs = AriFs::open(MemDisk(img)).unwrap();
        assert_eq!(fs.recovery(), Recovery::Discarded);
        assert_eq!(content(&mut fs, i), b"intacto");
        let fs = AriFs::open(fs.into_device()).unwrap();
        assert_eq!(fs.recovery(), Recovery::Clean, "the cleanup was written");
        // Valid pending header but data not matching its CRC.
        let journal = Journal {
            state: JOURNAL_PENDING,
            index: i as u32,
            len: 5,
            data_crc: 0xDEAD_BEEF,
            sequence: 9,
        };
        let mut img = base.clone();
        let o = JOURNAL_LBA as usize * SECTOR;
        img[o..o + SECTOR].copy_from_slice(&journal.encode());
        let mut fs = AriFs::open(MemDisk(img)).unwrap();
        assert_eq!(fs.recovery(), Recovery::Discarded);
        assert_eq!(content(&mut fs, i), b"intacto");
        // Valid pending header with correct data: redone.
        let journal = Journal {
            data_crc: crc32(b"nuevo"),
            ..journal
        };
        let mut img = base.clone();
        img[o..o + SECTOR].copy_from_slice(&journal.encode());
        let d = JOURNAL_DATA_LBA as usize * SECTOR;
        img[d..d + 5].copy_from_slice(b"nuevo");
        let mut fs = AriFs::open(MemDisk(img)).unwrap();
        assert_eq!(fs.recovery(), Recovery::Replayed { index: i });
        assert_eq!(content(&mut fs, i), b"nuevo");
        assert_eq!(fs.sequence, 9, "the sequence continues from the journal");
    }
}
