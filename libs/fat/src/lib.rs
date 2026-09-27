//! FAT32 mínimo (Fase 34): construye la partición EFI de la imagen USB en el
//! host (`Fat32Builder`) y la lee de vuelta (`Fat32`) para verificarla. Solo
//! lo que exige un firmware UEFI: BPB de FAT32, FSInfo, dos copias de la
//! FAT, directorios con entradas 8.3 y nombres largos (VFAT), sin fechas.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub const SECTOR: usize = 512;
const RESERVED_SECTORS: u32 = 32;
const FATS: u32 = 2;
const ROOT_CLUSTER: u32 = 2;
const EOC: u32 = 0x0FFF_FFFF;
/// Por debajo de esto un firmware toma la partición por FAT16.
const MIN_CLUSTERS: u32 = 65_525;
const MAX_CLUSTERS: u32 = 0x0FFF_FFF5;
const ENTRY: usize = 32;

pub const ATTR_DIRECTORY: u8 = 0x10;
pub const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatError {
    TooSmall,
    TooLarge,
    Full,
    BadName,
    NotFound,
    NotADirectory,
    IsADirectory,
    Exists,
    Corrupt,
}

/// Nombre corto 8.3 en mayúsculas si `name` ya lo es (sin necesitar LFN).
fn short_exact(name: &str) -> Option<[u8; 11]> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, ""),
    };
    let valid_chars = |s: &str| {
        s.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || b"!#$%&'()-@^_`{}~".contains(&b)
        })
    };
    if stem.is_empty() || stem.len() > 8 || ext.len() > 3 || !valid_chars(stem) || !valid_chars(ext)
    {
        return None;
    }
    let mut out = [b' '; 11];
    out[..stem.len()].copy_from_slice(stem.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(out)
}

/// Nombre corto derivado (`ARICOD~1`) único entre `existing`.
fn short_derived(name: &str, existing: &[[u8; 11]]) -> [u8; 11] {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let clean = |s: &str, max: usize| -> Vec<u8> {
        s.bytes()
            .filter(|b| b.is_ascii_alphanumeric())
            .map(|b| b.to_ascii_uppercase())
            .take(max)
            .collect()
    };
    let base = clean(stem, 6);
    let ext = clean(ext, 3);
    for n in 1..=999u32 {
        let mut out = [b' '; 11];
        let tail = alloc::format!("~{n}");
        let keep = (8 - tail.len()).min(base.len());
        out[..keep].copy_from_slice(&base[..keep]);
        out[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        out[8..8 + ext.len()].copy_from_slice(&ext);
        if !existing.contains(&out) {
            return out;
        }
    }
    [b'?'; 11]
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &c| {
        ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c)
    })
}

/// Entradas de directorio (LFN + corta) para `name`.
fn dir_entries(
    name: &str,
    short: [u8; 11],
    lfn: bool,
    attr: u8,
    cluster: u32,
    size: u32,
) -> Vec<[u8; ENTRY]> {
    let mut out = Vec::new();
    if lfn {
        let chars: Vec<u16> = name.encode_utf16().collect();
        let count = chars.len().div_ceil(13);
        let sum = lfn_checksum(&short);
        for seq in (1..=count).rev() {
            let mut e = [0u8; ENTRY];
            e[0] = seq as u8 | if seq == count { 0x40 } else { 0 };
            e[11] = ATTR_LFN;
            e[13] = sum;
            let slots: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
            for (i, off) in slots.iter().enumerate() {
                let idx = (seq - 1) * 13 + i;
                let v = match idx.cmp(&chars.len()) {
                    core::cmp::Ordering::Less => chars[idx],
                    core::cmp::Ordering::Equal => 0x0000,
                    core::cmp::Ordering::Greater => 0xFFFF,
                };
                e[*off..*off + 2].copy_from_slice(&v.to_le_bytes());
            }
            out.push(e);
        }
    }
    let mut e = [0u8; ENTRY];
    e[..11].copy_from_slice(&short);
    e[11] = attr;
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    out.push(e);
    out
}

