// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Forcewake and the GPU's frequency (ADR 0083). The 2012 PRM documents
//! neither; the sequences follow Linux i915 (`intel_uncore.c`,
//! `intel_rps.c`), and the laptop's probe read every register named here.

use crate::regs::{self, masked_off, masked_on};
use crate::{wait_for, Clock, Refusal, Regs, Timeouts};

/// Which register pair wakes the GT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `FORCEWAKE_MT`, when `ECOBUS` says the firmware set it up.
    MultiThreaded,
    /// `FORCEWAKE`, otherwise.
    Legacy,
}

impl Form {
    fn request(self) -> u32 {
        match self {
            Form::MultiThreaded => regs::FORCEWAKE_MT,
            Form::Legacy => regs::FORCEWAKE,
        }
    }

    fn ack(self) -> u32 {
        match self {
            Form::MultiThreaded => regs::FORCEWAKE_MT_ACK,
            Form::Legacy => regs::FORCEWAKE_ACK,
        }
    }
}

/// The way to keep the GT awake, once found.
#[derive(Debug, Clone, Copy)]
pub struct Forcewake {
    form: Form,
    timeouts: Timeouts,
}

impl Forcewake {
    /// Finds the form and drops any bits the firmware left held, so the GT
    /// may sleep when nobody holds it (Linux `intel_uncore_fw_domains_init`,
    /// the Ivy Bridge branch, and `fw_domain_reset`, `WaRsClearFWBitsAtReset`).
    pub fn init<R: Regs + ?Sized>(
        regs: &mut R,
        clock: &dyn Clock,
        timeouts: Timeouts,
    ) -> Result<Self, Refusal> {
        regs.write(regs::FORCEWAKE, 0);
        regs.read(regs::ECOBUS);
        let mt = Forcewake {
            form: Form::MultiThreaded,
            timeouts,
        };
        mt.reset(regs, clock)?;
        let ecobus = mt.hold(regs, clock)?.read(regs::ECOBUS);
        if ecobus & regs::ECOBUS_MT != 0 {
            return Ok(mt);
        }
        let legacy = Forcewake {
            form: Form::Legacy,
            timeouts,
        };
        legacy.reset(regs, clock)?;
        Ok(legacy)
    }

    pub fn form(&self) -> Form {
        self.form
    }

    fn reset<R: Regs + ?Sized>(&self, regs: &mut R, clock: &dyn Clock) -> Result<(), Refusal> {
        regs.write(self.form.request(), masked_off(0xFFFF));
        wait_for(
            regs,
            clock,
            self.form.ack(),
            self.timeouts.forcewake_ack,
            |v| v & regs::FORCEWAKE_KERNEL == 0,
        )
        .map(|_| ())
        .map_err(|_| Refusal::ForcewakeTimeout)
    }

    /// Wakes the GT and keeps it awake while the result lives (Linux
    /// `fw_domains_get_with_thread_status`).
    pub fn hold<'a, R: Regs + ?Sized>(
        &self,
        regs: &'a mut R,
        clock: &dyn Clock,
    ) -> Result<Awake<'a, R>, Refusal> {
        let (request, ack, t) = (self.form.request(), self.form.ack(), self.timeouts);
        let clear = |v: u32| v & regs::FORCEWAKE_KERNEL == 0;
        wait_for(regs, clock, ack, t.forcewake_ack, clear)
            .map_err(|_| Refusal::ForcewakeTimeout)?;
        regs.write(request, masked_on(regs::FORCEWAKE_KERNEL));
        // From here `Awake` releases the request whatever happens.
        let awake = Awake { regs, request };
        wait_for(awake.regs, clock, ack, t.forcewake_ack, |v| !clear(v))
            .map_err(|_| Refusal::ForcewakeTimeout)?;
        wait_for(
            awake.regs,
            clock,
            regs::GT_THREAD_STATUS,
            t.thread_status,
            |v| v & 7 == 0,
        )
        .map_err(|_| Refusal::ForcewakeTimeout)?;
        Ok(awake)
    }
}

/// The GT held awake: the only way to read or write registers below
/// 0x40000. Dropping it lets the GT sleep again.
pub struct Awake<'a, R: Regs + ?Sized> {
    regs: &'a mut R,
    request: u32,
}

impl<R: Regs + ?Sized> Awake<'_, R> {
    pub fn read(&self, offset: u32) -> u32 {
        self.regs.read(offset)
    }

    pub fn write(&mut self, offset: u32, value: u32) {
        debug_assert!(!regs::NEVER_TOUCH.contains(&offset));
        self.regs.write(offset, value);
    }

    /// Reads `offset` until `ok` holds, at most `limit_us`.
    pub fn wait(
        &self,
        clock: &dyn Clock,
        offset: u32,
        limit_us: u64,
        ok: impl Fn(u32) -> bool,
    ) -> Result<u32, u32> {
        wait_for(self.regs, clock, offset, limit_us, ok)
    }
}

impl<R: Regs + ?Sized> Drop for Awake<'_, R> {
    fn drop(&mut self) {
        self.regs
            .write(self.request, masked_off(regs::FORCEWAKE_KERNEL));
    }
}

/// The GPU's frequency limits, in steps of [`STEP_MHZ`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub highest: u8,
    pub efficient: u8,
    pub lowest: u8,
}

/// One frequency step (Linux `GT_FREQUENCY_MULTIPLIER`).
pub const STEP_MHZ: u32 = 50;
/// The request field holds 7 bits (bits 31:25 of `RPNSWREQ`).
const STEP_MAX: u8 = 0x7F;

impl Limits {
    /// From `RP_STATE_CAP`; `None` unless lowest ≤ efficient ≤ highest and
    /// the lowest is not zero.
    pub fn decode(cap: u32) -> Option<Self> {
        let l = Limits {
            highest: cap as u8,
            efficient: (cap >> 8) as u8,
            lowest: (cap >> 16) as u8,
        };
        (l.lowest != 0
            && l.lowest <= l.efficient
            && l.efficient <= l.highest
            && l.highest <= STEP_MAX)
            .then_some(l)
    }

    pub fn clamp(&self, step: u8) -> u8 {
        step.clamp(self.lowest, self.highest)
    }
}

/// Asks for `step` (clamped to `limits`), with RPS on as Linux has it and
/// its limits at the step. Returns the step asked for. The hardware decides
/// when it takes it: read [`frequency`] (Linux `gen6_rps_set`,
/// `rps_set_power`).
pub fn set_frequency<R: Regs + ?Sized>(awake: &mut Awake<'_, R>, limits: &Limits, step: u8) -> u8 {
    let step = limits.clamp(step);
    awake.write(regs::RP_CONTROL, regs::RP_CONTROL_ON);
    awake.write(
        regs::RP_INTERRUPT_LIMITS,
        (step as u32) << 24 | (limits.lowest as u32) << 16,
    );
    awake.write(regs::RPNSWREQ, (step as u32) << 25);
    step
}

/// Whether the GPU is to be held at its lowest clock for heat: from
/// `hot_at` degrees of the CPU package, which carries the GPU too, until
/// `margin` degrees below it, so the clock does not flap at the threshold.
pub fn too_hot(was: bool, degrees: u32, hot_at: u32, margin: u32) -> bool {
    if was {
        degrees + margin >= hot_at
    } else {
        degrees >= hot_at
    }
}

/// The step the GPU runs at now.
pub fn frequency<R: Regs + ?Sized>(awake: &Awake<'_, R>) -> u8 {
    ((awake.read(regs::RPSTAT1) >> 8) & 0x7F) as u8
}
