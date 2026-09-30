//! AriFS v1 (Fase 20 y etapa 2, ADR 0014): un directorio plano de hasta 32
//! ficheros con capacidad fija, sin fragmentación, con sumas de control en
//! los metadatos, un journal de reemplazo atómico y un contrato de `flush`.
//!
//! ```text
//! LBA 0        superbloque (512 bytes): magia, versión, sectores, siguiente
//!              LBA de datos, CRC32
//! LBA 1..4     tabla: 32 entradas de 64 bytes (nombre, LBA, capacidad,
//!              tamaño, CRC32 por entrada)
//! LBA 5        cabecera del journal: estado, fichero, longitud, CRC32 de
//!              los datos, secuencia, CRC32 de la cabecera
//! LBA 6..69    datos del journal (la capacidad de un fichero)
//! LBA 70..     datos: bloques de capacidad fija asignados de forma incremental
//! ```
//!
//! Contrato de persistencia:
//!
//! - un sector de 512 bytes se escribe entero o no se escribe (supuesto del
//!   soporte); nada se asume sobre el orden entre sectores salvo tras `flush`;
//! - `write` escribe en el sitio: tras un corte cada sector tiene el contenido
//!   viejo o el nuevo, y el tamaño solo crece cuando los datos ya están;
//! - `replace` es atómico: tras un corte el fichero tiene entero el contenido
//!   anterior o entero el nuevo. Los datos y su CRC van primero al journal,
//!   luego se marca pendiente, se aplican y se limpia, con `flush` entre
//!   fases; `open` rehace un reemplazo pendiente válido y descarta una
//!   cabecera rota (el reemplazo nunca llegó a comprometerse);
//! - `open` rechaza (`Corrupt`) un superbloque o una entrada en uso cuyo
//!   CRC no cuadra: no se monta un sistema del que no se puede fiar.
//!
//! Los LBA son relativos a la partición. Todo el acceso pasa por
//! [`BlockDevice`], así que la librería se prueba en host sobre un `Vec`,
//! incluidos cortes en cada escritura del reemplazo.

#![no_std]

pub const SECTOR: usize = 512;
const MAGIC: &[u8; 8] = b"ARIFS\x00\x00\x02";
const JOURNAL_MAGIC: &[u8; 8] = b"ARIJRNL\x00";
pub const MAX_FILES: usize = 32;
pub const ENTRY_LEN: usize = 64;
pub const NAME_LEN: usize = 32;
pub const TABLE_LBA: u64 = 1;
pub const TABLE_SECTORS: u64 = (MAX_FILES * ENTRY_LEN / SECTOR) as u64;
/// Capacidad por fichero: 64 sectores (32 KiB).
pub const FILE_CAPACITY_SECTORS: u32 = 64;
pub const JOURNAL_LBA: u64 = TABLE_LBA + TABLE_SECTORS;
pub const JOURNAL_DATA_LBA: u64 = JOURNAL_LBA + 1;
pub const DATA_LBA: u64 = JOURNAL_DATA_LBA + FILE_CAPACITY_SECTORS as u64;
/// Máximo de un reemplazo atómico: el área de datos del journal.
pub const JOURNAL_CAPACITY_BYTES: usize = FILE_CAPACITY_SECTORS as usize * SECTOR;

const JOURNAL_CLEAR: u32 = 0;
const JOURNAL_PENDING: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Io,
    BadMagic,
    /// Un CRC de metadatos no cuadra: no se monta.
    Corrupt,
    NotFound,
    Exists,
    NoSpace,
    BadName,
    /// Lectura o escritura fuera de la capacidad del fichero.
    OutOfRange,
}

/// Dispositivo de bloques de 512 bytes. `flush` hace duraderas las
/// escrituras anteriores antes de volver.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError>;
    fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError>;
    fn flush(&mut self) -> Result<(), FsError>;
    /// Varios sectores seguidos (`buf.len()` múltiplo de `SECTOR`, hasta
    /// `MAX_BATCH_SECTORS`); por defecto uno a uno. Un dispositivo por IPC
    /// los agrupa en una petición (etapa 5).
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

/// Sectores por lote en `read_sectors`/`write_sectors`.
pub const MAX_BATCH_SECTORS: usize = 8;
const BATCH_BYTES: usize = MAX_BATCH_SECTORS * SECTOR;

