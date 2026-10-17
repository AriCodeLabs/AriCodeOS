// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask fs-cat DISK [NAME]`: reads a file (or lists the files) from
//! the ARI0 partition of a disk image or a USB stick dump, using the same
//! `aricode-arifs` that fsd uses. It is the photo-free diagnostics channel
//! of `docs/hardware.md`: the shell saves `diag NAME` to AriFS and it is
//! read here.

use crate::Result;
use aricode_arifs::{AriFs, BlockDevice, FsError};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Read-only disk backed by a file, with the partition's first LBA.
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

/// First LBA of partition `name` in the disk's GPT.
fn partition_lba(file: &mut File, name: &str) -> Result<u64> {
    let mut sector = [0u8; 512];
    file.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
    file.read_exact(&mut sector).map_err(|e| e.to_string())?;
    let header = aricode_gpt::Header::parse(&sector).map_err(|_| "no GPT header")?;
    let mut table = vec![0u8; header.entries_bytes().next_multiple_of(512)];
    file.seek(SeekFrom::Start(header.entries_lba * 512))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut table).map_err(|e| e.to_string())?;
    let mut parts = [None; 16];
    let n =
        aricode_gpt::partitions(&header, &table, &mut parts).map_err(|_| "invalid GPT table")?;
    parts[..n]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| p.first_lba)
        .ok_or_else(|| format!("no partition {name}"))
}

/// Contents of `name` in ARI0, or the file listing (`name  bytes`) if
/// `name` is `None`.
pub fn read(disk: &Path, name: Option<&str>) -> Result<Vec<u8>> {
    let mut file = File::open(disk).map_err(|e| format!("{}: {e}", disk.display()))?;
    let base = partition_lba(&mut file, "ARI0")?;
    let mut fs = AriFs::open(FileDisk { file, base })
        .map_err(|e| format!("AriFS on {}: {e:?}", disk.display()))?;
    let Some(name) = name else {
        let mut out = String::new();
        for (_, e) in fs.entries() {
            out.push_str(&format!("{:<20} {} bytes\n", e.name_str(), e.size));
        }
        return Ok(out.into_bytes());
    };
    let index = fs.find(name).ok_or_else(|| format!("{name}: not found"))?;
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
        assert!(read(&disk, Some("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
