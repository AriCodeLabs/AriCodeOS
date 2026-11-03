// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriFS v2 (ADR 0072): up to 255 files of up to 16 MiB, each in up to
//! four extents, with copy-on-write replace, delete and rename.
//!
//! ```text
//! LBA 0                 superblock: magic, sectors, data LBA, the LBAs of
//!                       both table copies, CRC32 (written once)
//! table copy 0 and 1    32 sectors each: a header (magic, generation,
//!                       CRC32 of the whole copy, the geometry) and 255
//!                       entries of 64 bytes (name, size, extents)
//! data LBA..            extents; what no entry covers is free
//! ```
//!
//! A fresh volume has its copies at LBA 1 and 33 and data from 65; a
//! migrated one ([`AriFs::migrate`]) has them in v1's journal data area
//! (LBA 6 and 38) and data from 70, where v1's files already are.
//!
//! Persistence contract:
//!
//! - a 512-byte sector is written whole or not at all, and nothing is
//!   assumed about ordering between sectors except after `flush`;
//! - every change of metadata is a commit: the whole table, one
//!   generation newer, goes to the copy not holding the current one, then
//!   `flush`. `open` takes the valid copy with the higher generation, so a
//!   torn commit leaves the state before it;
//! - since every commit is flushed, the copy a commit overwrites is never
//!   the only durable one, and sectors freed by a commit are no longer
//!   referenced by any copy `open` could pick;
//! - `replace` writes the new contents to free sectors and commits them,
//!   flushed before the commit: after a cut the file holds the whole
//!   previous contents or the whole new ones;
//! - `write` writes in place inside the file's extents; a write that grows
//!   the file puts the data in new free sectors and commits the size after
//!   a flush;
//! - free space is never stored: it is what the committed table does not
//!   cover, so there is no map to disagree with the table;
//! - `open` rejects (`Corrupt`) metadata that does not hold together
//!   (overlapping extents, extents outside the data area, duplicate names)
//!   even under a valid CRC: a volume that cannot be trusted is not
//!   mounted. After a failed commit, the next change first writes the
//!   table in memory over the copy that commit may have left.

use alloc::boxed::Box;
use alloc::vec;

use crate::{
    crc32_update, read_area, u32_at, u64_at, v1, valid_name, write_area, BlockDevice, FsError,
    BATCH_BYTES, MAX_BATCH_SECTORS, NAME_LEN, SECTOR,
};

const MAGIC: &[u8; 8] = b"ARIFS\x00\x00\x03";
const TABLE_MAGIC: &[u8; 8] = b"ARITABLE";
pub const ENTRY_LEN: usize = 64;
/// Sectors of one table copy.
pub const TABLE_SECTORS: u64 = 32;
const TABLE_BYTES: usize = TABLE_SECTORS as usize * SECTOR;
const ENTRIES_PER_SECTOR: usize = SECTOR / ENTRY_LEN;
/// Files: the slots of a copy but the header.
pub const MAX_FILES: usize = TABLE_BYTES / ENTRY_LEN - 1;
/// Extents of one file.
pub const MAX_EXTENTS: usize = 4;
/// Largest file, and so the largest atomic replace.
pub const MAX_FILE_BYTES: u32 = 16 << 20;
const MAX_FILE_SECTORS: u32 = MAX_FILE_BYTES / SECTOR as u32;
/// A file that grows takes at least this much more at a time, so a file
/// written in small pieces stays in few extents.
pub const GROW_SECTORS: u32 = 64;
/// An extent's length is stored in 16 bits.
const MAX_EXTENT_SECTORS: u64 = u16::MAX as u64;

/// Where a fresh volume puts its table copies and data.
const FRESH_TABLES: [u64; 2] = [1, 1 + TABLE_SECTORS];
const FRESH_DATA: u64 = 1 + 2 * TABLE_SECTORS;
/// Where a migration puts them: v1's journal data area holds exactly the
/// two copies, and v1's files start right after it.
const MIGRATED_TABLES: [u64; 2] = [v1::JOURNAL_DATA_LBA, v1::JOURNAL_DATA_LBA + TABLE_SECTORS];
const MIGRATED_DATA: u64 = v1::DATA_LBA;
const _: () = assert!(MIGRATED_TABLES[1] + TABLE_SECTORS == MIGRATED_DATA);

