// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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

/// Which of the writes not yet flushed reach the medium at a power cut:
/// the medium may keep any of them, in any order.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kept {
    None,
    All,
    Even,
    Odd,
}

const KEPT: [Kept; 4] = [Kept::None, Kept::All, Kept::Even, Kept::Odd];

/// A disk whose writes fail from the `cut_at`-th on, as at a power cut,
/// and whose unflushed writes are kept by `Kept` when it is powered off.
struct CuttingDisk {
    persisted: Vec<u8>,
    cache: Vec<(u64, [u8; SECTOR])>,
    writes: usize,
    flushes: usize,
    cut_at: usize,
    cut: bool,
}

impl CuttingDisk {
    fn new(base: &[u8], cut_at: usize) -> Self {
        Self {
            persisted: base.to_vec(),
            cache: Vec::new(),
            writes: 0,
            flushes: 0,
            cut_at,
            cut: false,
        }
    }

    fn power_off(mut self, kept: Kept) -> MemDisk {
        for (n, (lba, s)) in self.cache.iter().enumerate() {
            let keep = match kept {
                Kept::None => false,
                Kept::All => true,
                Kept::Even => n % 2 == 0,
                Kept::Odd => n % 2 == 1,
            };
            if keep {
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
        self.flushes += 1;
        for (lba, s) in self.cache.drain(..) {
            let o = lba as usize * SECTOR;
            self.persisted[o..o + SECTOR].copy_from_slice(&s);
        }
        Ok(())
    }
}

fn content<D: BlockDevice>(fs: &mut AriFs<D>, i: usize) -> Vec<u8> {
    let size = fs.entry(i).unwrap().size as usize;
    let mut buf = std::vec![0u8; size];
    assert_eq!(fs.read(i, 0, &mut buf).unwrap(), size);
    buf
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    (0..len as u32)
        .map(|x| (x.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 24) as u8)
        .collect()
}

/// Every extent of every file is disjoint from the others and inside the
/// data area (what `open` checks, asserted from outside).
fn assert_disjoint<D: BlockDevice>(fs: &AriFs<D>) {
    let mut all: Vec<(u64, u64)> = fs
        .entries()
        .flat_map(|(_, e)| e.extents().iter().map(|x| (x.start as u64, x.end())))
        .collect();
    all.sort_unstable();
    for w in all.windows(2) {
        assert!(w[0].1 <= w[1].0, "overlapping extents: {all:?}");
    }
    if let Some(first) = all.first() {
        assert!(first.0 >= fs.geometry().data_lba);
    }
}

#[test]
fn format_create_write_read_and_reopen() {
    let mut fs = AriFs::format(disk(2000), 2000).unwrap();
    let a = fs.create("notes").unwrap();
    let b = fs.create("dir/b.txt").unwrap();
    assert_eq!(
        fs.entry(a).unwrap().capacity_sectors(),
        0,
        "no space until written"
    );
    fs.write(a, 0, b"hello").unwrap();
    fs.write(a, 5, b" world").unwrap();
    fs.write(b, 0, &pattern(70_000, 1)).unwrap();
    let mut fs = AriFs::open(fs.into_device()).unwrap();
    assert_eq!(fs.recovery(), Recovery::Clean);
    assert_eq!(content(&mut fs, a), b"hello world");
    assert_eq!(content(&mut fs, b), pattern(70_000, 1));
    assert_eq!(fs.find("dir/b.txt"), Some(b));
    // In place inside the extents: the size does not change.
    fs.write(a, 0, b"HELLO").unwrap();
    assert_eq!(content(&mut fs, a), b"HELLO world");
    assert_disjoint(&fs);
}

#[test]
fn files_larger_than_v1_replace_atomically_and_free_the_old_sectors() {
    let mut fs = AriFs::format(disk(3000), 3000).unwrap();
    let i = fs.create("apps/big.aripkg").unwrap();
    let free = fs.free_sectors();
    // Each replace needs room for the new copy only while it runs: ten
    // replaces of 500 KiB in 1.4 MiB of data.
    for round in 0..10 {
        let data = pattern(500_000 + round * 100, round as u32);
        fs.replace(i, &data).unwrap();
        assert_eq!(content(&mut fs, i), data);
        assert_eq!(fs.free_sectors(), free - (data.len() as u64).div_ceil(512));
    }
    let mut fs = AriFs::open(fs.into_device()).unwrap();
    assert_eq!(content(&mut fs, i), pattern(500_900, 9));
    // More than the free space beside the current copy: no room for both.
    assert!(1_100_000 > fs.free_sectors() * 512);
    assert_eq!(fs.replace(i, &pattern(1_100_000, 0)), Err(FsError::NoSpace));
    assert_eq!(
        content(&mut fs, i),
        pattern(500_900, 9),
        "a refused replace changes nothing"
    );
    fs.delete(i).unwrap();
    assert_eq!(fs.free_sectors(), free);
    let j = fs.create("apps/big.aripkg").unwrap();
    fs.replace(j, &pattern(1_100_000, 0)).unwrap();
    assert_eq!(content(&mut fs, j), pattern(1_100_000, 0));
}

#[test]
fn replace_survives_a_cut_at_every_write() {
    let old = pattern(40_000, 7);
    let new = pattern(90_000, 8);
    let mut fs = AriFs::format(disk(1000), 1000).unwrap();
    let i = fs.create("notes").unwrap();
    let other = fs.create("other").unwrap();
    fs.write(i, 0, &old).unwrap();
    fs.write(other, 0, b"untouched").unwrap();
    let old_sectors = fs.entry(i).unwrap().capacity_sectors();
    let base_free = fs.free_sectors();
    let base = fs.into_device().0;
    let mut fs = AriFs::open(CuttingDisk::new(&base, usize::MAX)).unwrap();
    fs.replace(i, &new).unwrap();
    let dev = fs.into_device();
    let total = dev.writes;
    assert_eq!(dev.flushes, 2, "the data, then the commit");
    let mut saw = [false; 2];
    for cut_at in 0..=total {
        for kept in KEPT {
            let mut fs = AriFs::open(CuttingDisk::new(&base, cut_at)).unwrap();
            assert_eq!(
                fs.replace(i, &new).is_ok(),
                cut_at >= total,
                "cut at {cut_at}"
            );
            let mut fs = AriFs::open(fs.into_device().power_off(kept)).unwrap();
            let got = content(&mut fs, i);
            assert!(
                got == old || got == new,
                "cut at {cut_at}, {kept:?}: mixed contents"
            );
            saw[(got == new) as usize] = true;
            assert_eq!(content(&mut fs, other), b"untouched");
            assert_disjoint(&fs);
            // The volume keeps serving, and no sector leaked: only the
            // one of "after" is in use where the old contents were.
            fs.replace(i, b"after").unwrap();
            let mut fs = AriFs::open(fs.into_device()).unwrap();
            assert_eq!(content(&mut fs, i), b"after");
            assert_eq!(fs.free_sectors(), base_free + old_sectors - 1);
        }
    }
    assert_eq!(
        saw,
        [true, true],
        "some cuts keep the old contents, some the new"
    );
}

#[test]
fn a_torn_commit_falls_back_to_the_previous_generation() {
    let mut fs = AriFs::format(disk(500), 500).unwrap();
    let i = fs.create("state").unwrap();
    fs.replace(i, b"one").unwrap();
    fs.replace(i, b"two").unwrap();
    let generation = fs.generation();
    let current = fs.current;
    let geo = fs.geometry();
    let mut img = fs.into_device().0;
    // One byte of the newest copy's last sector.
    let o = (geo.tables[current] + TABLE_SECTORS - 1) as usize * SECTOR + 100;
    img[o] ^= 0x40;
    let mut fs = AriFs::open(MemDisk(img)).unwrap();
    assert_eq!(fs.recovery(), Recovery::TornCommitDiscarded);
    assert_eq!(fs.generation(), generation - 1);
    assert_eq!(content(&mut fs, i), b"one");
    // The next commit overwrites the torn copy, not the good one.
    fs.replace(i, b"three").unwrap();
    let mut fs = AriFs::open(fs.into_device()).unwrap();
    assert_eq!(fs.recovery(), Recovery::Clean);
    assert_eq!(content(&mut fs, i), b"three");
    // Both copies broken: not mounted.
    let mut img = fs.into_device().0;
    for t in geo.tables {
        img[t as usize * SECTOR + 30] ^= 1;
    }
    assert_eq!(AriFs::open(MemDisk(img)).err(), Some(FsError::Corrupt));
}

#[test]
fn delete_and_rename_over_are_atomic() {
    let mut fs = AriFs::format(disk(800), 800).unwrap();
    let tmp = fs.create("apps/x.new").unwrap();
    let cur = fs.create("apps/x.aripkg").unwrap();
    fs.write(tmp, 0, &pattern(20_000, 1)).unwrap();
    fs.write(cur, 0, &pattern(10_000, 2)).unwrap();
    let base = fs.into_device().0;
    let mut fs = AriFs::open(CuttingDisk::new(&base, usize::MAX)).unwrap();
    fs.rename(tmp, "apps/x.aripkg").unwrap();
    let total = fs.into_device().writes;
    for cut_at in 0..=total {
        for kept in KEPT {
            let mut fs = AriFs::open(CuttingDisk::new(&base, cut_at)).unwrap();
            let _ = fs.rename(tmp, "apps/x.aripkg");
            let mut fs = AriFs::open(fs.into_device().power_off(kept)).unwrap();
            let x = fs.find("apps/x.aripkg").unwrap();
            match fs.find("apps/x.new") {
                Some(t) => {
                    assert_eq!(content(&mut fs, t), pattern(20_000, 1));
                    assert_eq!(content(&mut fs, x), pattern(10_000, 2), "cut at {cut_at}");
                }
                None => assert_eq!(content(&mut fs, x), pattern(20_000, 1), "cut at {cut_at}"),
            }
            assert_eq!(
                fs.entries().count(),
                1 + fs.find("apps/x.new").is_some() as usize
            );
        }
    }
    let mut fs = AriFs::open(MemDisk(base)).unwrap();
    let free = fs.free_sectors();
    fs.delete(cur).unwrap();
    assert_eq!(fs.find("apps/x.aripkg"), None);
    assert!(fs.free_sectors() > free);
    assert_eq!(fs.rename(tmp, "bad//name"), Err(FsError::BadName));
    assert_eq!(fs.delete(cur), Err(FsError::NotFound));
    fs.rename(tmp, "apps/x.new").unwrap();
    assert_eq!(fs.find("apps/x.new"), Some(tmp), "renaming to its own name");
}

#[test]
fn chunked_replace_rules() {
    let mut fs = AriFs::format(disk(800), 800).unwrap();
    let a = fs.create("a").unwrap();
    let b = fs.create("b").unwrap();
    fs.write(a, 0, b"old").unwrap();
    // It starts at zero and continues without gaps.
    assert_eq!(fs.replace_chunk(a, 10, b"x"), Err(FsError::BadTransaction));
    fs.replace_chunk(a, 0, &pattern(4096, 1)).unwrap();
    assert_eq!(
        fs.replace_chunk(a, 5000, b"x"),
        Err(FsError::BadTransaction)
    );
    assert_eq!(
        fs.replace_chunk(b, 4096, b"x"),
        Err(FsError::BadTransaction)
    );
    fs.replace_chunk(a, 4096, &pattern(4096, 2)).unwrap();
    // The commit covers exactly what was written.
    assert_eq!(fs.replace_commit(a, 4096), Err(FsError::BadTransaction));
    assert_eq!(fs.replace_commit(b, 8192), Err(FsError::BadTransaction));
    // Space taken by the replace in progress is not given to others.
    fs.write(b, 0, &pattern(30_000, 3)).unwrap();
    fs.replace_commit(a, 8192).unwrap();
    let mut want = pattern(4096, 1);
    want.extend(pattern(4096, 2));
    assert_eq!(content(&mut fs, a), want);
    assert_eq!(content(&mut fs, b), pattern(30_000, 3));
    assert_disjoint(&fs);
    // Abandoned: nothing changes and its sectors are free again.
    let free = fs.free_sectors();
    fs.replace_chunk(a, 0, &pattern(9000, 4)).unwrap();
    assert!(fs.free_sectors() < free);
    fs.abandon_replace();
    assert_eq!(fs.free_sectors(), free);
    assert_eq!(fs.replace_commit(a, 9000), Err(FsError::BadTransaction));
    assert_eq!(content(&mut fs, a), want);
    // The commit gives back what was taken ahead.
    fs.replace(a, b"tiny").unwrap();
    assert_eq!(fs.entry(a).unwrap().capacity_sectors(), 1);
    fs.replace(a, b"").unwrap();
    assert_eq!(fs.entry(a).unwrap().capacity_sectors(), 0);
    assert_eq!(content(&mut fs, a), b"");
}

#[test]
fn growth_prefers_contiguity_and_stops_at_four_extents() {
    let mut fs = AriFs::format(disk(1000), 1000).unwrap();
    let a = fs.create("a").unwrap();
    // Appends in small pieces stay in one extent while the space after it
    // is free.
    for k in 0..40 {
        fs.write(a, k * 1000, &pattern(1000, k)).unwrap();
    }
    assert_eq!(fs.entry(a).unwrap().extents().len(), 1);
    // Interleaved growth of two files: each extension adds an extent until
    // four, then the file cannot grow.
    let b = fs.create("b").unwrap();
    let c = fs.create("c").unwrap();
    let mut grown = 0;
    let mut refused = false;
    for k in 0..20 {
        let off = k * (GROW_SECTORS * 512);
        if fs
            .write(c, off, &pattern(GROW_SECTORS as usize * 512, k))
            .is_err()
        {
            refused = true;
            break;
        }
        fs.write(b, off, &pattern(GROW_SECTORS as usize * 512, k + 100))
            .unwrap_or(0);
        grown += 1;
    }
    assert!(refused, "space or extents run out");
    assert!(grown >= 1);
    assert!(fs.entry(c).unwrap().extents().len() <= MAX_EXTENTS);
    let mut fs = AriFs::open(fs.into_device()).unwrap();
    for k in 0..40 {
        let mut buf = [0u8; 1000];
        fs.read(a, k * 1000, &mut buf).unwrap();
        assert_eq!(&buf[..], &pattern(1000, k)[..]);
    }
    assert_disjoint(&fs);
}

#[test]
fn limits() {
    let mut fs = AriFs::format(disk(70_000), 70_000).unwrap();
    for n in 0..MAX_FILES {
        fs.create(&std::format!("f{n}")).unwrap();
    }
    assert_eq!(fs.create("one-more"), Err(FsError::NoSpace));
    assert_eq!(fs.create("f3"), Err(FsError::Exists));
    let i = fs.find("f0").unwrap();
    let max = std::vec![0x5Au8; MAX_FILE_BYTES as usize];
    fs.replace(i, &max).unwrap();
    assert_eq!(
        fs.replace(i, &std::vec![0u8; MAX_FILE_BYTES as usize + 1]),
        Err(FsError::OutOfRange)
    );
    assert_eq!(fs.write(i, MAX_FILE_BYTES, b"x"), Err(FsError::OutOfRange));
    let mut fs = AriFs::open(fs.into_device()).unwrap();
    assert_eq!(fs.entries().count(), MAX_FILES);
    assert_eq!(content(&mut fs, i), max);
    assert_eq!(AriFs::format(disk(100), 100).err(), Some(FsError::NoSpace));
}

#[test]
fn metadata_that_does_not_hold_is_not_mounted() {
    let build = |forge: &dyn Fn(&mut Entry)| {
        let mut fs = AriFs::format(disk(600), 600).unwrap();
        let a = fs.create("a").unwrap();
        let b = fs.create("b").unwrap();
        fs.write(a, 0, &pattern(5000, 1)).unwrap();
        fs.write(b, 0, &pattern(5000, 2)).unwrap();
        let mut e = *fs.entry(b).unwrap();
        forge(&mut e);
        // Committed with a valid CRC, as a bug or a forger would.
        fs.commit(&[(b, Some(e))]).unwrap();
        AriFs::open(fs.into_device()).err()
    };
    let a_start = AriFs::format(disk(600), 600)
        .map(|mut fs| {
            let a = fs.create("a").unwrap();
            fs.write(a, 0, &pattern(5000, 1)).unwrap();
            fs.entry(a).unwrap().extents()[0].start
        })
        .unwrap();
    assert_eq!(build(&|_| {}), None);
    assert_eq!(
        build(&|e| e.extents[0].start = a_start),
        Some(FsError::Corrupt),
        "overlap"
    );
    assert_eq!(
        build(&|e| e.extents[0].start = 2),
        Some(FsError::Corrupt),
        "inside the tables"
    );
    assert_eq!(
        build(&|e| e.extents[0].start = 590),
        Some(FsError::Corrupt),
        "past the end"
    );
    assert_eq!(
        build(&|e| e.size = 1_000_000),
        Some(FsError::Corrupt),
        "size past the extents"
    );
    assert_eq!(
        build(&|e| e.name[0] = b'a'),
        Some(FsError::Corrupt),
        "duplicate name"
    );
    assert_eq!(
        build(&|e| e.name[0] = b'/'),
        Some(FsError::Corrupt),
        "invalid name"
    );
    assert_eq!(
        build(&|e| {
            e.extents[1] = Extent {
                start: e.extents[0].start + 1,
                len: 1,
            };
            e.count = 2;
        }),
        Some(FsError::Corrupt),
        "two extents of one file overlapping"
    );
}

#[test]
fn a_failed_commit_poisons_until_reopened() {
    let mut fs = AriFs::format(disk(400), 400).unwrap();
    let i = fs.create("a").unwrap();
    fs.replace(i, b"old").unwrap();
    let base = fs.into_device().0;
    // Cut inside the commit: the data was flushed, the table was not.
    let mut fs = AriFs::open(CuttingDisk::new(&base, 3)).unwrap();
    assert!(fs.replace(i, b"new").is_err());
    assert!(fs.needs_reopen());
    assert_eq!(fs.read(i, 0, &mut [0u8; 8]), Err(FsError::Io));
    assert_eq!(fs.create("b"), Err(FsError::Io));
    let mut fs = AriFs::open(fs.into_device().power_off(Kept::None)).unwrap();
    assert_eq!(content(&mut fs, i), b"old");
}

fn v1_volume() -> (Vec<u8>, Vec<(&'static str, Vec<u8>)>) {
    let mut fs = v1::AriFs::format(disk(6110), 6110).unwrap();
    let files = std::vec![
        ("authorized_keys", pattern(91, 1)),
        ("updater/state", pattern(56, 2)),
        ("apps/hello.aripkg", pattern(30_000, 3)),
        ("empty", Vec::new()),
    ];
    for (name, data) in &files {
        let i = fs.create(name).unwrap();
        fs.write(i, 0, data).unwrap();
    }
    (fs.into_device().0, files)
}

fn assert_files<D: BlockDevice>(fs: &mut AriFs<D>, files: &[(&str, Vec<u8>)]) {
    assert_eq!(fs.entries().count(), files.len());
    for (name, data) in files {
        let i = fs.find(name).unwrap();
        assert_eq!(&content(fs, i), data, "{name}");
    }
}

#[test]
fn migration_keeps_every_file_in_place() {
    let (img, files) = v1_volume();
    assert_eq!(
        AriFs::open(MemDisk(img.clone())).err(),
        Some(FsError::BadMagic)
    );
    let mut fs = AriFs::migrate(MemDisk(img)).unwrap();
    assert_files(&mut fs, &files);
    assert_eq!(fs.geometry().data_lba, v1::DATA_LBA);
    // The migrated volume is a v2 volume: big files and the old ones.
    let big = fs.create("apps/big.aripkg").unwrap();
    fs.replace(big, &pattern(1_000_000, 9)).unwrap();
    let mut fs = AriFs::open(fs.into_device()).unwrap();
    assert_eq!(fs.recovery(), Recovery::Clean);
    assert_eq!(content(&mut fs, big), pattern(1_000_000, 9));
    let keys = fs.find("authorized_keys").unwrap();
    assert_eq!(content(&mut fs, keys), pattern(91, 1));
    // v1 no longer mounts it.
    assert_eq!(
        v1::AriFs::open(fs.into_device()).err(),
        Some(FsError::BadMagic)
    );
}

#[test]
fn migration_finishes_a_pending_v1_replace_first() {
    let (img, mut files) = v1_volume();
    let mut old = v1::AriFs::open(MemDisk(img)).unwrap();
    let i = old.find("updater/state").unwrap();
    old.stage_replace(i, b"staged").unwrap();
    files[1].1 = b"staged".to_vec();
    let mut fs = AriFs::migrate(old.into_device()).unwrap();
    assert_files(&mut fs, &files);
}

#[test]
fn migration_survives_a_cut_at_every_write() {
    let (img, files) = v1_volume();
    let fs = AriFs::migrate(CuttingDisk::new(&img, usize::MAX)).unwrap();
    let total = fs.into_device().writes;
    let mut saw = [false; 2];
    for cut_at in 0..=total {
        for kept in KEPT {
            let r = AriFs::migrate(CuttingDisk::new(&img, cut_at));
            assert_eq!(r.is_ok(), cut_at >= total);
            let dev = match r {
                Ok(fs) => fs.into_device(),
                Err(_) => {
                    // `migrate` consumed the device: redo the same writes
                    // to get it back at the cut.
                    let mut d = CuttingDisk::new(&img, cut_at);
                    let _ = migrate_into(&mut d);
                    d
                }
            };
            let disk = dev.power_off(kept);
            match AriFs::open(MemDisk(disk.0.clone())) {
                Ok(mut fs) => {
                    assert_files(&mut fs, &files);
                    saw[1] = true;
                }
                Err(FsError::BadMagic) => {
                    // Still v1: v1 mounts it with every file, and clears
                    // the marker so the stale tables cannot be taken later.
                    let mut old = v1::AriFs::open(disk).unwrap();
                    assert_eq!(old.entries().count(), files.len());
                    for (name, data) in &files {
                        let j = old.find(name).unwrap();
                        let mut buf = std::vec![0u8; data.len()];
                        old.read(j, 0, &mut buf).unwrap();
                        assert_eq!(&buf, data);
                    }
                    let mut s = [0u8; SECTOR];
                    old.into_device()
                        .read_sector(v1::JOURNAL_LBA, &mut s)
                        .unwrap();
                    assert!(!v1::is_migration_marker(&s));
                    saw[0] = true;
                }
                Err(e) => panic!("cut at {cut_at}, {kept:?}: {e:?}"),
            }
        }
    }
    assert_eq!(saw, [true, true]);
}

/// `migrate` over a borrowed device, for the cut test.
fn migrate_into(d: &mut CuttingDisk) -> Result<(), FsError> {
    struct Borrowed<'a>(&'a mut CuttingDisk);
    impl BlockDevice for Borrowed<'_> {
        fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
            self.0.read_sector(lba, buf)
        }
        fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
            self.0.write_sector(lba, buf)
        }
        fn flush(&mut self) -> Result<(), FsError> {
            self.0.flush()
        }
    }
    AriFs::migrate(Borrowed(d)).map(|_| ())
}