/// Constructor de una partición FAT32 en memoria.
pub struct Fat32Builder {
    sectors_per_cluster: u32,
    total_sectors: u32,
    fat_size: u32,
    clusters: u32,
    fat: Vec<u32>,
    data: Vec<u8>,
    next_free: u32,
}

impl Fat32Builder {
    /// Partición de `total_sectors` sectores de 512 bytes con clústeres de
    /// `sectors_per_cluster` (potencia de dos, 1..=128).
    pub fn new(total_sectors: u32, sectors_per_cluster: u32) -> Result<Self, FatError> {
        assert!(
            sectors_per_cluster.is_power_of_two() && sectors_per_cluster <= 128,
            "sectores por cluster invalidos"
        );
        let spc = sectors_per_cluster;
        let mut fat_size = 0u32;
        let mut clusters;
        loop {
            let data_sectors = total_sectors
                .checked_sub(RESERVED_SECTORS + FATS * fat_size)
                .ok_or(FatError::TooSmall)?;
            clusters = data_sectors / spc;
            let needed = ((clusters as u64 + 2) * 4).div_ceil(SECTOR as u64) as u32;
            if needed <= fat_size {
                break;
            }
            fat_size = needed;
        }
        if clusters < MIN_CLUSTERS {
            return Err(FatError::TooSmall);
        }
        if clusters >= MAX_CLUSTERS {
            return Err(FatError::TooLarge);
        }
        let mut fat = alloc::vec![0u32; clusters as usize + 2];
        fat[0] = 0x0FFF_FFF8;
        fat[1] = EOC;
        fat[ROOT_CLUSTER as usize] = EOC;
        Ok(Self {
            sectors_per_cluster: spc,
            total_sectors,
            fat_size,
            clusters,
            fat,
            data: alloc::vec![0u8; clusters as usize * spc as usize * SECTOR],
            next_free: ROOT_CLUSTER + 1,
        })
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }

    pub fn free_clusters(&self) -> u32 {
        self.clusters + 2 - self.next_free
    }

    fn cluster(&mut self, c: u32) -> &mut [u8] {
        let cb = self.cluster_bytes();
        let o = (c as usize - 2) * cb;
        &mut self.data[o..o + cb]
    }

    fn cluster_ref(&self, c: u32) -> &[u8] {
        let cb = self.cluster_bytes();
        let o = (c as usize - 2) * cb;
        &self.data[o..o + cb]
    }

    fn alloc_chain(&mut self, bytes: usize) -> Result<u32, FatError> {
        let n = bytes.div_ceil(self.cluster_bytes()).max(1) as u32;
        if self.next_free + n > self.clusters + 2 {
            return Err(FatError::Full);
        }
        let first = self.next_free;
        for i in 0..n {
            let c = first + i;
            self.fat[c as usize] = if i + 1 == n { EOC } else { c + 1 };
        }
        self.next_free += n;
        Ok(first)
    }

    fn chain(&self, first: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = first;
        while (2..self.clusters + 2).contains(&c) && out.len() <= self.clusters as usize {
            out.push(c);
            c = self.fat[c as usize];
        }
        out
    }

    fn write_chain(&mut self, first: u32, data: &[u8]) {
        let cb = self.cluster_bytes();
        for (i, c) in self.chain(first).into_iter().enumerate() {
            let start = i * cb;
            if start >= data.len() {
                break;
            }
            let end = (start + cb).min(data.len());
            self.cluster(c)[..end - start].copy_from_slice(&data[start..end]);
        }
    }

