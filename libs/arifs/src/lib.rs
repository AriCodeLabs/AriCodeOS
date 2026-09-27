//! AriFS v0 (Fase 20): deliberadamente simple (sección 34.2). Un directorio
//! plano de hasta 32 ficheros con capacidad fija; sin fragmentación, sin
//! journaling. Suficiente para que un proceso abra, lea y escriba por IPC.
//!
//! ```text
//! LBA 0        superbloque (512 bytes): magia, versión, sectores, tabla, datos
//! LBA 1..4     tabla: 32 entradas de 64 bytes (nombre, LBA, capacidad, tamaño)
//! LBA 5..      datos: bloques de capacidad fija asignados de forma incremental
//! ```
//!
//! Los LBA son relativos a la partición. Todo el acceso pasa por
//! [`BlockDevice`], así que la librería se prueba en host sobre un `Vec`.

#![no_std]

pub const SECTOR: usize = 512;
const MAGIC: &[u8; 8] = b"ARIFS\x00\x00\x01";
pub const MAX_FILES: usize = 32;
pub const ENTRY_LEN: usize = 64;
pub const NAME_LEN: usize = 32;
pub const TABLE_LBA: u64 = 1;
pub const TABLE_SECTORS: u64 = (MAX_FILES * ENTRY_LEN / SECTOR) as u64;
pub const DATA_LBA: u64 = TABLE_LBA + TABLE_SECTORS;
/// Capacidad por fichero: 16 sectores (8 KiB).
pub const FILE_CAPACITY_SECTORS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Io,
    BadMagic,
    NotFound,
    Exists,
    NoSpace,
    BadName,
    /// Lectura o escritura fuera de la capacidad del fichero.
    OutOfRange,
}

/// Dispositivo de bloques de 512 bytes.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError>;
    fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub total_sectors: u64,
    /// Siguiente LBA libre para datos.
    pub data_next: u64,
}