#[test]
fn a_torn_migration_superblock_is_completed_only_with_the_marker() {
    let (img, files) = v1_volume();
    let fs = AriFs::migrate(MemDisk(img.clone())).unwrap();
    let mut migrated = fs.into_device().0;
    // Half of the superblock written: neither v1 nor v2 verifies.
    migrated[..SECTOR / 2].copy_from_slice(&img[..SECTOR / 2]);
    let mut fs = AriFs::open(MemDisk(migrated.clone())).unwrap();
    assert_eq!(fs.recovery(), Recovery::MigrationCompleted);
    assert_files(&mut fs, &files);
    let fs = AriFs::open(fs.into_device()).unwrap();
    assert_eq!(
        fs.recovery(),
        Recovery::Clean,
        "the superblock was written again"
    );
    // Without the marker the same tables are not taken: a file's contents
    // in v1's journal area could be anything, a forged table included.
    let o = v1::JOURNAL_LBA as usize * SECTOR;
    migrated[o..o + SECTOR].fill(0);
    assert!(AriFs::open(MemDisk(migrated)).is_err());
}

#[test]
fn a_file_cannot_forge_the_migration() {
    // A v1 file replaced with the bytes of a valid v2 table lands in v1's
    // journal data area; with a broken superblock, it is still not taken.
    let (img, _) = v1_volume();
    let forged = AriFs::migrate(MemDisk(img.clone()))
        .unwrap()
        .into_device()
        .0;
    let o = v1::JOURNAL_DATA_LBA as usize * SECTOR;
    let table = forged[o..o + 32 * SECTOR].to_vec();
    let mut old = v1::AriFs::open(MemDisk(img)).unwrap();
    let i = old.find("apps/hello.aripkg").unwrap();
    old.replace(i, &table[..v1::JOURNAL_CAPACITY_BYTES.min(table.len())])
        .unwrap();
    let mut img = old.into_device().0;
    img[3] ^= 0xFF; // the superblock breaks
    assert!(AriFs::open(MemDisk(img)).is_err());
}