    /// Nombres cortos ya usados en el directorio y, si existe, la entrada
    /// (atributos, clúster) del nombre buscado (comparación sin mayúsculas).
    fn scan_dir(&self, dir: u32, wanted: &str) -> (Vec<[u8; 11]>, Option<(u8, u32)>) {
        let mut shorts = Vec::new();
        let mut found = None;
        let mut lfn: Vec<u16> = Vec::new();
        for c in self.chain(dir) {
            let data = self.cluster_ref(c);
            for e in data.as_chunks::<ENTRY>().0 {
                if e[0] == 0x00 {
                    return (shorts, found);
                }
                if e[0] == 0xE5 {
                    lfn.clear();
                    continue;
                }
                if e[11] == ATTR_LFN {
                    let mut piece = Vec::new();
                    for off in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                        piece.push(u16::from_le_bytes([e[off], e[off + 1]]));
                    }
                    let mut v = piece;
                    v.extend(lfn.iter());
                    lfn = v;
                    continue;
                }
                let mut short = [0u8; 11];
                short.copy_from_slice(&e[..11]);
                shorts.push(short);
                let long: String =
                    char::decode_utf16(lfn.iter().copied().take_while(|c| *c != 0 && *c != 0xFFFF))
                        .map(|r| r.unwrap_or('?'))
                        .collect();
                lfn.clear();
                let short_name = short_to_string(&short);
                if long.eq_ignore_ascii_case(wanted) || short_name.eq_ignore_ascii_case(wanted) {
                    let cluster = (u16::from_le_bytes([e[26], e[27]]) as u32)
                        | (u16::from_le_bytes([e[20], e[21]]) as u32) << 16;
                    found = Some((e[11], cluster));
                }
            }
        }
        (shorts, found)
    }

    /// Añade entradas contiguas al directorio, extendiéndolo si hace falta.
    fn add_entries(&mut self, dir: u32, entries: &[[u8; ENTRY]]) -> Result<(), FatError> {
        let per_cluster = self.cluster_bytes() / ENTRY;
        let chain = self.chain(dir);
        // Primer hueco libre: las entradas se escriben siempre al final.
        let mut used = 0usize;
        'outer: for (i, c) in chain.iter().enumerate() {
            let data = self.cluster_ref(*c);
            for (j, e) in data.as_chunks::<ENTRY>().0.iter().enumerate() {
                if e[0] == 0x00 {
                    used = i * per_cluster + j;
                    break 'outer;
                }
                used = i * per_cluster + j + 1;
            }
        }
        let capacity = chain.len() * per_cluster;
        let mut chain = chain;
        while used + entries.len() > chain.len() * per_cluster {
            let last = *chain.last().ok_or(FatError::Corrupt)?;
            let new = self.alloc_chain(1)?;
            self.fat[last as usize] = new;
            chain.push(new);
        }
        let _ = capacity;
        for (k, e) in entries.iter().enumerate() {
            let slot = used + k;
            let c = chain[slot / per_cluster];
            let off = (slot % per_cluster) * ENTRY;
            self.cluster(c)[off..off + ENTRY].copy_from_slice(e);
        }
        Ok(())
    }

    fn name_entries(
        &self,
        dir: u32,
        name: &str,
        attr: u8,
        cluster: u32,
        size: u32,
    ) -> Result<Vec<[u8; ENTRY]>, FatError> {
        if name.is_empty()
            || name.len() > 255
            || name.contains(['/', '\\'])
            || name == "."
            || name == ".."
        {
            return Err(FatError::BadName);
        }
        let (shorts, found) = self.scan_dir(dir, name);
        if found.is_some() {
            return Err(FatError::Exists);
        }
        let (short, lfn) = match short_exact(name) {
            Some(s) if !shorts.contains(&s) => (s, false),
            _ => (short_derived(name, &shorts), true),
        };
        Ok(dir_entries(name, short, lfn, attr, cluster, size))
    }

    /// Clúster del subdirectorio `name` de `dir`, creándolo si no existe.
    fn subdir(&mut self, dir: u32, name: &str) -> Result<u32, FatError> {
        if let (_, Some((attr, cluster))) = self.scan_dir(dir, name) {
            return if attr & ATTR_DIRECTORY != 0 {
                Ok(cluster)
            } else {
                Err(FatError::NotADirectory)
            };
        }
        let cluster = self.alloc_chain(1)?;
        let mut dot = [0u8; ENTRY];
        dot[..11].copy_from_slice(b".          ");
        dot[11] = ATTR_DIRECTORY;
        dot[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        dot[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        let mut dotdot = dot;
        dotdot[..11].copy_from_slice(b"..         ");
        let parent = if dir == ROOT_CLUSTER { 0 } else { dir };
        dotdot[26..28].copy_from_slice(&(parent as u16).to_le_bytes());
        dotdot[20..22].copy_from_slice(&((parent >> 16) as u16).to_le_bytes());
        self.cluster(cluster)[..ENTRY].copy_from_slice(&dot);
        self.cluster(cluster)[ENTRY..2 * ENTRY].copy_from_slice(&dotdot);
        let entries = self.name_entries(dir, name, ATTR_DIRECTORY, cluster, 0)?;
        self.add_entries(dir, &entries)?;
        Ok(cluster)
    }

    /// Añade `data` como el fichero `path` (`a/b/c.txt`), creando directorios.
    pub fn add_file(&mut self, path: &str, data: &[u8]) -> Result<(), FatError> {
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let name = parts.pop().ok_or(FatError::BadName)?;
        let mut dir = ROOT_CLUSTER;
        for d in parts {
            dir = self.subdir(dir, d)?;
        }
        if let (_, Some(_)) = self.scan_dir(dir, name) {
            return Err(FatError::Exists);
        }
        let cluster = if data.is_empty() {
            0
        } else {
            let c = self.alloc_chain(data.len())?;
            self.write_chain(c, data);
            c
        };
        let entries = self.name_entries(dir, name, ATTR_ARCHIVE, cluster, data.len() as u32)?;
        self.add_entries(dir, &entries)
    }

    fn boot_sector(&self) -> [u8; SECTOR] {
        let mut b = [0u8; SECTOR];
        b[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        b[3..11].copy_from_slice(b"ARICODE ");
        b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        b[13] = self.sectors_per_cluster as u8;
        b[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
        b[16] = FATS as u8;
        b[21] = 0xF8;
        b[24..26].copy_from_slice(&63u16.to_le_bytes());
        b[26..28].copy_from_slice(&255u16.to_le_bytes());
        b[32..36].copy_from_slice(&self.total_sectors.to_le_bytes());
        b[36..40].copy_from_slice(&self.fat_size.to_le_bytes());
        b[44..48].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
        b[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo
        b[50..52].copy_from_slice(&6u16.to_le_bytes()); // copia del sector de arranque
        b[64] = 0x80;
        b[66] = 0x29;
        b[67..71].copy_from_slice(&0x4152_4943u32.to_le_bytes());
        b[71..82].copy_from_slice(b"ARICODEOS  ");
        b[82..90].copy_from_slice(b"FAT32   ");
        b[510] = 0x55;
        b[511] = 0xAA;
        b
    }

    fn fsinfo(&self) -> [u8; SECTOR] {
        let mut b = [0u8; SECTOR];
        b[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        b[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        b[488..492].copy_from_slice(&self.free_clusters().to_le_bytes());
        b[492..496].copy_from_slice(&self.next_free.to_le_bytes());
        b[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
        b
    }

    /// Imagen completa de la partición.
    pub fn finish(self) -> Vec<u8> {
        let mut img = alloc::vec![0u8; self.total_sectors as usize * SECTOR];
        let boot = self.boot_sector();
        let info = self.fsinfo();
        img[..SECTOR].copy_from_slice(&boot);
        img[SECTOR..2 * SECTOR].copy_from_slice(&info);
        img[6 * SECTOR..7 * SECTOR].copy_from_slice(&boot);
        img[7 * SECTOR..8 * SECTOR].copy_from_slice(&info);
        let fat_bytes = self.fat_size as usize * SECTOR;
        for copy in 0..FATS as usize {
            let start = (RESERVED_SECTORS as usize + copy * self.fat_size as usize) * SECTOR;
            for (i, e) in self.fat.iter().enumerate() {
                img[start + 4 * i..start + 4 * i + 4].copy_from_slice(&e.to_le_bytes());
            }
            let _ = fat_bytes;
        }
        let data_start = (RESERVED_SECTORS + FATS * self.fat_size) as usize * SECTOR;
        img[data_start..data_start + self.data.len()].copy_from_slice(&self.data);
        img
    }
}

fn short_to_string(short: &[u8; 11]) -> String {
    let stem = core::str::from_utf8(&short[..8]).unwrap_or("").trim_end();
    let ext = core::str::from_utf8(&short[8..]).unwrap_or("").trim_end();
    if ext.is_empty() {
        String::from(stem)
    } else {
        alloc::format!("{stem}.{ext}")
    }
}

/// Entrada de directorio leída.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub short: String,
    pub attr: u8,
    pub cluster: u32,
    pub size: u32,
}

/// Lector de una partición FAT32 en memoria.
pub struct Fat32<'a> {
    img: &'a [u8],
    sectors_per_cluster: u32,
    fat_start: usize,
    data_start: usize,
    clusters: u32,
    root: u32,
}

impl<'a> Fat32<'a> {
    pub fn parse(img: &'a [u8]) -> Result<Self, FatError> {
        if img.len() < 8 * SECTOR || img[510] != 0x55 || img[511] != 0xAA {
            return Err(FatError::Corrupt);
        }
        let u16_at = |o: usize| u16::from_le_bytes([img[o], img[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([img[o], img[o + 1], img[o + 2], img[o + 3]]);
        if u16_at(11) as usize != SECTOR || u16_at(17) != 0 || u16_at(22) != 0 || img[13] == 0 {
            return Err(FatError::Corrupt);
        }
        let spc = img[13] as u32;
        let reserved = u16_at(14) as u32;
        let fats = img[16] as u32;
        let total = u32_at(32);
        let fat_size = u32_at(36);
        let root = u32_at(44);
        let data_sector = reserved + fats * fat_size;
        if total as usize * SECTOR > img.len() || data_sector >= total {
            return Err(FatError::Corrupt);
        }
        Ok(Self {
            img,
            sectors_per_cluster: spc,
            fat_start: reserved as usize * SECTOR,
            data_start: data_sector as usize * SECTOR,
            clusters: (total - data_sector) / spc,
            root,
        })
    }

    pub fn total_clusters(&self) -> u32 {
        self.clusters
    }

    fn fat_entry(&self, c: u32) -> u32 {
        let o = self.fat_start + 4 * c as usize;
        u32::from_le_bytes([
            self.img[o],
            self.img[o + 1],
            self.img[o + 2],
            self.img[o + 3],
        ]) & 0x0FFF_FFFF
    }

    fn cluster(&self, c: u32) -> &'a [u8] {
        let cb = self.sectors_per_cluster as usize * SECTOR;
        let o = self.data_start + (c as usize - 2) * cb;
        &self.img[o..o + cb]
    }

    fn read_chain(&self, first: u32) -> Result<Vec<u8>, FatError> {
        let mut out = Vec::new();
        let mut c = first;
        let mut steps = 0;
        while (2..self.clusters + 2).contains(&c) {
            out.extend_from_slice(self.cluster(c));
            c = self.fat_entry(c);
            steps += 1;
            if steps > self.clusters {
                return Err(FatError::Corrupt);
            }
        }
        if c < 0x0FFF_FFF8 && first != 0 {
            return Err(FatError::Corrupt);
        }
        Ok(out)
    }

    /// Entradas del directorio cuyo primer clúster es `cluster` (sin `.`/`..`).
    pub fn read_dir(&self, cluster: u32) -> Result<Vec<DirEntry>, FatError> {
        let data = self.read_chain(cluster)?;
        let mut out = Vec::new();
        let mut lfn: Vec<u16> = Vec::new();
        for e in data.as_chunks::<ENTRY>().0 {
            if e[0] == 0x00 {
                break;
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if e[11] == ATTR_LFN {
                let mut piece = Vec::new();
                for off in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                    piece.push(u16::from_le_bytes([e[off], e[off + 1]]));
                }
                piece.extend(lfn.iter());
                lfn = piece;
                continue;
            }
            let mut short = [0u8; 11];
            short.copy_from_slice(&e[..11]);
            let short_s = short_to_string(&short);
            if short_s == "." || short_s == ".." {
                lfn.clear();
                continue;
            }
            let long: String =
                char::decode_utf16(lfn.iter().copied().take_while(|c| *c != 0 && *c != 0xFFFF))
                    .map(|r| r.unwrap_or('?'))
                    .collect();
            lfn.clear();
            out.push(DirEntry {
                name: if long.is_empty() {
                    short_s.clone()
                } else {
                    long
                },
                short: short_s,
                attr: e[11],
                cluster: (u16::from_le_bytes([e[26], e[27]]) as u32)
                    | (u16::from_le_bytes([e[20], e[21]]) as u32) << 16,
                size: u32::from_le_bytes([e[28], e[29], e[30], e[31]]),
            });
        }
        Ok(out)
    }

    pub fn root_dir(&self) -> Result<Vec<DirEntry>, FatError> {
        self.read_dir(self.root)
    }

    /// Contenido de `path` (`a/b/c.txt`, sin distinguir mayúsculas).
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FatError> {
        let mut dir = self.root;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let (name, dirs) = parts.split_last().ok_or(FatError::BadName)?;
        for d in dirs {
            let e = self
                .read_dir(dir)?
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(d) || e.short.eq_ignore_ascii_case(d))
                .ok_or(FatError::NotFound)?;
            if e.attr & ATTR_DIRECTORY == 0 {
                return Err(FatError::NotADirectory);
            }
            dir = e.cluster;
        }
        let e = self
            .read_dir(dir)?
            .into_iter()
            .find(|e| e.name.eq_ignore_ascii_case(name) || e.short.eq_ignore_ascii_case(name))
            .ok_or(FatError::NotFound)?;
        if e.attr & ATTR_DIRECTORY != 0 {
            return Err(FatError::IsADirectory);
        }
        if e.size == 0 {
            return Ok(Vec::new());
        }
        let mut data = self.read_chain(e.cluster)?;
        data.truncate(e.size as usize);
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn size_limits() {
        assert_eq!(Fat32Builder::new(1000, 1).err(), Some(FatError::TooSmall));
        assert!(
            Fat32Builder::new(131_072, 1).is_ok(),
            "64 MiB con clusteres de 512"
        );
        assert_eq!(
            Fat32Builder::new(65_600 * 8, 8).err(),
            Some(FatError::TooSmall),
            "con 4 KiB no llega a 65525 clusteres"
        );
    }

    #[test]
    fn esp_layout_round_trip() {
        let mut b = Fat32Builder::new(131_072, 1).unwrap();
        let efi = pattern(4000, 1);
        let kernel = pattern(1_000_000, 2);
        b.add_file("EFI/BOOT/BOOTX64.EFI", &efi).unwrap();
        b.add_file("aricodeos/kernel.elf", &kernel).unwrap();
        b.add_file("aricodeos/cmdline.txt", b"qemu_exit shell")
            .unwrap();
        b.add_file("aricodeos/vacio", b"").unwrap();
        b.add_file("README", b"hola").unwrap();
        assert_eq!(b.add_file("README", b"otro"), Err(FatError::Exists));
        assert_eq!(b.add_file("README/x", b"x"), Err(FatError::NotADirectory));
        assert_eq!(b.add_file("", b"x"), Err(FatError::BadName));
        let img = b.finish();
        assert_eq!(img.len(), 131_072 * 512);
        assert_eq!(&img[0..3], &[0xEB, 0x58, 0x90]);
        assert_eq!(&img[82..90], b"FAT32   ");
        assert_eq!(
            &img[6 * 512..6 * 512 + 3],
            &[0xEB, 0x58, 0x90],
            "copia del sector de arranque"
        );
        assert_eq!(
            u32::from_le_bytes(img[512..516].try_into().unwrap()),
            0x4161_5252
        );
        let fat_start = 32 * 512;
        assert_eq!(
            u32::from_le_bytes(img[fat_start..fat_start + 4].try_into().unwrap()),
            0x0FFF_FFF8
        );

        let fs = Fat32::parse(&img).unwrap();
        assert!(fs.total_clusters() >= 65_525);
        assert_eq!(fs.read_file("EFI/BOOT/BOOTX64.EFI").unwrap(), efi);
        assert_eq!(
            fs.read_file("efi/boot/bootx64.efi").unwrap(),
            efi,
            "sin mayusculas"
        );
        assert_eq!(fs.read_file("aricodeos/kernel.elf").unwrap(), kernel);
        assert_eq!(
            fs.read_file("ARICOD~1/KERNEL.ELF").unwrap(),
            kernel,
            "nombre corto"
        );
        assert_eq!(
            fs.read_file("aricodeos/cmdline.txt").unwrap(),
            b"qemu_exit shell"
        );
        assert_eq!(fs.read_file("aricodeos/vacio").unwrap(), b"");
        assert_eq!(fs.read_file("README").unwrap(), b"hola");
        assert_eq!(
            fs.read_file("aricodeos/nada").err(),
            Some(FatError::NotFound)
        );
        assert_eq!(
            fs.read_file("aricodeos").err(),
            Some(FatError::IsADirectory)
        );
        let root = fs.root_dir().unwrap();
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["EFI", "aricodeos", "README"]);
        let ari = root.iter().find(|e| e.name == "aricodeos").unwrap();
        assert_eq!(ari.short, "ARICOD~1");
        assert!(ari.attr & ATTR_DIRECTORY != 0);
        let efi_dir = root.iter().find(|e| e.name == "EFI").unwrap();
        assert_eq!(efi_dir.short, "EFI", "8.3 exacto sin LFN");
    }

    #[test]
    fn many_long_names_extend_the_directory() {
        let mut b = Fat32Builder::new(131_072, 1).unwrap();
        for i in 0..100 {
            let name = alloc::format!("dir/fichero-con-nombre-largo-{i}.txt");
            b.add_file(&name, &[i as u8; 700]).unwrap();
        }
        let img = b.finish();
        let fs = Fat32::parse(&img).unwrap();
        let dir = fs.root_dir().unwrap();
        let d = dir.iter().find(|e| e.name == "dir").unwrap();
        let entries = fs.read_dir(d.cluster).unwrap();
        assert_eq!(entries.len(), 100);
        let shorts: alloc::collections::BTreeSet<&str> =
            entries.iter().map(|e| e.short.as_str()).collect();
        assert_eq!(shorts.len(), 100, "nombres cortos unicos");
        assert_eq!(
            fs.read_file("dir/fichero-con-nombre-largo-42.txt").unwrap(),
            [42u8; 700]
        );
        assert_eq!(
            fs.read_file("dir/fichero-con-nombre-largo-99.txt").unwrap(),
            [99u8; 700]
        );
    }

    #[test]
    fn lfn_checksum_matches_spec_example() {
        // Nombre corto "ARICOD~1   ": suma conocida calculada con el algoritmo de Microsoft.
        let s = *b"ARICOD~1   ";
        let expected = s.iter().fold(0u8, |sum, &c| {
            ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c)
        });
        assert_eq!(lfn_checksum(&s), expected);
        assert_eq!(short_exact("KERNEL.ELF"), Some(*b"KERNEL  ELF"));
        assert_eq!(short_exact("kernel.elf"), None);
        assert_eq!(short_exact("aricodeos"), None);
        assert_eq!(short_exact("EFI"), Some(*b"EFI        "));
        assert_eq!(short_derived("aricodeos", &[]), *b"ARICOD~1   ");
        assert_eq!(
            short_derived("aricodeos", &[*b"ARICOD~1   "]),
            *b"ARICOD~2   "
        );
        assert_eq!(short_derived("kernel.elf", &[]), *b"KERNEL~1ELF");
    }
}