impl Superblock {
    fn encode(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(MAGIC);
        s[8..16].copy_from_slice(&self.total_sectors.to_le_bytes());
        s[16..24].copy_from_slice(&self.data_next.to_le_bytes());
        s
    }
    fn decode(s: &[u8; SECTOR]) -> Result<Self, FsError> {
        if &s[..8] != MAGIC {
            return Err(FsError::BadMagic);
        }
        Ok(Self {
            total_sectors: u64::from_le_bytes(s[8..16].try_into().unwrap()),
            data_next: u64::from_le_bytes(s[16..24].try_into().unwrap()),
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
    fn encode(&self) -> [u8; ENTRY_LEN] {
        let mut e = [0u8; ENTRY_LEN];
        e[..NAME_LEN].copy_from_slice(&self.name);
        e[32] = self.name_len;
        e[33] = 1; // en uso
        e[40..48].copy_from_slice(&self.start_lba.to_le_bytes());
        e[48..52].copy_from_slice(&self.capacity_sectors.to_le_bytes());
        e[52..56].copy_from_slice(&self.size.to_le_bytes());
        e
    }
    fn decode(e: &[u8]) -> Option<Self> {
        if e[33] == 0 {
            return None;
        }
        let mut name = [0u8; NAME_LEN];
        name.copy_from_slice(&e[..NAME_LEN]);
        Some(Self {
            name,
            name_len: e[32].min(NAME_LEN as u8),
            start_lba: u64::from_le_bytes(e[40..48].try_into().unwrap()),
            capacity_sectors: u32::from_le_bytes(e[48..52].try_into().unwrap()),
            size: u32::from_le_bytes(e[52..56].try_into().unwrap()),
        })
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

/// Un sistema de archivos abierto sobre un dispositivo.
pub struct AriFs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
    table: [Option<Entry>; MAX_FILES],
}

impl<D: BlockDevice> AriFs<D> {
    /// Abre un sistema de archivos existente (lee superbloque y tabla).
    pub fn open(mut dev: D) -> Result<Self, FsError> {
        let mut s = [0u8; SECTOR];
        dev.read_sector(0, &mut s)?;
        let sb = Superblock::decode(&s)?;
        let mut table = [None; MAX_FILES];
        for t in 0..TABLE_SECTORS {
            dev.read_sector(TABLE_LBA + t, &mut s)?;
            for (i, e) in s.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
                table[t as usize * (SECTOR / ENTRY_LEN) + i] = Entry::decode(e);
            }
        }
        Ok(Self { dev, sb, table })
    }

    /// Crea un sistema de archivos vacío sobre `total_sectors` sectores.
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
        Ok(Self {
            dev,
            sb,
            table: [None; MAX_FILES],
        })
    }

    pub fn superblock(&self) -> Superblock {
        self.sb
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

    /// Crea un fichero vacío con capacidad fija. Devuelve su índice.
    pub fn create(&mut self, name: &str) -> Result<usize, FsError> {
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
        if start + FILE_CAPACITY_SECTORS as u64 > self.sb.total_sectors {
            return Err(FsError::NoSpace);
        }
        let mut entry = Entry {
            name: [0; NAME_LEN],
            name_len: name.len() as u8,
            start_lba: start,
            capacity_sectors: FILE_CAPACITY_SECTORS,
            size: 0,
        };
        entry.name[..name.len()].copy_from_slice(name.as_bytes());
        self.table[index] = Some(entry);
        self.sb.data_next = start + FILE_CAPACITY_SECTORS as u64;
        self.dev.write_sector(0, &self.sb.encode())?;
        self.write_table_entry(index)?;
        Ok(index)
    }

    /// Lee hasta `buf.len()` bytes desde `offset`. Devuelve los bytes leídos
    /// (0 al final del fichero).
    pub fn read(&mut self, index: usize, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        if offset >= e.size {
            return Ok(0);
        }
        let len = buf.len().min((e.size - offset) as usize);
        let mut done = 0;
        let mut s = [0u8; SECTOR];
        while done < len {
            let pos = offset as usize + done;
            let lba = e.start_lba + (pos / SECTOR) as u64;
            self.dev.read_sector(lba, &mut s)?;
            let in_sector = pos % SECTOR;
            let n = (SECTOR - in_sector).min(len - done);
            buf[done..done + n].copy_from_slice(&s[in_sector..in_sector + n]);
            done += n;
        }
        Ok(done)
    }

    /// Escribe `data` en `offset` (sin huecos: `offset <= size`) dentro de la
    /// capacidad. Actualiza el tamaño.
    pub fn write(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        if offset > e.size {
            return Err(FsError::OutOfRange);
        }
        let end = offset as usize + data.len();
        if end > e.capacity_bytes() as usize {
            return Err(FsError::OutOfRange);
        }
        let mut done = 0;
        let mut s = [0u8; SECTOR];
        while done < data.len() {
            let pos = offset as usize + done;
            let lba = e.start_lba + (pos / SECTOR) as u64;
            let in_sector = pos % SECTOR;
            let n = (SECTOR - in_sector).min(data.len() - done);
            if n != SECTOR {
                self.dev.read_sector(lba, &mut s)?;
            }
            s[in_sector..in_sector + n].copy_from_slice(&data[done..done + n]);
            self.dev.write_sector(lba, &s)?;
            done += n;
        }
        if end as u32 > e.size {
            let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
            entry.size = end as u32;
            self.write_table_entry(index)?;
        }
        Ok(done)
    }

    /// Trunca a cero bytes.
    pub fn truncate(&mut self, index: usize) -> Result<(), FsError> {
        let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
        entry.size = 0;
        self.write_table_entry(index)
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
    }

    fn disk(sectors: usize) -> MemDisk {
        MemDisk(std::vec![0u8; sectors * SECTOR])
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
        assert_eq!(fs.read(i, 17, &mut buf).unwrap(), 0, "fin de fichero");
        // Reabrir desde el dispositivo: tabla y superbloque persistidos.
        let dev = fs.into_device();
        let mut fs = AriFs::open(dev).unwrap();
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
        // Sobrescritura parcial en medio.
        assert_eq!(fs.write(i, 510, b"XXXX").unwrap(), 4);
        assert_eq!(fs.read(i, 508, &mut buf[..8]).unwrap(), 8);
        assert_eq!(&buf[2..6], b"XXXX");
        assert_eq!(fs.entry(i).unwrap().size, 1504);
        fs.truncate(i).unwrap();
        assert_eq!(fs.entry(i).unwrap().size, 0);
    }

    #[test]
    fn errors() {
        let mut fs = AriFs::format(disk(200), 200).unwrap();
        assert_eq!(fs.create("con espacio"), Err(FsError::BadName));
        assert_eq!(fs.create("/abs"), Err(FsError::BadName));
        assert_eq!(fs.create("dir/"), Err(FsError::BadName));
        assert_eq!(fs.create("a//b"), Err(FsError::BadName));
        assert!(
            fs.create("app/notas").is_ok(),
            "prefijo de directorio privado"
        );
        assert_eq!(fs.create(""), Err(FsError::BadName));
        let i = fs.create("a").unwrap();
        assert_eq!(fs.create("a"), Err(FsError::Exists));
        assert_eq!(fs.write(i, 10, b"x"), Err(FsError::OutOfRange), "hueco");
        let too_big = std::vec![0u8; FILE_CAPACITY_SECTORS as usize * SECTOR + 1];
        assert_eq!(fs.write(i, 0, &too_big), Err(FsError::OutOfRange));
        assert_eq!(fs.read(99, 0, &mut [0u8; 4]), Err(FsError::NotFound));
        assert_eq!(fs.find("zzz"), None);
        // Sin espacio para mas ficheros de 16 sectores en 200 sectores.
        let mut created = 1;
        while fs.create(&std::format!("f{created}")).is_ok() {
            created += 1;
        }
        assert!(created > 5 && created <= MAX_FILES);
        assert!(AriFs::open(disk(10)).is_err(), "sin magia");
        assert_eq!(AriFs::format(disk(5), 5).err(), Some(FsError::NoSpace));
    }
}
