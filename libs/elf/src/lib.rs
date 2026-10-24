// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Minimal ELF64 parser (x86-64, little endian, `ET_EXEC`) to load the
//! kernel: header, `PT_LOAD` program headers and entry point.
//!
//! `no_std` + `alloc`. Tested on the host with `cargo test -p aricode-elf`.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub const PT_LOAD: u32 = 1;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub flags: u32,
}

impl Segment {
    pub fn writable(&self) -> bool {
        self.flags & PF_W != 0
    }
    pub fn executable(&self) -> bool {
        self.flags & PF_X != 0
    }
    /// Exclusive end in memory. Guaranteed overflow-free by `Elf::parse`.
    pub fn end(&self) -> u64 {
        self.vaddr + self.memsz
    }
    /// Page-rounded [start, end) range. Guaranteed overflow-free by `Elf::parse`.
    pub fn page_range(&self) -> (u64, u64) {
        (
            self.vaddr & !PAGE_MASK,
            (self.end() + PAGE_MASK) & !PAGE_MASK,
        )
    }
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.vaddr && addr < self.end()
    }
    /// The whole pages of a writable segment past its file contents, which
    /// hold only zeros (`.bss`, a runtime's heap arena): a loader may back
    /// them on first touch instead of at load (performance audit 731c398,
    /// finding D). `[start, end)`, or `None` when there is no such page.
    /// Overflow-free for segments from `Elf::parse` (`filesz <= memsz`,
    /// the end fits), as `page_range`.
    pub fn zero_fill_pages(&self) -> Option<(u64, u64)> {
        if !self.writable() {
            return None;
        }
        let (_, end) = self.page_range();
        let start = (self.vaddr + self.filesz + PAGE_MASK) & !PAGE_MASK;
        (start < end).then_some((start, end))
    }
}

const PAGE_MASK: u64 = 0xFFF;

pub struct Elf<'a> {
    data: &'a [u8],
    pub entry: u64,
    /// `PT_LOAD` segments with `memsz > 0`, validated: no overflow, no
    /// per-page overlap. Empty ones are dropped in `parse`.
    pub segments: Vec<Segment>,
}

impl core::fmt::Debug for Elf<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Elf")
            .field("len", &self.data.len())
            .field("entry", &format_args!("{:#x}", self.entry))
            .field("segments", &self.segments)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    NotExecutable,
    NotX86_64,
    BadProgramHeader,
    /// No `PT_LOAD` segments.
    NoSegments,
    /// `vaddr + memsz` (or its page rounding) is not representable.
    AddressOverflow,
    /// Two `PT_LOAD` segments overlap in memory (at page granularity).
    OverlappingSegments,
    /// `e_entry` does not fall inside an executable `PT_LOAD` segment.
    EntryOutsideText,
}

