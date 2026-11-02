// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Either version behind one interface, for fsd and xtask: a disk stays
//! v1 until it is migrated (ADR 0072, decision 3), and until then it is
//! served by v1's own code.

use alloc::boxed::Box;

use crate::{v1, v2, BlockDevice, FsError, NAME_LEN};

/// A mounted volume of either version.
pub enum Volume<D: BlockDevice> {
    /// Boxed: v1 keeps its 32 entries inline, v2 keeps its table on the heap.
    V1(Box<v1::AriFs<D>>),
    V2(v2::AriFs<D>),
}

/// What `open` found, by version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mounted {
    V1(v1::Recovery),
    V2(v2::Recovery),
}

/// A file as both versions describe it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    name: [u8; NAME_LEN],
    name_len: u8,
    pub size: u32,
}

impl Info {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }
}

impl<D: BlockDevice> Volume<D> {
    /// Opens the volume in whichever version it is.
    pub fn open(mut dev: D) -> Result<Self, FsError> {
        if v2::AriFs::is_v2(&mut dev)? {
            v2::AriFs::open(dev).map(Self::V2)
        } else {
            v1::AriFs::open(dev).map(|fs| Self::V1(Box::new(fs)))
        }
    }

    /// 1 or 2.
    pub fn version(&self) -> u8 {
        match self {
            Self::V1(_) => 1,
            Self::V2(_) => 2,
        }
    }

    pub fn mounted(&self) -> Mounted {
        match self {
            Self::V1(fs) => Mounted::V1(fs.recovery()),
            Self::V2(fs) => Mounted::V2(fs.recovery()),
        }
    }

    /// Largest file an atomic replace can write.
    pub fn max_replace(&self) -> u32 {
        match self {
            Self::V1(_) => v1::JOURNAL_CAPACITY_BYTES as u32,
            Self::V2(_) => v2::MAX_FILE_BYTES,
        }
    }

    /// Slots of the table: indices go from 0 to this.
    pub fn slots(&self) -> usize {
        match self {
            Self::V1(_) => v1::MAX_FILES,
            Self::V2(_) => v2::MAX_FILES,
        }
    }

    pub fn info(&self, index: usize) -> Option<Info> {
        let (name, name_len, size) = match self {
            Self::V1(fs) => fs.entry(index).map(|e| (e.name, e.name_len, e.size))?,
            Self::V2(fs) => fs.entry(index).map(|e| (e.name, e.name_len, e.size))?,
        };
        Some(Info {
            name,
            name_len,
            size,
        })
    }

    pub fn entries(&self) -> impl Iterator<Item = (usize, Info)> + '_ {
        (0..self.slots()).filter_map(|i| self.info(i).map(|e| (i, e)))
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        match self {
            Self::V1(fs) => fs.find(name),
            Self::V2(fs) => fs.find(name),
        }
    }

    pub fn create(&mut self, name: &str) -> Result<usize, FsError> {
        match self {
            Self::V1(fs) => fs.create(name),
            Self::V2(fs) => fs.create(name),
        }
    }

    pub fn read(&mut self, index: usize, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        match self {
            Self::V1(fs) => fs.read(index, offset, buf),
            Self::V2(fs) => fs.read(index, offset, buf),
        }
    }

    pub fn write(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        match self {
            Self::V1(fs) => fs.write(index, offset, data),
            Self::V2(fs) => fs.write(index, offset, data),
        }
    }

    pub fn replace(&mut self, index: usize, data: &[u8]) -> Result<(), FsError> {
        match self {
            Self::V1(fs) => fs.replace(index, data),
            Self::V2(fs) => fs.replace(index, data),
        }
    }

    pub fn replace_chunk(&mut self, index: usize, offset: u32, data: &[u8]) -> Result<(), FsError> {
        match self {
            Self::V1(fs) => fs.replace_chunk(index, offset, data),
            Self::V2(fs) => fs.replace_chunk(index, offset, data),
        }
    }

    pub fn replace_commit(&mut self, index: usize, len: u32) -> Result<(), FsError> {
        match self {
            Self::V1(fs) => fs.replace_commit(index, len),
            Self::V2(fs) => fs.replace_commit(index, len),
        }
    }

    pub fn abandon_replace(&mut self) {
        match self {
            Self::V1(fs) => fs.abandon_replace(),
            Self::V2(fs) => fs.abandon_replace(),
        }
    }

    pub fn truncate(&mut self, index: usize) -> Result<(), FsError> {
        match self {
            Self::V1(fs) => fs.truncate(index),
            Self::V2(fs) => fs.truncate(index),
        }
    }

    /// v2 only.
    pub fn delete(&mut self, index: usize) -> Result<(), FsError> {
        match self {
            Self::V1(_) => Err(FsError::Unsupported),
            Self::V2(fs) => fs.delete(index),
        }
    }

    /// v2 only; see [`v2::AriFs::rename`].
    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), FsError> {
        match self {
            Self::V1(_) => Err(FsError::Unsupported),
            Self::V2(fs) => fs.rename(index, name),
        }
    }

    pub fn flush(&mut self) -> Result<(), FsError> {
        match self {
            Self::V1(fs) => fs.flush(),
            Self::V2(fs) => fs.flush(),
        }
    }

    pub fn needs_reopen(&self) -> bool {
        match self {
            Self::V1(fs) => fs.needs_reopen(),
            // v2 resyncs the table itself before the next change.
            Self::V2(_) => false,
        }
    }

    /// Migrates a v1 volume to v2 ([`v2::AriFs::migrate`]); a v2 one is
    /// returned as it is. On an error the device is gone with it: the
    /// caller mounts again, and finds v1 or v2, never a mix.
    pub fn migrate(self) -> Result<Self, FsError> {
        match self {
            Self::V1(fs) => v2::AriFs::migrate((*fs).into_device()).map(Self::V2),
            v2 => Ok(v2),
        }
    }

    pub fn into_device(self) -> D {
        match self {
            Self::V1(fs) => (*fs).into_device(),
            Self::V2(fs) => fs.into_device(),
        }
    }
}