/// CRC-32 (IEEE 802.3, reflejado) incremental, sin tabla: los metadatos son
/// pequeños. Empieza en `crc32_init()` y termina con `!`.
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
    /// Siguiente LBA libre para datos.
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
    /// El CRC (bytes 36..40) cubre el resto de la entrada.
    fn encode(&self) -> [u8; ENTRY_LEN] {
        let mut e = [0u8; ENTRY_LEN];
        e[..NAME_LEN].copy_from_slice(&self.name);
        e[32] = self.name_len;
        e[33] = 1; // en uso
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
    /// `Ok(None)` si la entrada está libre; `Corrupt` si está en uso y el
    /// CRC no cuadra.
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

/// Cabecera del journal de reemplazo (un sector). Opaca fuera de la
/// librería: solo la devuelve `stage_replace`.
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

    /// `None` si la cabecera no es válida (magia o CRC): equivale a limpia.
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

/// Qué encontró `open` en el journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Journal limpio.
    Clean,
    /// Un reemplazo comprometido y no aplicado del todo se rehízo.
    Replayed { index: usize },
    /// Cabecera rota o datos que no cuadran con su CRC: el reemplazo nunca
    /// llegó a comprometerse y se descartó.
    Discarded,
}

/// Un sistema de archivos abierto sobre un dispositivo.
pub struct AriFs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
    table: [Option<Entry>; MAX_FILES],
    sequence: u64,
    recovery: Recovery,
}

