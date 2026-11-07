// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The GT of Intel Ivy Bridge graphics as the kernel owns it (ADR 0083): the
//! engines, the global and per-process GTT, forcewake and the GPU's frequency.
//!
//! On this family a batch's privilege is its address space, so whoever
//! writes the ring, the GTT or the engine's registers can reach all of
//! memory. The kernel keeps them; programs give batches and memory only.
//!
//! Written from Intel's PRM for Ivy Bridge (2012) and, where it is silent
//! (forcewake, RC6, frequency), from Linux i915 (MIT), each value checked by
//! a read-only probe of the reference laptop. [`inspect`] reads only, and
//! refuses any state the probe did not see: such a machine keeps what its
//! firmware set.

#![no_std]

pub mod check;
pub mod cmd;
pub mod gtt;
pub mod power;
pub mod ppgtt;
pub mod regs;
pub mod ring;

/// Access to BAR 0 of the graphics device: registers, then from
/// [`regs::GTT`] the global GTT.
pub trait Regs {
    fn read(&self, offset: u32) -> u32;
    fn write(&mut self, offset: u32, value: u32);
}

/// Time, for the limits every wait has.
pub trait Clock {
    fn now_us(&self) -> u64;
}

/// Device IDs of the family this is written and tested for: Ivy Bridge
/// (Linux `i915_pciids.h`, `INTEL_IVB_*_IDS`). Only `0166` has been on real
/// hardware so far.
pub const SUPPORTED: [u16; 6] = [0x0156, 0x0166, 0x0152, 0x015A, 0x0162, 0x016A];

/// Why the GT is left as the firmware set it. Numbered for `diag`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not a device of a family in [`SUPPORTED`].
    NotThisFamily = 1,
    /// BAR 0 is not the size this family has.
    BadBars = 2,
    /// Registers read all ones: the device does not answer.
    NoAnswer = 3,
    /// The firmware or a previous system left an engine's ring running.
    RingRunning = 4,
    /// The PPGTT is on for an engine.
    PpgttOn = 5,
    /// A reset is in progress.
    ResetPending = 6,
    /// Forcewake was not acknowledged in time.
    ForcewakeTimeout = 7,
    /// An engine is not idle.
    NotIdle = 8,
    /// An engine reports a fault or an error.
    Faulted = 9,
    /// The GTT's size is not one this family has.
    GttSize = 10,
    /// The frequency limits read are not sane.
    FrequencyLimits = 11,
    /// A wait on the engine ran out of time.
    Timeout = 12,
    /// No memory for the engine's own pages.
    NoMemory = 13,
}

/// How long each wait may take, in microseconds. The defaults are Linux's
/// (`intel_uncore.c`, `intel_ring_submission.c`, `intel_reset.c`).
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// A forcewake acknowledge (`FORCEWAKE_ACK_TIMEOUT_MS`).
    pub forcewake_ack: u64,
    /// Threads settling after a wake (`WaRsForcewakeWaitTC0`).
    pub thread_status: u64,
    /// A register reading back what was written (ring head).
    pub readback: u64,
    /// An engine stopping.
    pub idle: u64,
    /// A reset bit clearing.
    pub reset: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            forcewake_ack: 50_000,
            thread_status: 5_000,
            readback: 50_000,
            idle: 100_000,
            reset: 2_000,
        }
    }
}

/// Reads `offset` until `ok` holds or `limit_us` passes; the last value
/// either way.
pub(crate) fn wait_for<R: Regs + ?Sized>(
    regs: &R,
    clock: &dyn Clock,
    offset: u32,
    limit_us: u64,
    ok: impl Fn(u32) -> bool,
) -> Result<u32, u32> {
    let start = clock.now_us();
    loop {
        let v = regs.read(offset);
        if ok(v) {
            return Ok(v);
        }
        if clock.now_us().saturating_sub(start) > limit_us {
            return Err(v);
        }
        core::hint::spin_loop();
    }
}

/// What [`inspect`] found, reading only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// Entries in the global GTT.
    pub gtt_entries: u32,
    /// The GT's RC state (0 awake).
    pub rc_state: u32,
    /// Frequency limits, in 50 MHz steps.
    pub limits: power::Limits,
}

/// Reads the device's always-readable registers and decides whether the GT
/// may be taken over. `mggc0`: the device's PCI register 0x50 (PRM Vol 3
/// Part 2, MGGC0). Writes nothing.
pub fn inspect<R: Regs + ?Sized>(
    regs: &R,
    device: u16,
    bar0_size: u64,
    mggc0: u32,
) -> Result<Found, Refusal> {
    if !SUPPORTED.contains(&device) {
        return Err(Refusal::NotThisFamily);
    }
    if bar0_size != 4 << 20 {
        return Err(Refusal::BadBars);
    }
    let core = regs.read(regs::GT_CORE_STATUS);
    let ack = regs.read(regs::FORCEWAKE_MT_ACK);
    if core == u32::MAX || ack == u32::MAX {
        return Err(Refusal::NoAnswer);
    }
    let gtt_entries = gtt::entries(mggc0).ok_or(Refusal::GttSize)?;
    let limits =
        power::Limits::decode(regs.read(regs::RP_STATE_CAP)).ok_or(Refusal::FrequencyLimits)?;
    Ok(Found {
        gtt_entries,
        rc_state: core & 7,
        limits,
    })
}

/// Checks, with the GT awake, that the blitter is as the probe found it:
/// off, idle, without the PPGTT, faults or a reset under way.
pub fn check_blitter<R: Regs + ?Sized>(awake: &power::Awake<'_, R>) -> Result<(), Refusal> {
    use regs::bcs;
    if awake.read(regs::GDRST) != 0 {
        return Err(Refusal::ResetPending);
    }
    if awake.read(bcs::CTL) & bcs::CTL_VALID != 0 {
        return Err(Refusal::RingRunning);
    }
    if awake.read(bcs::MODE) & bcs::PPGTT_ENABLE != 0 {
        return Err(Refusal::PpgttOn);
    }
    if awake.read(bcs::MI_MODE) & bcs::MODE_IDLE == 0 {
        return Err(Refusal::NotIdle);
    }
    if awake.read(bcs::FAULT) & 1 != 0 || awake.read(bcs::EIR) != 0 {
        return Err(Refusal::Faulted);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
