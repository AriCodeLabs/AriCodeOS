// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! PCI configuration helpers for drivers.

use crate::{self as sys, Error, Handle};

/// Command register offset and its Bus Master Enable bit (PCI Local Bus
/// 6.2.2).
const COMMAND: u64 = 0x04;
const BUS_MASTER: u16 = 1 << 2;

/// Lets the claimed function whose configuration space is `cfg` (the
/// claim's configuration handle) read and write memory. The kernel claims a
/// function with bus mastering off (ADR 0038): a device a dead driver left
/// running must not reach memory reused since. Call it once the device is
/// reset, before handing it any DMA address.
pub fn enable_bus_master(cfg: Handle) -> Result<(), Error> {
    let virt = sys::mmio_map(cfg)?;
    let command = (virt + COMMAND) as *mut u16;
    // SAFETY: the function's configuration page, mapped uncached by
    // `mmio_map`; a 16-bit access leaves the status register (write one to
    // clear) alone.
    unsafe { command.write_volatile(command.read_volatile() | BUS_MASTER) };
    Ok(())
}