fn u16_at(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn u32_at(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off + 4].try_into().unwrap())
}
fn u64_at(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, ElfError> {
        if data.len() < 64 {
            return Err(ElfError::TooShort);
        }
        if &data[0..4] != b"\x7FELF" {
            return Err(ElfError::BadMagic);
        }
        if data[4] != 2 {
            return Err(ElfError::Not64Bit);
        }
        if data[5] != 1 {
            return Err(ElfError::NotLittleEndian);
        }
        if u16_at(data, 16) != 2 {
            return Err(ElfError::NotExecutable); // ET_EXEC
        }
        if u16_at(data, 18) != 0x3E {
            return Err(ElfError::NotX86_64);
        }
        let entry = u64_at(data, 24);
        let phoff = u64_at(data, 32) as usize;
        let phentsize = u16_at(data, 54) as usize;
        let phnum = u16_at(data, 56) as usize;
        if phentsize < 56
            || phoff
                .checked_add(phentsize * phnum)
                .is_none_or(|e| e > data.len())
        {
            return Err(ElfError::BadProgramHeader);
        }
        let mut segments = Vec::new();
        for i in 0..phnum {
            let p = phoff + i * phentsize;
            if u32_at(data, p) != PT_LOAD {
                continue;
            }
            let seg = Segment {
                flags: u32_at(data, p + 4),
                offset: u64_at(data, p + 8),
                vaddr: u64_at(data, p + 16),
                filesz: u64_at(data, p + 32),
                memsz: u64_at(data, p + 40),
            };
            if seg.filesz > seg.memsz
                || (seg.offset as usize)
                    .checked_add(seg.filesz as usize)
                    .is_none_or(|e| e > data.len())
            {
                return Err(ElfError::BadProgramHeader);
            }
            // The whole range, page rounding included, must be representable.
            if seg
                .vaddr
                .checked_add(seg.memsz)
                .and_then(|e| e.checked_add(PAGE_MASK))
                .is_none()
            {
                return Err(ElfError::AddressOverflow);
            }
            // A PT_LOAD with memsz == 0 occupies no memory: it is dropped here so
            // that neither allocation, copy nor mapping sees it. If it were
            // kept, its `page_range` (a full page if vaddr is not
            // aligned) could overwrite another segment's permissions.
            if seg.memsz == 0 {
                continue;
            }
            segments.push(seg);
        }
        if segments.is_empty() {
            return Err(ElfError::NoSegments);
        }
        Self::check_no_overlap(&segments)?;
        if !segments.iter().any(|s| s.executable() && s.contains(entry)) {
            return Err(ElfError::EntryOutsideText);
        }
        Ok(Self {
            data,
            entry,
            segments,
        })
    }

    /// Two segments cannot share pages: allocation and mapping are done
    /// per page with per-segment permissions. (Empty ones are already gone.)
    fn check_no_overlap(segments: &[Segment]) -> Result<(), ElfError> {
        for (i, a) in segments.iter().enumerate() {
            let (a0, a1) = a.page_range();
            for b in segments.iter().skip(i + 1) {
                let (b0, b1) = b.page_range();
                if a0 < b1 && b0 < a1 {
                    return Err(ElfError::OverlappingSegments);
                }
            }
        }
        Ok(())
    }

    pub fn segment_bytes(&self, seg: &Segment) -> &'a [u8] {
        &self.data[seg.offset as usize..(seg.offset + seg.filesz) as usize]
    }

    /// Virtual range [min, max) covered by the segments, page aligned.
    /// No overflow: `parse` already validated every segment.
    pub fn virt_range(&self) -> (u64, u64) {
        self.segments
            .iter()
            .map(Segment::page_range)
            .fold((u64::MAX, 0), |(lo, hi), (a, b)| (lo.min(a), hi.max(b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn only_whole_zero_pages_of_writable_segments_fill_on_demand() {
        let seg = |vaddr, filesz, memsz, flags| Segment {
            offset: 0,
            vaddr,
            filesz,
            memsz,
            flags,
        };
        // .data of 0x1800 bytes then .bss to 0x45000: the page holding the
        // end of the file contents stays eager.
        assert_eq!(
            seg(0x40_0000, 0x1800, 0x45000, PF_W).zero_fill_pages(),
            Some((0x40_2000, 0x44_5000))
        );
        // A page-aligned end of file: the next page is the first zero one.
        assert_eq!(
            seg(0x40_0000, 0x2000, 0x3000, PF_W).zero_fill_pages(),
            Some((0x40_2000, 0x40_3000))
        );
        // Only zeros: every whole page past an unaligned start.
        assert_eq!(
            seg(0x40_0010, 0, 0x2000, PF_W).zero_fill_pages(),
            Some((0x40_1000, 0x40_3000))
        );
        // Unaligned with file contents crossing a page, and an unaligned
        // segment whose file contents fill it.
        assert_eq!(
            seg(0x40_0800, 0x1000, 0x5000, PF_W).zero_fill_pages(),
            Some((0x40_2000, 0x40_6000))
        );
        assert_eq!(seg(0x40_0800, 0x1800, 0x1800, PF_W).zero_fill_pages(), None);
        // Nothing past the file contents, or not writable.
        assert_eq!(seg(0x40_0000, 0x1800, 0x1900, PF_W).zero_fill_pages(), None);
        assert_eq!(seg(0x40_0000, 0, 0x8000, 0).zero_fill_pages(), None);
    }

    /// Builds a minimal ELF64 with `n` PT_LOAD program headers.
    fn build_elf(entry: u64, segs: &[(u64, u64, u64, u32)]) -> Vec<u8> {
        let mut d = vec![0u8; 64 + 56 * segs.len() + 64];
        d[0..4].copy_from_slice(b"\x7FELF");
        d[4] = 2; // 64 bits
        d[5] = 1; // little endian
        d[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        d[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // x86-64
        d[24..32].copy_from_slice(&entry.to_le_bytes());
        d[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
        d[54..56].copy_from_slice(&56u16.to_le_bytes());
        d[56..58].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        for (i, (vaddr, filesz, memsz, flags)) in segs.iter().enumerate() {
            let p = 64 + i * 56;
            d[p..p + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
            d[p + 4..p + 8].copy_from_slice(&flags.to_le_bytes());
            let off = 64 + 56 * segs.len();
            d[p + 8..p + 16].copy_from_slice(&(off as u64).to_le_bytes());
            d[p + 16..p + 24].copy_from_slice(&vaddr.to_le_bytes());
            d[p + 32..p + 40].copy_from_slice(&filesz.to_le_bytes());
            d[p + 40..p + 48].copy_from_slice(&memsz.to_le_bytes());
        }
        d
    }

    const TEXT: u64 = 0xFFFF_FFFF_8000_0000;

    #[test]
    fn rejects_entry_outside_executable_segment() {
        // entry = 0 (audit, finding 3)
        let d = build_elf(0, &[(TEXT, 16, 16, PF_X | 4)]);
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::EntryOutsideText);
        // entry inside an NX segment
        let d = build_elf(
            TEXT + 0x3000,
            &[(TEXT, 16, 16, PF_X | 4), (TEXT + 0x3000, 8, 8, PF_W | 4)],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::EntryOutsideText);
        // entry right at the end of text (exclusive)
        let d = build_elf(TEXT + 16, &[(TEXT, 16, 16, PF_X | 4)]);
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::EntryOutsideText);
    }

    #[test]
    fn rejects_address_overflow() {
        // vaddr + memsz wraps around (audit, finding 1)
        let d = build_elf(
            TEXT,
            &[
                (TEXT, 1, 1, PF_X | 4),
                (0xFFFF_FFFF_FFFF_FFF8, 8, 8, PF_W | 4),
            ],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::AddressOverflow);
        // vaddr + memsz fits but the page rounding does not
        let d = build_elf(
            TEXT,
            &[
                (TEXT, 1, 1, PF_X | 4),
                (0xFFFF_FFFF_FFFF_F000, 0, 0xFF0, PF_W | 4),
            ],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::AddressOverflow);
        // huge memsz
        let d = build_elf(TEXT, &[(TEXT, 1, u64::MAX, PF_X | 4)]);
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::AddressOverflow);
    }

    #[test]
    fn rejects_overlapping_segments() {
        let d = build_elf(
            TEXT,
            &[(TEXT, 16, 16, PF_X | 4), (TEXT + 0x800, 8, 8, PF_W | 4)],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::OverlappingSegments);
    }

    #[test]
    fn empty_segments_are_dropped_and_cannot_clobber_text() {
        // Audit review, finding 1: empty, unaligned RW PT_LOAD
        // on the entry page. It must vanish from the result.
        let d = build_elf(
            TEXT + 0x10,
            &[
                (TEXT, 16, 0x100, PF_X | 4),
                (TEXT + 0x10, 0, 0, PF_W | 4),
                (TEXT + 0x3000, 8, 8, PF_W | 4),
            ],
        );
        let elf = Elf::parse(&d).unwrap();
        assert_eq!(elf.segments.len(), 2);
        assert!(elf.segments.iter().all(|s| s.memsz > 0));
        assert_eq!(elf.virt_range(), (TEXT, TEXT + 0x4000));
        // An ELF whose only PT_LOAD is empty has nothing to load.
        let d = build_elf(TEXT, &[(TEXT, 0, 0, PF_X | 4)]);
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::NoSegments);
    }

    #[test]
    fn rejects_no_segments() {
        assert_eq!(
            Elf::parse(&build_elf(0, &[])).unwrap_err(),
            ElfError::NoSegments
        );
    }

    #[test]
    fn parses_entry_and_segments() {
        let d = build_elf(
            0xFFFF_FFFF_8000_0410,
            &[
                (0xFFFF_FFFF_8000_0000, 16, 0x1000, PF_X | 4),
                (0xFFFF_FFFF_8000_3000, 8, 0x2000, PF_W | 4),
            ],
        );
        let elf = Elf::parse(&d).unwrap();
        assert_eq!(elf.entry, 0xFFFF_FFFF_8000_0410);
        assert_eq!(elf.segments.len(), 2);
        assert!(elf.segments[0].executable() && !elf.segments[0].writable());
        assert!(elf.segments[1].writable() && !elf.segments[1].executable());
        assert_eq!(
            elf.virt_range(),
            (0xFFFF_FFFF_8000_0000, 0xFFFF_FFFF_8000_5000)
        );
        assert_eq!(elf.segment_bytes(&elf.segments[0]).len(), 16);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut d = build_elf(TEXT, &[(TEXT, 4, 4, PF_X)]);
        d[0] = 0;
        assert!(matches!(Elf::parse(&d), Err(ElfError::BadMagic)));
    }

    #[test]
    fn rejects_short_and_truncated() {
        assert!(matches!(Elf::parse(&[0u8; 10]), Err(ElfError::TooShort)));
        let d = build_elf(0, &[(0x1000, 4, 4, 0)]);
        let truncated = &d[..64 + 56 - 1];
        assert!(matches!(
            Elf::parse(truncated),
            Err(ElfError::BadProgramHeader)
        ));
    }

    #[test]
    fn rejects_wrong_class_or_machine() {
        let mut d = build_elf(TEXT, &[(TEXT, 4, 4, PF_X)]);
        d[4] = 1;
        assert!(matches!(Elf::parse(&d), Err(ElfError::Not64Bit)));
        let mut d = build_elf(TEXT, &[(TEXT, 4, 4, PF_X)]);
        d[18..20].copy_from_slice(&0xB7u16.to_le_bytes());
        assert!(matches!(Elf::parse(&d), Err(ElfError::NotX86_64)));
    }

    #[test]
    fn rejects_segment_with_filesz_beyond_data() {
        let mut d = build_elf(0, &[(0x1000, 4, 4, 0)]);
        d[64 + 32..64 + 40].copy_from_slice(&10_000u64.to_le_bytes()); // huge filesz
        d[64 + 40..64 + 48].copy_from_slice(&10_000u64.to_le_bytes());
        assert!(matches!(Elf::parse(&d), Err(ElfError::BadProgramHeader)));
    }
}