/// Where the superblock and the tables are, and the data area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub total_sectors: u64,
    pub data_lba: u64,
    pub tables: [u64; 2],
}

impl Geometry {
    fn fresh(total_sectors: u64) -> Self {
        Self {
            total_sectors,
            data_lba: FRESH_DATA,
            tables: FRESH_TABLES,
        }
    }

    fn migrated(total_sectors: u64) -> Self {
        Self {
            total_sectors,
            data_lba: MIGRATED_DATA,
            tables: MIGRATED_TABLES,
        }
    }

    /// The copies lie in order between the superblock and the data, and
    /// every LBA fits an extent's 32-bit start.
    fn valid(&self) -> bool {
        // Checked: under a valid CRC a forged geometry must not wrap
        // (audit of ADR 0072, I2).
        let end = |lba: u64| lba.checked_add(TABLE_SECTORS);
        self.tables[0] >= 1
            && end(self.tables[0]).is_some_and(|e| e <= self.tables[1])
            && end(self.tables[1]).is_some_and(|e| e <= self.data_lba)
            && self.data_lba < self.total_sectors
            && self.total_sectors <= u32::MAX as u64
    }

    fn encode_into(&self, b: &mut [u8]) {
        b[0..8].copy_from_slice(&self.total_sectors.to_le_bytes());
        b[8..16].copy_from_slice(&self.data_lba.to_le_bytes());
        b[16..24].copy_from_slice(&self.tables[0].to_le_bytes());
        b[24..32].copy_from_slice(&self.tables[1].to_le_bytes());
    }

    fn decode(b: &[u8]) -> Self {
        Self {
            total_sectors: u64_at(b, 0),
            data_lba: u64_at(b, 8),
            tables: [u64_at(b, 16), u64_at(b, 24)],
        }
    }

    fn superblock(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(MAGIC);
        self.encode_into(&mut s[8..40]);
        let crc = crate::crc32(&s[..SECTOR - 4]);
        s[SECTOR - 4..].copy_from_slice(&crc.to_le_bytes());
        s
    }

