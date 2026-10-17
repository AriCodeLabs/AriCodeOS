// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Permissions: in AriCodeOS a program can only do what its handles allow.
//! This module lets it inspect them: what kind of object is behind each one
//! and which ones it received at startup.

use crate::{self as sys, kind, Error, Handle};

/// First handle a process receives beyond the three initial ones.
pub const FIRST_EXTRA: u32 = 0x0001_0004;
/// Maximum number of extra handles walked by [`granted`].
pub const MAX_EXTRA: u32 = 16;

/// Human-readable name of a handle's object kind, or `None` if it does not exist.
pub fn kind_name(handle: Handle) -> Option<&'static str> {
    Some(match sys::object_kind(handle).ok()? {
        kind::THREAD => "thread",
        kind::PROCESS => "process",
        kind::CONSOLE => "console",
        kind::ENDPOINT => "endpoint",
        kind::SPAWNER => "spawner",
        kind::IRQ => "irq",
        kind::MMIO => "mmio",
        kind::DMA => "dma",
        kind::HARDWARE => "hardware",
        kind::PCI_ROOT => "pci root",
        kind::SYSTEM => "system",
        _ => "unknown",
    })
}

/// Walks the initial handles and the extra ones the creator handed over
/// (`0x10004..0x10004 + MAX_EXTRA`), calling `f(handle, name)` for each one
/// that exists. Gaps (handles already closed or transferred) are skipped:
/// giving up one permission does not hide the others. Only covers the
/// handles received at startup, not those created later.
pub fn granted(mut f: impl FnMut(Handle, &'static str)) {
    for h in [Handle::SELF_PROCESS, Handle::MAIN_THREAD, Handle::CONSOLE] {
        if let Some(name) = kind_name(h) {
            f(h, name);
        }
    }
    for i in 0..MAX_EXTRA {
        let h = Handle(FIRST_EXTRA + i);
        if let Some(name) = kind_name(h) {
            f(h, name);
        }
    }
}

/// `true` if the handle exists and is of the given kind (`kind::*`).
pub fn is(handle: Handle, expected: u64) -> bool {
    sys::object_kind(handle) == Ok(expected)
}

/// Closes a handle that is no longer needed: giving up a permission is the
/// cheapest way to shrink a program's attack surface.
pub fn drop_handle(handle: Handle) -> Result<(), Error> {
    sys::handle_close(handle)
}
