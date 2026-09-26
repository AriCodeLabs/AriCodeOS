//! Parser mínimo de ELF64 (x86-64, little endian, `ET_EXEC`) para cargar el
//! kernel: cabecera, program headers `PT_LOAD` y punto de entrada.
//!
//! `no_std` + `alloc`. Se prueba en host con `cargo test -p aricode-elf`.

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
}

pub struct Elf<'a> {
    data: &'a [u8],
    pub entry: u64,
    pub segments: Vec<Segment>,
}

#[derive(Debug)]
pub enum ElfError {
    TooShort,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    NotExecutable,
    NotX86_64,
    BadProgramHeader,
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
            segments.push(seg);
        }
        Ok(Self {
            data,
            entry,
            segments,
        })
    }

    pub fn segment_bytes(&self, seg: &Segment) -> &'a [u8] {
        &self.data[seg.offset as usize..(seg.offset + seg.filesz) as usize]
    }

    /// Rango virtual [min, max) cubierto por los segmentos, alineado a página.
    pub fn virt_range(&self) -> (u64, u64) {
        let mut lo = u64::MAX;
        let mut hi = 0;
        for s in &self.segments {
            lo = lo.min(s.vaddr & !0xFFF);
            hi = hi.max((s.vaddr + s.memsz + 0xFFF) & !0xFFF);
        }
        (lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Construye un ELF64 mínimo con `n` program headers PT_LOAD.
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

    #[test]
    fn parses_entry_and_segments() {
        let d = build_elf(
            0xFFFF_FFFF_8000_0410,
            &[
                (0xFFFF_FFFF_8000_0000, 16, 16, PF_X | 4),
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
        let mut d = build_elf(0, &[]);
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
        let mut d = build_elf(0, &[]);
        d[4] = 1;
        assert!(matches!(Elf::parse(&d), Err(ElfError::Not64Bit)));
        let mut d = build_elf(0, &[]);
        d[18..20].copy_from_slice(&0xB7u16.to_le_bytes());
        assert!(matches!(Elf::parse(&d), Err(ElfError::NotX86_64)));
    }

    #[test]
    fn rejects_segment_with_filesz_beyond_data() {
        let mut d = build_elf(0, &[(0x1000, 4, 4, 0)]);
        d[64 + 32..64 + 40].copy_from_slice(&10_000u64.to_le_bytes()); // filesz gigante
        d[64 + 40..64 + 48].copy_from_slice(&10_000u64.to_le_bytes());
        assert!(matches!(Elf::parse(&d), Err(ElfError::BadProgramHeader)));
    }
}