    /// `BadMagic` if it is not a v2 superblock, `Corrupt` if it is one with
    /// a wrong CRC or a geometry that does not hold.
    fn from_superblock(s: &[u8; SECTOR]) -> Result<Self, FsError> {
        if &s[..8] != MAGIC {
            return Err(FsError::BadMagic);
        }
        let g = Self::decode(&s[8..40]);
        if u32_at(s, SECTOR - 4) != crate::crc32(&s[..SECTOR - 4]) || !g.valid() {
            return Err(FsError::Corrupt);
        }
        Ok(g)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extent {
    /// LBA of the first sector (a volume has at most 2^32 sectors).
    pub start: u32,
    pub len: u32,
}

impl Extent {
    fn end(&self) -> u64 {
        self.start as u64 + self.len as u64
    }
    fn overlaps(&self, start: u64, len: u64) -> bool {
        (self.start as u64) < start + len && start < self.end()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub name: [u8; NAME_LEN],
    pub name_len: u8,
    pub size: u32,
    extents: [Extent; MAX_EXTENTS],
    count: u8,
}

impl Entry {
    fn new(name: &str) -> Self {
        let mut e = Self {
            name: [0; NAME_LEN],
            name_len: name.len() as u8,
            size: 0,
            extents: [Extent::default(); MAX_EXTENTS],
            count: 0,
        };
        e.name[..name.len()].copy_from_slice(name.as_bytes());
        e
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }

    pub fn extents(&self) -> &[Extent] {
        &self.extents[..self.count as usize]
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.extents().iter().map(|e| e.len as u64).sum()
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_sectors() * SECTOR as u64
    }

    /// Name, flags, size and the extents as (start u32, length u16).
    fn encode_into(&self, b: &mut [u8]) {
        b[..NAME_LEN].copy_from_slice(&self.name);
        b[32] = self.name_len;
        b[33] = 1; // in use
        b[34] = self.count;
        b[36..40].copy_from_slice(&self.size.to_le_bytes());
        for (i, x) in self.extents().iter().enumerate() {
            let o = 40 + i * 6;
            b[o..o + 4].copy_from_slice(&x.start.to_le_bytes());
            b[o + 4..o + 6].copy_from_slice(&(x.len as u16).to_le_bytes());
        }
    }

    /// `None` for a free slot; `Corrupt` for one whose fields cannot be an
    /// entry (the layout check sees the rest).
    fn decode(b: &[u8]) -> Result<Option<Self>, FsError> {
        if b[33] == 0 {
            return Ok(None);
        }
        let count = b[34];
        let name_len = b[32];
        if b[33] != 1 || count as usize > MAX_EXTENTS || name_len as usize > NAME_LEN {
            return Err(FsError::Corrupt);
        }
        let mut name = [0u8; NAME_LEN];
        name.copy_from_slice(&b[..NAME_LEN]);
        let mut extents = [Extent::default(); MAX_EXTENTS];
        for (i, x) in extents.iter_mut().take(count as usize).enumerate() {
            let o = 40 + i * 6;
            *x = Extent {
                start: u32_at(b, o),
                len: u16::from_le_bytes([b[o + 4], b[o + 5]]) as u32,
            };
        }
        Ok(Some(Self {
            name,
            name_len,
            size: u32_at(b, 36),
            extents,
            count,
        }))
    }

    /// Adds `sectors` at the end of the file in `fs`'s free space (counting
    /// `self`'s own extents as used): the last extent grows in place if the
    /// space after it is free, otherwise the largest free run starts a new
    /// extent.
    fn grow<D: BlockDevice>(&mut self, fs: &AriFs<D>, mut sectors: u64) -> Result<(), FsError> {
        while sectors > 0 {
            if self.count > 0 {
                let last = self.extents[self.count as usize - 1];
                let room = fs
                    .free_run_at(last.end(), self.extents())
                    .min(MAX_EXTENT_SECTORS - last.len as u64);
                if room > 0 {
                    let n = room.min(sectors);
                    self.extents[self.count as usize - 1].len += n as u32;
                    sectors -= n;
                    continue;
                }
            }
            if self.count as usize == MAX_EXTENTS {
                return Err(FsError::NoSpace);
            }
            let (start, run) = fs.largest_free(self.extents()).ok_or(FsError::NoSpace)?;
            let n = run.min(sectors).min(MAX_EXTENT_SECTORS);
            self.extents[self.count as usize] = Extent {
                start: start as u32,
                len: n as u32,
            };
            self.count += 1;
            sectors -= n;
        }
        Ok(())
    }

    /// Keeps only the first `sectors` sectors of the extents.
    fn trim(&mut self, sectors: u64) {
        let mut left = sectors;
        let mut kept = 0;
        for x in self.extents.iter_mut().take(self.count as usize) {
            if left == 0 {
                break;
            }
            x.len = (x.len as u64).min(left) as u32;
            left -= x.len as u64;
            kept += 1;
        }
        self.count = kept;
        for x in self.extents.iter_mut().skip(kept as usize) {
            *x = Extent::default();
        }
    }
}

/// What `open` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Both table copies valid.
    Clean,
    /// One copy did not verify, a commit cut half-written: the other one,
    /// the state before that commit, was taken.
    TornCommitDiscarded,
    /// The superblock of a migration did not verify; the migration's
    /// tables and v1's marker did, so it was written again.
    MigrationCompleted,
}

/// A replace in progress: the file, the next offset expected, and the
/// sectors taken so far (not in the table until the commit).
#[derive(Debug, Clone, Copy)]
struct Staged {
    index: usize,
    next: u32,
    space: Entry,
}

/// A file system opened on a device.
pub struct AriFs<D: BlockDevice> {
    dev: D,
    geo: Geometry,
    /// On the heap: 255 entries are too large for a frame of fsd's stack.
    table: Table,
    generation: u64,
    /// The copy holding `generation`; the next commit writes the other.
    current: usize,
    recovery: Recovery,
    staged: Option<Staged>,
    /// A commit failed: the copy it wrote may hold that newer generation
    /// (written, not known to be flushed) or nothing valid. The next change
    /// first writes the table in memory over it and flushes, so no sector is
    /// written, and none reused, while a copy on disk may say otherwise.
    resync: bool,
}

/// The entries, `MAX_FILES` long.
type Table = Box<[Option<Entry>]>;

/// An empty table, built on the heap directly.
fn empty_table() -> Table {
    vec![None; MAX_FILES].into_boxed_slice()
}

/// One change to the table in a commit: slot and new value.
type Change = (usize, Option<Entry>);

impl<D: BlockDevice> AriFs<D> {
    /// Creates an empty file system on `total_sectors` sectors.
    pub fn format(dev: D, total_sectors: u64) -> Result<Self, FsError> {
        let geo = Geometry::fresh(total_sectors);
        if !geo.valid() || total_sectors < FRESH_DATA + GROW_SECTORS as u64 {
            return Err(FsError::NoSpace);
        }
        let mut fs = Self::new(dev, geo, empty_table());
        fs.write_copy(0, 1, &[])?;
        fs.write_copy(1, 1, &[])?;
        fs.dev.flush()?;
        fs.dev.write_sector(0, &geo.superblock())?;
        fs.dev.flush()?;
        fs.generation = 1;
        Ok(fs)
    }

    fn new(dev: D, geo: Geometry, table: Table) -> Self {
        Self {
            dev,
            geo,
            table,
            generation: 0,
            current: 0,
            recovery: Recovery::Clean,
            staged: None,
            resync: false,
        }
    }

    /// Whether `dev` holds a v2 volume, or a migration to v2 whose
    /// superblock write was cut (which `open` completes).
    pub fn is_v2(dev: &mut D) -> Result<bool, FsError> {
        let mut s = [0u8; SECTOR];
        dev.read_sector(0, &mut s)?;
        if Geometry::from_superblock(&s).is_ok() {
            return Ok(true);
        }
        if v1::Superblock::is_valid(&s) {
            return Ok(false);
        }
        Ok(Self::interrupted_migration(dev)?.is_some())
    }

    /// Opens a v2 volume. `BadMagic` for a v1 volume (or none at all).
    pub fn open(mut dev: D) -> Result<Self, FsError> {
        let mut s = [0u8; SECTOR];
        dev.read_sector(0, &mut s)?;
        let (geo, recovery) = match Geometry::from_superblock(&s) {
            Ok(g) => (g, None),
            Err(e) => {
                if v1::Superblock::is_valid(&s) {
                    return Err(FsError::BadMagic);
                }
                match Self::interrupted_migration(&mut dev)? {
                    Some(g) => (g, Some(Recovery::MigrationCompleted)),
                    None => return Err(e),
                }
            }
        };
        let mut fs = Self::new(dev, geo, empty_table());
        let generations = [fs.verify_copy(0)?, fs.verify_copy(1)?];
        let (current, generation) = match generations {
            [Some(a), Some(b)] => (if b > a { 1 } else { 0 }, a.max(b)),
            [Some(a), None] => (0, a),
            [None, Some(b)] => (1, b),
            [None, None] => return Err(FsError::Corrupt),
        };
        fs.current = current;
        fs.generation = generation;
        fs.recovery = recovery.unwrap_or(if generations.contains(&None) {
            Recovery::TornCommitDiscarded
        } else {
            Recovery::Clean
        });
        fs.load_copy(current)?;
        check_layout(&fs.geo, &fs.table)?;
        if recovery.is_some() {
            fs.dev.write_sector(0, &geo.superblock())?;
            fs.dev.flush()?;
        }
        Ok(fs)
    }

    /// The geometry of a migration whose superblock write was cut: v1's
    /// journal header is the migration marker and a table copy at the
    /// migration's place verifies with the migration's geometry.
    fn interrupted_migration(dev: &mut D) -> Result<Option<Geometry>, FsError> {
        let mut s = [0u8; SECTOR];
        dev.read_sector(v1::JOURNAL_LBA, &mut s)?;
        if !v1::is_migration_marker(&s) {
            return Ok(None);
        }
        for copy in MIGRATED_TABLES {
            dev.read_sector(copy, &mut s)?;
            if &s[..8] != TABLE_MAGIC {
                continue;
            }
            let geo = Geometry::decode(&s[20..52]);
            if geo.tables == MIGRATED_TABLES && geo.data_lba == MIGRATED_DATA && geo.valid() {
                return Ok(Some(geo));
            }
        }
        Ok(None)
    }

    /// Migrates a v1 volume in place, without moving data: v1's files are
    /// contiguous runs, valid extents. A pending v1 replace is finished
    /// first (v1's `open`). Both copies go to v1's journal data area with
    /// the marker in v1's journal header, flushed; the v2 superblock is the
    /// commit. A cut before it leaves v1, which clears the marker.
    pub fn migrate(dev: D) -> Result<Self, FsError> {
        let old = v1::AriFs::open(dev)?;
        let geo = Geometry::migrated(old.superblock().total_sectors);
        if !geo.valid() {
            return Err(FsError::OutOfRange);
        }
        let mut table = empty_table();
        for (i, e) in old.entries() {
            let mut entry = Entry::new(e.name_str());
            entry.size = e.size;
            let (mut start, mut left) = (e.start_lba, e.capacity_sectors as u64);
            while left > 0 {
                if entry.count as usize == MAX_EXTENTS {
                    return Err(FsError::OutOfRange);
                }
                let len = left.min(MAX_EXTENT_SECTORS);
                entry.extents[entry.count as usize] = Extent {
                    start: start as u32,
                    len: len as u32,
                };
                entry.count += 1;
                start += len;
                left -= len;
            }
            table[i] = Some(entry);
        }
        check_layout(&geo, &table)?;
        let mut fs = Self::new(old.into_device(), geo, table);
        fs.write_copy(0, 1, &[])?;
        fs.write_copy(1, 1, &[])?;
        fs.dev
            .write_sector(v1::JOURNAL_LBA, &v1::migration_marker())?;
        fs.dev.flush()?;
        fs.dev.write_sector(0, &geo.superblock())?;
        fs.dev.flush()?;
        fs.generation = 1;
        Ok(fs)
    }

    /// Generation of `copy` if its magic, CRC and geometry verify.
    fn verify_copy(&mut self, copy: usize) -> Result<Option<u64>, FsError> {
        let base = self.geo.tables[copy];
        let mut batch = [0u8; BATCH_BYTES];
        let mut crc = 0xFFFF_FFFFu32;
        let mut header = [0u8; ENTRY_LEN];
        for first in (0..TABLE_SECTORS).step_by(MAX_BATCH_SECTORS) {
            self.dev.read_sectors(base + first, &mut batch)?;
            if first == 0 {
                header.copy_from_slice(&batch[..ENTRY_LEN]);
                batch[16..20].fill(0);
            }
            crc = crc32_update(crc, &batch);
        }
        let ok = &header[..8] == TABLE_MAGIC
            && u32_at(&header, 16) == !crc
            && Geometry::decode(&header[20..52]) == self.geo;
        Ok(ok.then(|| u64_at(&header, 8)))
    }

    fn load_copy(&mut self, copy: usize) -> Result<(), FsError> {
        let base = self.geo.tables[copy];
        let mut batch = [0u8; BATCH_BYTES];
        for first in (0..TABLE_SECTORS).step_by(MAX_BATCH_SECTORS) {
            self.dev.read_sectors(base + first, &mut batch)?;
            for (k, slot) in batch.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
                let n = first as usize * ENTRIES_PER_SECTOR + k;
                if n > 0 {
                    self.table[n - 1] = Entry::decode(slot)?;
                }
            }
        }
        Ok(())
    }

    /// Sector `k` of a copy of the table with `changes` applied, holding
    /// `generation` and `crc` in its header (in sector 0).
    fn copy_sector(&self, k: usize, generation: u64, crc: u32, changes: &[Change]) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        for (j, slot) in s.as_chunks_mut::<ENTRY_LEN>().0.iter_mut().enumerate() {
            let n = k * ENTRIES_PER_SECTOR + j;
            if n == 0 {
                slot[..8].copy_from_slice(TABLE_MAGIC);
                slot[8..16].copy_from_slice(&generation.to_le_bytes());
                slot[16..20].copy_from_slice(&crc.to_le_bytes());
                self.geo.encode_into(&mut slot[20..52]);
                continue;
            }
            let i = n - 1;
            let entry = match changes.iter().find(|(c, _)| *c == i) {
                Some((_, e)) => *e,
                None => self.table[i],
            };
            if let Some(e) = entry {
                e.encode_into(slot);
            }
        }
        s
    }

