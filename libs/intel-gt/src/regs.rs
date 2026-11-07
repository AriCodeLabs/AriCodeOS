// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Register offsets in BAR 0, with where each comes from (ADR 0083).
//!
//! "PRM" is Intel's Programmer's Reference Manual for Ivy Bridge, 2012, by
//! volume and part. The 2012 manual says nothing about forcewake, RC6 or the
//! GPU's frequency; those come from Linux i915 (MIT), and each was read back
//! on the reference laptop by the probe of ADR 0083.

/// The GTT, in BAR 0 (PRM Vol 3 Part 2, 1.10: BAR 0 holds registers, then the
/// GTT from 2 MiB).
pub const GTT: u32 = 0x20_0000;
/// The register half of BAR 0; offsets at or above it are the GTT.
pub const REGS_END: u32 = GTT;

// Always readable: offsets at or above 0x40000 need no forcewake on this
// family (Linux `intel_uncore.c`, `__gen6_fw_ranges`).

/// Forcewake request, multi-threaded form (masked). Linux `FORCEWAKE_MT`.
pub const FORCEWAKE_MT: u32 = 0xA188;
/// Forcewake request, legacy form. Linux `FORCEWAKE`.
pub const FORCEWAKE: u32 = 0xA18C;
/// Bit 5: the multi-threaded form is available. Linux `ECOBUS`,
/// `FORCEWAKE_MT_ENABLE`. Needs the GT awake to read true.
pub const ECOBUS: u32 = 0xA180;
pub const ECOBUS_MT: u32 = 1 << 5;
/// Acks of the two forms. Linux `FORCEWAKE_MT_ACK`, `FORCEWAKE_ACK`.
pub const FORCEWAKE_MT_ACK: u32 = 0x13_0040;
pub const FORCEWAKE_ACK: u32 = 0x13_0090;
/// Bit 0 of a forcewake request: the kernel's own (Linux `FORCEWAKE_KERNEL`).
pub const FORCEWAKE_KERNEL: u32 = 1 << 0;
/// Bits 2:0: threads still running after a wake (Linux
/// `GEN6_GT_THREAD_STATUS_REG`, used by `WaRsForcewakeWaitTC0`).
pub const GT_THREAD_STATUS: u32 = 0x13_805C;
/// Bits 2:0: the GT's RC state, 0 awake (Linux `GEN6_GT_CORE_STATUS`).
pub const GT_CORE_STATUS: u32 = 0x13_8060;
/// GPU frequency limits from the memory controller's mirror: bits 7:0 RP0
/// (highest), 15:8 RP1 (efficient), 23:16 RPn (lowest), in 50 MHz steps
/// (Linux `GEN6_RP_STATE_CAP`, `GT_FREQUENCY_MULTIPLIER`).
pub const RP_STATE_CAP: u32 = 0x14_5998;
/// The interrupt registers of the GT (PRM Vol 1 Part 4, 2.1.10).
pub const GTIMR: u32 = 0x4_4014;
pub const GTIIR: u32 = 0x4_4018;
pub const GTIER: u32 = 0x4_401C;
/// Writing 1 flushes the GPU's view of the GTT after entries change (Linux
/// `GFX_FLSH_CNTL_GEN6`, `gen6_ggtt_invalidate`).
pub const GFX_FLSH_CNTL: u32 = 0x10_1008;

// Below 0x40000: only with forcewake held (`Awake`).

