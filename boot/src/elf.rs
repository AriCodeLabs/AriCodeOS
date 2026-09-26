//! Parser mínimo de ELF64 para cargar el kernel. Solo lo necesario:
//! cabecera, program headers `PT_LOAD` y punto de entrada.

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