    /// Writes the table with `changes` as `generation` into `copy`: the CRC
    /// first, from the same bytes, then the body, a flush, and the header
    /// (not flushed: the caller does).
    fn write_copy(
        &mut self,
        copy: usize,
        generation: u64,
        changes: &[Change],
    ) -> Result<(), FsError> {
        let mut crc = 0xFFFF_FFFFu32;
        for k in 0..TABLE_SECTORS as usize {
            crc = crc32_update(crc, &self.copy_sector(k, generation, 0, changes));
        }
        let crc = !crc;
        let base = self.geo.tables[copy];
        // The body first, flushed; then the header, one sector, is the
        // commit point. Until it lands the copy keeps its older header,
        // whose generation `open` does not prefer, so no mix of old and new
        // body sectors can pass for a copy, whatever names an application
        // chose to steer the CRC (audit of ADR 0072, L1).
        let mut batch = [0u8; BATCH_BYTES];
        let mut first = 1;
        while first < TABLE_SECTORS as usize {
            let n = (TABLE_SECTORS as usize - first).min(MAX_BATCH_SECTORS);
            for (j, s) in batch.as_chunks_mut::<SECTOR>().0[..n]
                .iter_mut()
                .enumerate()
            {
                *s = self.copy_sector(first + j, generation, crc, changes);
            }
            self.dev
                .write_sectors(base + first as u64, &batch[..n * SECTOR])?;
            first += n;
        }
        self.dev.flush()?;
        self.dev
            .write_sector(base, &self.copy_sector(0, generation, crc, changes))
    }