/// Requested frequency, bits 31:25 (Linux `GEN6_RPNSWREQ`, `gen6_rps_set`).
pub const RPNSWREQ: u32 = 0xA008;
/// Bits 31:24 the highest and 23:16 the lowest frequency RPS may ask for
/// (Linux `GEN6_RP_INTERRUPT_LIMITS`).
pub const RP_INTERRUPT_LIMITS: u32 = 0xA014;
/// The frequency in use, bits 14:8 (Linux `GEN6_RPSTAT1`, `GEN6_CAGF`).
pub const RPSTAT1: u32 = 0xA01C;
/// RPS control; 0 is off (Linux `GEN6_RP_CONTROL`).
pub const RP_CONTROL: u32 = 0xA024;
/// RC6 control (Linux `GEN6_RC_CONTROL`).
pub const RC_CONTROL: u32 = 0xA090;
/// Bit 2 must stay clear on this family: masked, a looping batch can hang
/// the GPU (Linux `intel_rps_init`, `GEN6_PMINTRMSK`).
pub const PMINTRMSK: u32 = 0xA168;
/// Engine resets: bit 0 all, 3 the blitter; each clears itself when done
/// (PRM Vol 1 Part 6, 1.4.8).
pub const GDRST: u32 = 0x941C;
pub const GDRST_FULL: u32 = 1 << 0;
pub const GDRST_BLITTER: u32 = 1 << 3;
/// The PPGTT's cache bits (Linux `gen7_ppgtt_enable`).
pub const GAM_ECOCHK: u32 = 0x4090;
pub const ECOCHK_PPGTT_LLC_IVB: u32 = 1 << 3;
pub const ECOCHK_PPGTT_GFDT_IVB: u32 = 1 << 4;
pub const GAC_ECO_BITS: u32 = 0x1_4090;
pub const ECOBITS_PPGTT_CACHE64B: u32 = 3 << 8;
/// The render engine's mode register, which on this family also sets how
/// the other engines' TLBs are invalidated (Linux `rcs_engine_wa_init`,
/// `WaBCSVCSTlbInvalidationMode`; PRM Vol 1 Part 4, 2.2.5).
pub const RCS_GFX_MODE: u32 = 0x229C;
pub const GFX_TLB_INVALIDATE_EXPLICIT: u32 = 1 << 13;
pub const GFX_REPLAY_MODE: u32 = 1 << 11;
/// Accesses here can keep the GT out of RC6 (PRM Vol 1 Part 6, 1.1).
pub const NEVER_TOUCH: core::ops::Range<u32> = 0x9500..0x9800;

/// The blitter's registers (PRM Vol 1 Part 4, 2.1).
pub mod bcs {
    pub const BASE: u32 = 0x2_2000;
    pub const TAIL: u32 = BASE + 0x30;
    pub const HEAD: u32 = BASE + 0x34;
    pub const START: u32 = BASE + 0x38;
    pub const CTL: u32 = BASE + 0x3C;
    pub const ACTHD: u32 = BASE + 0x74;
    pub const HWSTAM: u32 = BASE + 0x98;
    pub const MI_MODE: u32 = BASE + 0x9C;
    pub const IMR: u32 = BASE + 0xA8;
    pub const EIR: u32 = BASE + 0xB0;
    pub const ESR: u32 = BASE + 0xB8;
    pub const INSTPM: u32 = BASE + 0xC0;
    pub const PP_DIR_DCLV: u32 = BASE + 0x220;
    pub const PP_DIR_BASE: u32 = BASE + 0x228;
    /// `BLT_MODE`; bit 9 turns the PPGTT on (PRM Vol 1 Part 4, 2.1.7.3).
    pub const MODE: u32 = BASE + 0x29C;
    /// The status page's GTT address; not at `BASE + 0x80` on this family
    /// (PRM Vol 1 Part 4, 2.1.6.1).
    pub const HWS_PGA: u32 = 0x4280;
    /// Page fault status (Linux `_RING_FAULT_REG_BCS`).
    pub const FAULT: u32 = 0x4294;

    /// `CTL`: bit 0 on; bits 20:12 the ring's pages less one.
    pub const CTL_VALID: u32 = 1 << 0;
    pub const CTL_PAGES: u32 = 0x001F_F000;
    /// `HEAD` bits 20:2 and `TAIL` bits 20:3 are offsets in the ring.
    pub const HEAD_ADDR: u32 = 0x001F_FFFC;
    pub const TAIL_ADDR: u32 = 0x001F_FFF8;
    /// `MI_MODE`: 8 stops the ring, 9 reads 1 when it is idle.
    pub const MODE_STOP: u32 = 1 << 8;
    pub const MODE_IDLE: u32 = 1 << 9;
    /// `MODE` bit 9.
    pub const PPGTT_ENABLE: u32 = 1 << 9;
    /// `INSTPM`: 9 invalidates the TLB, only together with 5, the
    /// synchronizing flush (PRM Vol 1 Part 4, 2.1.7.4).
    pub const INSTPM_TLB_INVALIDATE: u32 = 1 << 9;
    pub const INSTPM_SYNC_FLUSH: u32 = 1 << 5;
    /// `PP_DIR_DCLV`: all 2 GiB of the PPGTT (Linux `PP_DIR_DCLV_2G`).
    pub const DCLV_2G: u32 = 0xFFFF_FFFF;
}

/// The value that sets `bits` in a masked register (upper half selects).
pub const fn masked_on(bits: u32) -> u32 {
    (bits << 16) | bits
}

/// The value that clears `bits` in a masked register.
pub const fn masked_off(bits: u32) -> u32 {
    bits << 16
}