impl<D: BlockDevice> AriFs<D> {
    /// Abre un sistema de archivos existente (lee superbloque y tabla,
    /// comprueba los CRC y rehace o descarta un reemplazo pendiente).
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
        let mut fs = Self {
            dev,
            sb,
            table,
            sequence: 0,
            recovery: Recovery::Clean,
        };
        fs.recover()?;
        Ok(fs)
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
        dev.write_sector(JOURNAL_LBA, &Journal::CLEAR.encode())?;
        dev.flush()?;
        Ok(Self {
            dev,
            sb,
            table: [None; MAX_FILES],
            sequence: 0,
            recovery: Recovery::Clean,
        })
    }

    pub fn superblock(&self) -> Superblock {
        self.sb
    }

    /// Resultado de la recuperación al abrir.
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

    /// Crea un fichero vacío con la capacidad fija habitual. Devuelve su índice.
    pub fn create(&mut self, name: &str) -> Result<usize, FsError> {
        self.create_sized(name, FILE_CAPACITY_SECTORS)
    }

    /// Como `create`, con capacidad `sectors` (múltiplo de
    /// `FILE_CAPACITY_SECTORS`): ficheros grandes, como los paquetes
    /// instalados (ADR 0017). `replace` solo admite hasta la capacidad del
    /// journal (`FILE_CAPACITY_SECTORS`).
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
        // Primero el superbloque: un corte entre ambos deja espacio
        // reservado sin entrada, nunca una entrada sobre espacio no reservado.
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
        self.copy_from_device(e.start_lba, offset as usize, &mut buf[..len])?;
        Ok(len)
    }

    /// Copia `out.len()` bytes desde la posición `pos` (bytes) del área que
    /// empieza en `base`: sectores parciales uno a uno, los enteros por lotes.
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

    /// Escribe `data` en la posición `pos` del área que empieza en `base`:
    /// los sectores parciales se leen antes; los enteros van por lotes.
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

    /// Escribe `data` en `offset` (sin huecos: `offset <= size`) dentro de la
    /// capacidad, en el sitio. Actualiza el tamaño después de los datos. No
    /// es duradero hasta `flush`.
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
            let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
            entry.size = end as u32;
            self.write_table_entry(index)?;
        }
        Ok(data.len())
    }

    /// Sustituye el contenido entero del fichero por `data` de forma atómica
    /// (journal de reemplazo): tras un corte el fichero tiene el contenido
    /// anterior o el nuevo, nunca una mezcla. Duradero al volver.
    pub fn replace(&mut self, index: usize, data: &[u8]) -> Result<(), FsError> {
        self.replace_chunk(index, 0, data)?;
        self.replace_commit(index, data.len() as u32)
    }

    /// Un trozo del contenido nuevo al área de datos del journal, sin
    /// compromiso: nada cambia hasta `replace_commit`. Varios trozos (en
    /// cualquier orden, dentro de la capacidad del journal y del fichero)
    /// forman un reemplazo mayor que una petición IPC (ADR 0018).
    pub fn replace_chunk(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<(), FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        let end = offset as usize + data.len();
        if end > e.capacity_bytes() as usize || end > JOURNAL_CAPACITY_BYTES {
            return Err(FsError::OutOfRange);
        }
        self.copy_to_device(JOURNAL_DATA_LBA, offset as usize, data)
    }

    /// Compromete los `len` primeros bytes que hay en el journal como nuevo
    /// contenido del fichero y los aplica (idempotente: `open` lo repite si
    /// hace falta).
    pub fn replace_commit(&mut self, index: usize, len: u32) -> Result<(), FsError> {
        let journal = self.commit_header(index, len)?;
        self.apply_journal(&journal)
    }

    /// Las dos primeras fases de `replace`: deja un reemplazo comprometido
    /// en el journal sin aplicarlo. Es lo que queda en disco tras un corte
    /// justo después del compromiso; xtask lo usa para fabricar discos de
    /// prueba de recuperación y el siguiente `open` lo rehace.
    pub fn stage_replace(&mut self, index: usize, data: &[u8]) -> Result<Journal, FsError> {
        self.replace_chunk(index, 0, data)?;
        self.commit_header(index, data.len() as u32)
    }

    /// Hace duraderos los datos del journal y escribe la cabecera pendiente
    /// con su CRC (leído del propio journal: es lo que se aplicará).
    fn commit_header(&mut self, index: usize, len: u32) -> Result<Journal, FsError> {
        let e = *self.entry(index).ok_or(FsError::NotFound)?;
        if len > e.capacity_bytes() || len as usize > JOURNAL_CAPACITY_BYTES {
            return Err(FsError::OutOfRange);
        }
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

    /// Copia los datos del journal al fichero, fija el tamaño y limpia la
    /// cabecera, con `flush` entre ambas fases.
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
        // La limpieza no necesita `flush`: si se pierde, el siguiente `open`
        // rehace un reemplazo ya aplicado (idempotente). Cada `flush` es un
        // fsync del soporte y domina el coste del reemplazo (etapa 5).
        self.dev.write_sector(JOURNAL_LBA, &Journal::CLEAR.encode())
    }

    /// Rehace un reemplazo comprometido (cabecera válida y pendiente, datos
    /// que cuadran con su CRC) o descarta uno que no llegó a comprometerse.
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

    /// CRC-32 de los `len` primeros bytes del área de datos del journal.
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

    /// Trunca a cero bytes.
    pub fn truncate(&mut self, index: usize) -> Result<(), FsError> {
        let entry = self.table[index].as_mut().ok_or(FsError::NotFound)?;
        entry.size = 0;
        self.write_table_entry(index)
    }

    /// Hace duraderas las escrituras anteriores (`write`, `create`, `truncate`).
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
        assert_eq!(fs.read(i, 17, &mut buf).unwrap(), 0, "fin de fichero");
        // Reabrir desde el dispositivo: tabla y superbloque persistidos.
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
        // Sobrescritura parcial en medio.
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
        assert_eq!(fs.replace(i, &too_big), Err(FsError::OutOfRange));
        assert_eq!(fs.read(99, 0, &mut [0u8; 4]), Err(FsError::NotFound));
        assert_eq!(fs.replace(99, b"x"), Err(FsError::NotFound));
        assert_eq!(fs.find("zzz"), None);
        // Disco lleno: sin espacio para mas ficheros de 64 sectores.
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
            "el journal no depende del espacio de datos"
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
        // Mas corto que el anterior: el tamaño baja.
        fs.replace(i, b"corto").unwrap();
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"corto");
        let mut fs = AriFs::open(fs.into_device()).unwrap();
        assert_eq!(fs.recovery(), Recovery::Clean);
        assert_eq!(fs.read(i, 0, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"corto");
        // Vacío también vale.
        fs.replace(i, b"").unwrap();
        assert_eq!(fs.entry(i).unwrap().size, 0);
    }

    /// Disco con caché volátil y corte programado: las escrituras quedan en
    /// caché hasta `flush`; desde la escritura `cut_at` todo devuelve `Io` y,
    /// al "apagar", la caché se pierde (`lose_cache`) o se conserva (el
    /// soporte alcanzó a escribirla). Ambos extremos acotan cualquier
    /// subconjunto perdido porque cada fase del reemplazo termina en `flush`.
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
        // Sin corte: cuenta las escrituras de un reemplazo completo.
        let mut fs = AriFs::open(CuttingDisk::new(&base, usize::MAX)).unwrap();
        fs.replace(i, &new).unwrap();
        let total = fs.into_device().writes;
        assert!(
            total > 8,
            "el reemplazo escribe datos, cabecera, fichero, tabla y limpieza"
        );
        let mut replayed = 0;
        for cut_at in 0..=total {
            for lose_cache in [true, false] {
                let mut fs = AriFs::open(CuttingDisk::new(&base, cut_at)).unwrap();
                let result = fs.replace(i, &new);
                assert_eq!(result.is_ok(), cut_at >= total, "corte en {cut_at}");
                let mut fs = AriFs::open(fs.into_device().power_off(lose_cache)).unwrap();
                let got = content(&mut fs, i);
                match fs.recovery() {
                    Recovery::Replayed { index } => {
                        assert_eq!(index, i);
                        assert_eq!(got, new, "corte en {cut_at}: rehecho");
                        replayed += 1;
                    }
                    Recovery::Discarded => assert_eq!(got, old, "corte en {cut_at}: descartado"),
                    Recovery::Clean => assert!(
                        got == old || got == new,
                        "corte en {cut_at} (cache perdida {lose_cache}): contenido mezclado"
                    ),
                }
                // Tras recuperar, el sistema sigue sirviendo.
                fs.replace(i, b"despues").unwrap();
                assert_eq!(content(&mut fs, i), b"despues");
                let mut fs = AriFs::open(fs.into_device()).unwrap();
                assert_eq!(fs.recovery(), Recovery::Clean);
                assert_eq!(content(&mut fs, i), b"despues");
            }
        }
        assert!(
            replayed > 0,
            "algun corte deja un reemplazo comprometido a medias"
        );
    }

    #[test]
    fn chunked_replace_is_atomic_too() {
        let old: Vec<u8> = (0..900u32).map(|x| (x % 11) as u8 + b'A').collect();
        let new: Vec<u8> = (0..9000u32).map(|x| (x % 19) as u8 + b'a').collect();
        let mut fs = AriFs::format(disk(300), 300).unwrap();
        let i = fs.create("pkg").unwrap();
        fs.write(i, 0, &old).unwrap();
        // Trozos de 4 KiB, como el cliente por IPC, y el ultimo parcial.
        for (k, chunk) in new.chunks(4096).enumerate() {
            fs.replace_chunk(i, (k * 4096) as u32, chunk).unwrap();
            assert_eq!(content(&mut fs, i), old, "nada cambia antes del compromiso");
        }
        fs.replace_commit(i, new.len() as u32).unwrap();
        assert_eq!(content(&mut fs, i), new);
        assert_eq!(
            fs.replace_chunk(i, JOURNAL_CAPACITY_BYTES as u32 - 1, b"xx"),
            Err(FsError::OutOfRange)
        );
        // Corte en cada escritura del reemplazo por trozos: viejo o nuevo.
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
            assert_eq!(result.is_ok(), cut_at >= total, "corte en {cut_at}");
            let mut fs = AriFs::open(fs.into_device().power_off(true)).unwrap();
            let got = content(&mut fs, i);
            assert!(
                got == old || got == new,
                "corte en {cut_at}: contenido mezclado"
            );
        }
    }

    /// Disco que cuenta peticiones: los lotes reducen las llamadas y el
    /// contenido no cambia respecto a ir sector a sector.
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
        // Desalineado por ambos extremos: parciales al principio y al final.
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
        assert_eq!(
            fs.dev.calls - before,
            1,
            "4 KiB alineados: una sola peticion"
        );
        let before = fs.dev.calls;
        fs.replace(i, &data[..4096]).unwrap();
        let used = fs.dev.calls - before;
        assert!(used <= 8, "reemplazo de 4 KiB en {used} peticiones");
        assert_eq!(content_of_counting(&mut fs, i), &data[..4096]);
    }

    fn content_of_counting(fs: &mut AriFs<CountingDisk>, i: usize) -> Vec<u8> {
        let mut buf = std::vec![0u8; 40_000];
        let n = fs.read(i, 0, &mut buf).unwrap();
        buf.truncate(n);
        buf
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
            "el journal no cubre mas de 64 sectores"
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
        assert_eq!(content_of(&mut fs, i), b"antes", "sin aplicar todavia");
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
        // Superbloque tocado.
        let mut img = base.clone();
        img[9] ^= 0x01;
        assert_eq!(AriFs::open(MemDisk(img)).err(), Some(FsError::Corrupt));
        // Entrada en uso tocada (un byte del nombre).
        let mut img = base.clone();
        img[TABLE_LBA as usize * SECTOR] ^= 0x20;
        assert_eq!(AriFs::open(MemDisk(img)).err(), Some(FsError::Corrupt));
        // Entrada libre con basura: sigue libre (no esta en uso).
        let mut img = base.clone();
        img[TABLE_LBA as usize * SECTOR + ENTRY_LEN + 3] = 0x55;
        assert!(AriFs::open(MemDisk(img)).is_ok());
        // Cabecera del journal rota (CRC): se descarta, el fichero no cambia.
        let mut img = base.clone();
        img[JOURNAL_LBA as usize * SECTOR + 8] = JOURNAL_PENDING as u8;
        let mut fs = AriFs::open(MemDisk(img)).unwrap();
        assert_eq!(fs.recovery(), Recovery::Discarded);
        assert_eq!(content(&mut fs, i), b"intacto");
        let fs = AriFs::open(fs.into_device()).unwrap();
        assert_eq!(fs.recovery(), Recovery::Clean, "la limpieza se escribio");
        // Cabecera pendiente valida pero datos que no cuadran con su CRC.
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
        // Cabecera pendiente valida con datos correctos: se rehace.
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
        assert_eq!(fs.sequence, 9, "la secuencia continua desde el journal");
    }
}
