// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The page attribute table (ADR 0078, stage 0): `aricode_x86_paging::pat`,
//! where entry 1 is write-combining, for the framebuffer.

use super::msr;

const IA32_PAT: u32 = 0x277;

/// Programs this CPU's table. The register is per CPU: the BSP and every
/// AP call this at startup, before anything is mapped with entry 1.
pub fn init() {
    // SAFETY: IA32_PAT exists on every x86-64 CPU; the new table only
    // changes an entry no page used.
    unsafe { msr::write(IA32_PAT, aricode_x86_paging::pat::VALUE) };
}