#[test]
fn the_volume_is_small_on_the_stack() {
    // The table is on the heap (fsd's frames have a 16 KiB budget) and
    // small there too.
    assert!(core::mem::size_of::<AriFs<MemDisk>>() <= 256);
    assert!(MAX_FILES * core::mem::size_of::<Option<Entry>>() <= 20 * 1024);
}

#[test]
fn a_volume_opens_either_version_and_migrates() {
    use crate::{Mounted, Volume};
    let (img, files) = v1_volume();
    let mut vol = Volume::open(MemDisk(img)).unwrap();
    assert_eq!(vol.version(), 1);
    assert_eq!(vol.max_replace(), v1::JOURNAL_CAPACITY_BYTES as u32);
    let keys = vol.find("authorized_keys").unwrap();
    assert_eq!(vol.delete(keys), Err(FsError::Unsupported));
    assert_eq!(vol.rename(keys, "x"), Err(FsError::Unsupported));
    let vol = vol.migrate().unwrap();
    assert_eq!(vol.version(), 2);
    let mut vol = Volume::open(vol.into_device()).unwrap();
    assert_eq!(vol.version(), 2);
    assert_eq!(vol.mounted(), Mounted::V2(Recovery::Clean));
    assert_eq!(vol.max_replace(), MAX_FILE_BYTES);
    assert_eq!(vol.entries().count(), files.len());
    for (name, data) in &files {
        let i = vol.find(name).unwrap();
        assert_eq!(vol.info(i).unwrap().size as usize, data.len());
        let mut buf = std::vec![0u8; data.len()];
        assert_eq!(vol.read(i, 0, &mut buf).unwrap(), data.len());
        assert_eq!(&buf, data);
    }
    // Migrating again changes nothing.
    let vol = vol.migrate().unwrap();
    assert_eq!(vol.version(), 2);
    // A migration whose superblock write was cut opens as v2.
    let (img, _) = v1_volume();
    let mut done = AriFs::migrate(MemDisk(img.clone()))
        .unwrap()
        .into_device()
        .0;
    done[..SECTOR].copy_from_slice(&img[..SECTOR]);
    done[0] ^= 0xFF;
    let vol = Volume::open(MemDisk(done)).unwrap();
    assert_eq!(vol.mounted(), Mounted::V2(Recovery::MigrationCompleted));
}