    /// A commit: the table with `changes`, one generation newer, into the
    /// other copy, flushed; memory follows only then. On a failure memory
    /// stays as it was and the next change resyncs first ([`Self::ready`]).
    fn commit(&mut self, changes: &[Change]) -> Result<(), FsError> {
        let target = 1 - self.current;
        let generation = self.generation + 1;
        let done = self
            .write_copy(target, generation, changes)
            .and_then(|()| self.dev.flush());
        if done.is_err() {
            self.resync = true;
            return done;
        }
        for &(i, e) in changes {
            self.table[i] = e;
        }
        self.current = target;
        self.generation = generation;
        self.resync = false;
        Ok(())
    }

    /// Before a change: after a failed commit, the table in memory goes over
    /// the copy that commit may have left, flushed. Until that succeeds no
    /// change starts (a transient error of the medium ends once it does).
    fn ready(&mut self) -> Result<(), FsError> {
        if self.resync {
            self.commit(&[])?;
        }
        Ok(())
    }

    pub fn geometry(&self) -> Geometry {
        self.geo
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

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

    /// Sectors neither an entry nor the replace in progress covers.
    pub fn free_sectors(&self) -> u64 {
        let used: u64 = self.used().map(|x| x.len as u64).sum();
        self.geo.total_sectors - self.geo.data_lba - used
    }

    /// Every extent in use: the table's and the staged replace's.
    fn used(&self) -> impl Iterator<Item = &Extent> {
        self.entries()
            .flat_map(|(_, e)| e.extents())
            .chain(self.staged.iter().flat_map(|s| s.space.extents()))
    }

    /// Free sectors from `pos` on before the next used one (0 if `pos` is
    /// used or outside the data area), counting `extra` as used.
    fn free_run_at(&self, pos: u64, extra: &[Extent]) -> u64 {
        if pos < self.geo.data_lba || pos >= self.geo.total_sectors {
            return 0;
        }
        let mut end = self.geo.total_sectors;
        for x in self.used().chain(extra) {
            if x.overlaps(pos, 1) {
                return 0;
            }
            if x.start as u64 > pos {
                end = end.min(x.start as u64);
            }
        }
        end - pos
    }

    /// The longest free run, counting `extra` as used: it starts at the
    /// data area or right after an extent.
    fn largest_free(&self, extra: &[Extent]) -> Option<(u64, u64)> {
        let starts =
            core::iter::once(self.geo.data_lba).chain(self.used().chain(extra).map(Extent::end));
        let mut best: Option<(u64, u64)> = None;
        for start in starts {
            let run = self.free_run_at(start, extra);
            if run > 0 && best.is_none_or(|(_, b)| run > b) {
                best = Some((start, run));
            }
        }
        best
    }

    /// Creates an empty file; it takes space as it is written. Returns its
    /// index.
    pub fn create(&mut self, name: &str) -> Result<usize, FsError> {
        self.create_sized(name, 0)
    }

    /// Like `create`, with `sectors` already taken (xtask fills disks with
    /// it).
    pub fn create_sized(&mut self, name: &str, sectors: u32) -> Result<usize, FsError> {
        self.ready()?;
        if !valid_name(name) {
            return Err(FsError::BadName);
        }
        if sectors > MAX_FILE_SECTORS {
            return Err(FsError::OutOfRange);
        }
        if self.find(name).is_some() {
            return Err(FsError::Exists);
        }
        let index = self
            .table
            .iter()
            .position(Option::is_none)
            .ok_or(FsError::NoSpace)?;
        let mut entry = Entry::new(name);
        entry.grow(self, sectors as u64)?;
        self.commit(&[(index, Some(entry))])?;
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
        read_extents(&mut self.dev, e.extents(), offset as usize, &mut buf[..len])?;
        Ok(len)
    }

    /// Writes `data` at `offset` (no gaps: `offset <= size`). Inside the
    /// file's extents it writes in place, durable at `flush`; past them
    /// the file grows into free sectors, and the new size is committed
    /// after the data is flushed.
    pub fn write(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        self.ready()?;
        let mut e = *self.entry(index).ok_or(FsError::NotFound)?;
        if offset > e.size {
            return Err(FsError::OutOfRange);
        }
        let end = offset as u64 + data.len() as u64;
        if end > MAX_FILE_BYTES as u64 {
            return Err(FsError::OutOfRange);
        }
        let needed = end.div_ceil(SECTOR as u64);
        if needed > e.capacity_sectors() {
            let more = (needed - e.capacity_sectors())
                .max(GROW_SECTORS as u64)
                .min(MAX_FILE_SECTORS as u64 - e.capacity_sectors());
            e.grow(self, more)?;
        }
        write_extents(&mut self.dev, e.extents(), offset as usize, data)?;
        if end > e.size as u64 {
            // The data must be durable before the size that covers it.
            self.dev.flush()?;
            e.size = end as u32;
            self.commit(&[(index, Some(e))])?;
        }
        Ok(data.len())
    }

    /// Atomically replaces the whole contents with `data`. Durable on
    /// return.
    pub fn replace(&mut self, index: usize, data: &[u8]) -> Result<(), FsError> {
        self.replace_chunk(index, 0, data)?;
        self.replace_commit(index, data.len() as u32)
    }

    /// One chunk of the new contents into free sectors, without committing:
    /// nothing changes until `replace_commit` (ADR 0018). A replace starts
    /// at offset 0 and continues without gaps.
    pub fn replace_chunk(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<(), FsError> {
        self.ready()?;
        self.entry(index).ok_or(FsError::NotFound)?;
        if offset == 0 {
            self.staged = Some(Staged {
                index,
                next: 0,
                space: Entry::new(""),
            });
        }
        let mut staged = match self.staged {
            Some(s) if s.index == index && s.next == offset => s,
            _ => return Err(FsError::BadTransaction),
        };
        let end = offset as u64 + data.len() as u64;
        if end > MAX_FILE_BYTES as u64 {
            return Err(FsError::OutOfRange);
        }
        let needed = end.div_ceil(SECTOR as u64);
        let have = staged.space.capacity_sectors();
        if needed > have {
            // Taken ahead, so the next chunks continue in the same run;
            // the commit gives back what the contents do not use.
            let more = (needed - have)
                .max(GROW_SECTORS as u64)
                .min(MAX_FILE_SECTORS as u64 - have);
            let mut space = staged.space;
            self.staged = None;
            // Out of space: the replace is abandoned and what it took is
            // free again (audit of ADR 0072, L2).
            space.grow(self, more)?;
            staged.space = space;
            self.staged = Some(staged);
        }
        if let Err(e) = write_extents(&mut self.dev, staged.space.extents(), offset as usize, data)
        {
            self.staged = None;
            return Err(e);
        }
        staged.next = end as u32;
        self.staged = Some(staged);
        Ok(())
    }

    /// Discards the replace in progress, if any: its sectors are free again.
    pub fn abandon_replace(&mut self) {
        self.staged = None;
    }

    /// Commits the first `len` bytes written by the replace in progress as
    /// the file's contents: they are flushed, then the table points at them
    /// and the old sectors are free.
    pub fn replace_commit(&mut self, index: usize, len: u32) -> Result<(), FsError> {
        self.ready()?;
        let mut staged = match self.staged.take() {
            Some(s) if s.index == index && s.next == len => s,
            other => {
                self.staged = other;
                return Err(FsError::BadTransaction);
            }
        };
        let mut e = *self.entry(index).ok_or(FsError::NotFound)?;
        staged.space.trim((len as u64).div_ceil(SECTOR as u64));
        e.extents = staged.space.extents;
        e.count = staged.space.count;
        e.size = len;
        self.dev.flush()?;
        self.commit(&[(index, Some(e))])
    }

    /// Truncates to zero bytes; the file keeps its sectors.
    pub fn truncate(&mut self, index: usize) -> Result<(), FsError> {
        self.ready()?;
        let mut e = *self.entry(index).ok_or(FsError::NotFound)?;
        e.size = 0;
        self.commit(&[(index, Some(e))])
    }

    /// Deletes the file; its sectors are free once committed.
    pub fn delete(&mut self, index: usize) -> Result<(), FsError> {
        self.ready()?;
        self.entry(index).ok_or(FsError::NotFound)?;
        self.commit(&[(index, None)])?;
        if self.staged.is_some_and(|s| s.index == index) {
            self.staged = None;
        }
        Ok(())
    }

    /// Renames the file. If `name` exists, that file is deleted in the same
    /// commit: after a cut either both are as before or `name` holds this
    /// file (the atomic rename of temp + rename installers).
    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), FsError> {
        self.ready()?;
        if !valid_name(name) {
            return Err(FsError::BadName);
        }
        let mut e = *self.entry(index).ok_or(FsError::NotFound)?;
        let other = self.find(name).filter(|&j| j != index);
        e = Entry {
            name: Entry::new(name).name,
            name_len: name.len() as u8,
            ..e
        };
        match other {
            Some(j) => {
                self.commit(&[(index, Some(e)), (j, None)])?;
                if self.staged.is_some_and(|s| s.index == j) {
                    self.staged = None;
                }
            }
            None => self.commit(&[(index, Some(e))])?,
        }
        Ok(())
    }

    /// Makes previous in-place writes durable.
    pub fn flush(&mut self) -> Result<(), FsError> {
        self.dev.flush()
    }

    pub fn into_device(self) -> D {
        self.dev
    }
}

/// The table must hold together, not just carry a CRC: names valid and
/// unique, extents inside the data area and disjoint, sizes inside their
/// extents and the file limit.
fn check_layout(geo: &Geometry, table: &[Option<Entry>]) -> Result<(), FsError> {
    let used = || table.iter().flatten();
    for (i, e) in table.iter().enumerate() {
        let Some(e) = e else { continue };
        let name =
            core::str::from_utf8(&e.name[..e.name_len as usize]).map_err(|_| FsError::Corrupt)?;
        if !valid_name(name)
            || e.capacity_sectors() > MAX_FILE_SECTORS as u64
            || e.size as u64 > e.capacity_bytes()
            || table[..i].iter().flatten().any(|o| o.name_str() == name)
        {
            return Err(FsError::Corrupt);
        }
        for x in e.extents() {
            if x.len == 0 || (x.start as u64) < geo.data_lba || x.end() > geo.total_sectors {
                return Err(FsError::Corrupt);
            }
        }
    }
    // Every pair of extents, of the same file or of two, is disjoint.
    let all = || used().flat_map(Entry::extents);
    for (a, x) in all().enumerate() {
        if all()
            .skip(a + 1)
            .any(|y| y.overlaps(x.start as u64, x.len as u64))
        {
            return Err(FsError::Corrupt);
        }
    }
    Ok(())
}

/// Reads `out.len()` bytes from byte `pos` of the file made of `extents`.
fn read_extents<D: BlockDevice>(
    dev: &mut D,
    extents: &[Extent],
    pos: usize,
    out: &mut [u8],
) -> Result<(), FsError> {
    let mut done = 0;
    let mut base = 0;
    for x in extents {
        let len = x.len as usize * SECTOR;
        let at = pos + done;
        if done < out.len() && at < base + len {
            let n = (base + len - at).min(out.len() - done);
            read_area(dev, x.start as u64, at - base, &mut out[done..done + n])?;
            done += n;
        }
        base += len;
    }
    if done < out.len() {
        return Err(FsError::OutOfRange);
    }
    Ok(())
}

/// Writes `data` at byte `pos` of the file made of `extents`.
fn write_extents<D: BlockDevice>(
    dev: &mut D,
    extents: &[Extent],
    pos: usize,
    data: &[u8],
) -> Result<(), FsError> {
    let mut done = 0;
    let mut base = 0;
    for x in extents {
        let len = x.len as usize * SECTOR;
        let at = pos + done;
        if done < data.len() && at < base + len {
            let n = (base + len - at).min(data.len() - done);
            write_area(dev, x.start as u64, at - base, &data[done..done + n])?;
            done += n;
        }
        base += len;
    }
    if done < data.len() {
        return Err(FsError::OutOfRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
