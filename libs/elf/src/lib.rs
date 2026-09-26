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
    /// Fin exclusivo en memoria. Garantizado sin overflow por `Elf::parse`.
    pub fn end(&self) -> u64 {
        self.vaddr + self.memsz
    }
    /// Rango [inicio, fin) redondeado a página. Garantizado sin overflow por `Elf::parse`.
    pub fn page_range(&self) -> (u64, u64) {
        (
            self.vaddr & !PAGE_MASK,
            (self.end() + PAGE_MASK) & !PAGE_MASK,
        )
    }
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.vaddr && addr < self.end()
    }
}

const PAGE_MASK: u64 = 0xFFF;

pub struct Elf<'a> {
    data: &'a [u8],
    pub entry: u64,
    /// Segmentos `PT_LOAD` con `memsz > 0`, validados: sin overflow, sin
    /// solapes por página. Los vacíos se descartan en `parse`.
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
    /// Sin segmentos `PT_LOAD`.
    NoSegments,
    /// `vaddr + memsz` (o su redondeo a página) no es representable.
    AddressOverflow,
    /// Dos segmentos `PT_LOAD` se solapan en memoria (a granularidad de página).
    OverlappingSegments,
    /// `e_entry` no cae dentro de un segmento `PT_LOAD` ejecutable.
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
            // Todo el rango, incluido el redondeo a página, debe ser representable.
            if seg
                .vaddr
                .checked_add(seg.memsz)
                .and_then(|e| e.checked_add(PAGE_MASK))
                .is_none()
            {
                return Err(ElfError::AddressOverflow);
            }
            // Un PT_LOAD con memsz == 0 no ocupa memoria: se descarta aquí para
            // que ni la reserva, ni la copia ni el mapeo lo vean. Si se
            // conservara, su `page_range` (una página completa si vaddr no
            // está alineado) podría pisar los permisos de otro segmento.
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

    /// Dos segmentos no pueden compartir páginas: la reserva y el mapeo se
    /// hacen por página y con permisos por segmento. (Los vacíos ya no están.)
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

    /// Rango virtual [min, max) cubierto por los segmentos, alineado a página.
    /// Sin overflow: `parse` ya validó cada segmento.
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

    const TEXT: u64 = 0xFFFF_FFFF_8000_0000;

    #[test]
    fn rejects_entry_outside_executable_segment() {
        // entry = 0 (auditoría, hallazgo 3)
        let d = build_elf(0, &[(TEXT, 16, 16, PF_X | 4)]);
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::EntryOutsideText);
        // entry dentro de un segmento NX
        let d = build_elf(
            TEXT + 0x3000,
            &[(TEXT, 16, 16, PF_X | 4), (TEXT + 0x3000, 8, 8, PF_W | 4)],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::EntryOutsideText);
        // entry justo al final del texto (exclusivo)
        let d = build_elf(TEXT + 16, &[(TEXT, 16, 16, PF_X | 4)]);
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::EntryOutsideText);
    }

    #[test]
    fn rejects_address_overflow() {
        // vaddr + memsz envuelve (auditoría, hallazgo 1)
        let d = build_elf(
            TEXT,
            &[
                (TEXT, 1, 1, PF_X | 4),
                (0xFFFF_FFFF_FFFF_FFF8, 8, 8, PF_W | 4),
            ],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::AddressOverflow);
        // vaddr + memsz cabe pero el redondeo a página no
        let d = build_elf(
            TEXT,
            &[
                (TEXT, 1, 1, PF_X | 4),
                (0xFFFF_FFFF_FFFF_F000, 0, 0xFF0, PF_W | 4),
            ],
        );
        assert_eq!(Elf::parse(&d).unwrap_err(), ElfError::AddressOverflow);
        // memsz enorme
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
        // Revision de auditoria, hallazgo 1: PT_LOAD RW vacio, sin alinear,
        // sobre la pagina de la entrada. Debe desaparecer del resultado.
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
        // Un ELF cuyo unico PT_LOAD es vacio no tiene nada que cargar.
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
        d[64 + 32..64 + 40].copy_from_slice(&10_000u64.to_le_bytes()); // filesz gigante
        d[64 + 40..64 + 48].copy_from_slice(&10_000u64.to_le_bytes());
        assert!(matches!(Elf::parse(&d), Err(ElfError::BadProgramHeader)));
    }
}
