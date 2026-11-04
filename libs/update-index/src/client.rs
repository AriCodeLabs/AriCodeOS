// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The decisions of this crate over fsd's client (feature `client`): the
//! one adapter of `FsClient` to [`Files`] the shell, init, the installer
//! and the updater share, and the writes they make the same way.

use crate::files::{Files, NAME_LEN};
use aricode_proto::fs::Status;
use aricode_sys::fs::{FsClient, FsError};

/// A file space of fsd as [`Files`]: only an explicit `NotFound` from the
/// service means a file is absent; a lost reply, a timeout or any other
/// status is an error the callers refuse to act on (incident 0008).
pub struct FsFiles<'a>(pub &'a mut FsClient);

impl Files for FsFiles<'_> {
    type Fd = u32;
    type Error = FsError;

    fn open(&mut self, name: &str) -> Result<Option<u32>, FsError> {
        match self.0.open(name) {
            Ok(fd) => Ok(Some(fd)),
            Err(FsError::Status(Status::NotFound)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read(&mut self, fd: &u32, offset: u32, out: &mut [u8]) -> Result<usize, FsError> {
        self.0.read(*fd, offset, out)
    }

    fn close(&mut self, fd: u32) {
        let _ = self.0.close(fd);
    }

    fn list(
        &mut self,
        index: u32,
        name_out: &mut [u8; NAME_LEN],
    ) -> Result<Option<(usize, u64)>, FsError> {
        self.0.list(index, name_out)
    }
}

/// Replaces the whole of `name` with `data` atomically, making it if it
/// does not exist.
pub fn save(fs: &mut FsClient, name: &str, data: &[u8]) -> Result<usize, FsError> {
    let fd = fs.open(name).or_else(|_| fs.create(name))?;
    let r = fs.replace(fd, data);
    let _ = fs.close(fd);
    r
}

/// Why a line did not reach the install history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    /// The history could not be read: it is not rewritten (incident 0008).
    Read,
    Write,
}

/// Appends `line` to the install history (`updater/history`).
pub fn log_history(fs: &mut FsClient, line: &str) -> Result<(), HistoryError> {
    let path = crate::main_path(crate::HISTORY_FILE);
    let log = crate::history::appended(&mut FsFiles(fs), &path, line, crate::HISTORY_MAX)
        .map_err(|_| HistoryError::Read)?;
    save(fs, &path, &log)
        .map(|_| ())
        .map_err(|_| HistoryError::Write)
}
