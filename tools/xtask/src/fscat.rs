//! `cargo xtask fs-cat DISCO [NOMBRE]`: lee un fichero (o lista los que
//! hay) de la partición ARI0 de una imagen de disco o un volcado del
//! pendrive, con la misma `aricode-arifs` que usa fsd. Es el canal de
//! diagnóstico sin fotos de `docs/hardware.md`: el shell guarda `diag NOM`
//! en AriFS y aquí se lee.

use crate::Result;
use aricode_arifs::{AriFs, BlockDevice, FsError};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Disco de solo lectura sobre un fichero, con el primer LBA de la partición.
struct FileDisk {
    file: File,
    base: u64,
}

impl BlockDevice for FileDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> std::result::Result<(), FsError> {
        self.file
            .seek(SeekFrom::Start((self.base + lba) * 512))
            .map_err(|_| FsError::Io)?;
        self.file.read_exact(buf).map_err(|_| FsError::Io)
    }
    fn write_sector(&mut self, _lba: u64, _buf: &[u8; 512]) -> std::result::Result<(), FsError> {
        Err(FsError::Io)
    }
    fn flush(&mut self) -> std::result::Result<(), FsError> {
        Ok(())
    }
}

/// Primer LBA de la partición `name` en la GPT del disco.
fn partition_lba(file: &mut File, name: &str) -> Result<u64> {
    let mut sector = [0u8; 512];
    file.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
    file.read_exact(&mut sector).map_err(|e| e.to_string())?;
    let header = aricode_gpt::Header::parse(&sector).map_err(|_| "sin cabecera GPT")?;
    let mut table = vec![0u8; header.entries_bytes().next_multiple_of(512)];
    file.seek(SeekFrom::Start(header.entries_lba * 512))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut table).map_err(|e| e.to_string())?;
    let mut parts = [None; 16];
    let n =
        aricode_gpt::partitions(&header, &table, &mut parts).map_err(|_| "tabla GPT invalida")?;
    parts[..n]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| p.first_lba)
        .ok_or_else(|| format!("sin particion {name}"))
}

/// Contenido de `name` en ARI0, o la lista de ficheros (`nombre  bytes`) si
/// `name` es `None`.
pub fn read(disk: &Path, name: Option<&str>) -> Result<Vec<u8>> {
    let mut file = File::open(disk).map_err(|e| format!("{}: {e}", disk.display()))?;
    let base = partition_lba(&mut file, "ARI0")?;
    let mut fs = AriFs::open(FileDisk { file, base })
        .map_err(|e| format!("AriFS en {}: {e:?}", disk.display()))?;
    let Some(name) = name else {
        let mut out = String::new();
        for (_, e) in fs.entries() {
            out.push_str(&format!("{:<20} {} bytes\n", e.name_str(), e.size));
        }
        return Ok(out.into_bytes());
    };
    let index = fs.find(name).ok_or_else(|| format!("{name}: no existe"))?;
    let size = fs.entry(index).map(|e| e.size).unwrap_or(0) as usize;
    let mut data = vec![0u8; size];
    let mut got = 0;
    while got < size {
        let n = fs
            .read(index, got as u32, &mut data[got..])
            .map_err(|e| format!("{name}: {e:?}"))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    data.truncate(got);
    Ok(data)
}

pub fn run(disk: &Path, name: Option<&str>) -> Result {
    let data = read(disk, name)?;
    std::io::stdout()
        .write_all(&data)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{write_test_disk, DiskVariant};

    #[test]
    fn reads_files_from_the_test_disk() {
        let dir = std::env::temp_dir().join(format!("aricode-fscat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let disk = dir.join("disk.img");
        write_test_disk(&disk, DiskVariant::Slots, &[]).unwrap();
        assert_eq!(
            read(&disk, Some("hola.txt")).unwrap(),
            b"Hola desde AriFS\n"
        );
        let listing = String::from_utf8(read(&disk, None).unwrap()).unwrap();
        assert!(listing.contains("hola.txt"));
        assert!(listing.contains("README"));
        assert!(read(&disk, Some("no-existe")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
