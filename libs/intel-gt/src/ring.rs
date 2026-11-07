// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The blitter's ring, which only the kernel writes (ADR 0083). Each request
//! starts one client batch, non-secure in the PPGTT, then stores its number
//! in the status page. The bring-up, the stop and the reset follow Linux's
//! `xcs_resume`, `intel_engine_stop_cs` and `gen6_hw_domain_reset`, with the
//! limits of PRM Vol 1 Part 4, 2.1.

use crate::cmd;
use crate::gtt::{Layout, PAGE};
use crate::power::Awake;
use crate::regs::{self, bcs, masked_off, masked_on};
use crate::{Clock, Refusal, Regs, Timeouts};

/// The dword of the status page that receives each request's number
/// (Linux `I915_GEM_HWS_SEQNO`; dwords 0x10 to 0x3FF are software's, PRM
/// Vol 1 Part 4, 2.1.6.1).
pub const SEQNO_INDEX: u32 = 0x40;
const SEQNO_ADDR: u32 = SEQNO_INDEX * 4;
/// A dword the TLB invalidation before each batch stores to: the
/// invalidation works only with a store (PRM Vol 1 Part 4, 2.2.5; Linux
/// `I915_GEM_HWS_SCRATCH`, `mi_flush_dw`).
const SCRATCH_ADDR: u32 = 0x80 * 4;
/// Stores repeated after the flush. Linux does this on gen7's other engines
/// (`GEN7_XCS_WA`) and gives no reason; we keep it until the laptop shows it
/// is not needed.
const STORE_REPEATS: usize = 32;
/// Bytes left between the tail and the head so a full ring never looks
/// empty (Linux `__intel_ring_space`, a cache line).
const GAP: u32 = 64;
/// Dwords of one request: a TLB invalidation (4), so the batch sees the
/// PPGTT as it is now; the start (2); flush and store (3); the repeated
/// stores; a flush (3); the interrupt and a no-op.
pub const REQUEST_DWORDS: usize = 4 + 2 + 3 + 3 * STORE_REPEATS + 3 + 2;

/// Whether a request numbered `seqno` has finished, given the number last
/// stored. Numbers wrap.
pub fn passed(stored: u32, seqno: u32) -> bool {
    stored.wrapping_sub(seqno) as i32 >= 0
}

/// The kernel's side of the ring: its size and where the next command goes.
/// The ring's memory is given to each call: the kernel maps it uncached,
/// as the PRM asks (Vol 1 Part 4, 2.1.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ring {
    bytes: u32,
    tail: u32,
}

impl Ring {
    pub fn new(pages: u32) -> Self {
        Ring {
            bytes: pages * PAGE as u32,
            tail: 0,
        }
    }

    pub fn tail(&self) -> u32 {
        self.tail
    }

    /// Bytes free, given the engine's head.
    pub fn space(&self, head: u32) -> u32 {
        (head & bcs::HEAD_ADDR)
            .wrapping_sub(self.tail)
            .wrapping_sub(GAP)
            & (self.bytes - 1)
    }

    /// Writes the commands that run the batch at `batch` (a PPGTT address)
    /// and then store `seqno`; returns the new tail for `TAIL`, or `None` if
    /// the ring has no room for it yet. A request that would cross the ring's
    /// end starts again at 0, the rest filled with no-ops.
    pub fn request(&mut self, mem: &mut [u32], head: u32, batch: u32, seqno: u32) -> Option<u32> {
        let mut words = [cmd::NOOP; REQUEST_DWORDS];
        let mut n = 0;
        let mut put = |w: u32| {
            words[n] = w;
            n += 1;
        };
        let flush = cmd::FLUSH_DW | cmd::FLUSH_DW_STORE_DWORD | cmd::FLUSH_DW_STORE_INDEX;
        put(flush | cmd::FLUSH_DW_INVALIDATE_TLB);
        put(SCRATCH_ADDR | cmd::FLUSH_DW_GLOBAL_GTT);
        put(0);
        put(cmd::NOOP);
        put(cmd::BATCH_START | cmd::BATCH_PPGTT);
        put(batch);
        put(cmd::FLUSH_DW
            | cmd::FLUSH_DW_INVALIDATE_TLB
            | cmd::FLUSH_DW_STORE_DWORD
            | cmd::FLUSH_DW_STORE_INDEX);
        put(SEQNO_ADDR | cmd::FLUSH_DW_GLOBAL_GTT);
        put(seqno);
        for _ in 0..STORE_REPEATS {
            put(cmd::STORE_DWORD_INDEX);
            put(SEQNO_ADDR);
            put(seqno);
        }
        put(cmd::FLUSH_DW);
        put(0);
        put(0);
        put(cmd::USER_INTERRUPT);
        put(cmd::NOOP);
        debug_assert_eq!(n, REQUEST_DWORDS);
        let need = (REQUEST_DWORDS * 4) as u32;
        let to_end = self.bytes - self.tail;
        let wrap = if need > to_end { to_end } else { 0 };
        if self.space(head) < need + wrap || mem.len() * 4 < self.bytes as usize {
            return None;
        }
        let mut at = (self.tail / 4) as usize;
        if wrap != 0 {
            mem[at..(self.bytes / 4) as usize].fill(cmd::NOOP);
            at = 0;
        }
        mem[at..at + REQUEST_DWORDS].copy_from_slice(&words);
        self.tail = ((at + REQUEST_DWORDS) * 4) as u32 % self.bytes;
        Some(self.tail)
    }

    /// After a reset: the ring starts empty again.
    pub fn rewind(&mut self) {
        self.tail = 0;
    }
}

/// Settings made once for the whole GT before the blitter starts: the TLB
/// invalidation mode of the other engines, which on this family sits in the
/// render engine's register (`WaBCSVCSTlbInvalidationMode`), and the PPGTT's
/// cache bits (Linux `gen7_ppgtt_enable`).
pub fn prepare<R: Regs + ?Sized>(awake: &mut Awake<'_, R>) {
    awake.write(
        regs::RCS_GFX_MODE,
        masked_on(regs::GFX_TLB_INVALIDATE_EXPLICIT | regs::GFX_REPLAY_MODE),
    );
    let eco = awake.read(regs::GAC_ECO_BITS);
    awake.write(regs::GAC_ECO_BITS, eco | regs::ECOBITS_PPGTT_CACHE64B);
    let chk = awake.read(regs::GAM_ECOCHK);
    awake.write(
        regs::GAM_ECOCHK,
        (chk | regs::ECOCHK_PPGTT_LLC_IVB) & !regs::ECOCHK_PPGTT_GFDT_IVB,
    );
}

/// Empties and turns off the ring (Linux `stop_ring`); true if the head
/// went back to 0.
fn stop_ring<R: Regs + ?Sized>(awake: &mut Awake<'_, R>) -> bool {
    let tail = awake.read(bcs::TAIL);
    awake.write(bcs::HEAD, tail);
    awake.read(bcs::HEAD);
    awake.write(bcs::CTL, 0);
    awake.read(bcs::CTL);
    awake.write(bcs::HEAD, 0);
    awake.write(bcs::TAIL, 0);
    awake.read(bcs::HEAD) & bcs::HEAD_ADDR == 0
}

/// Starts the blitter on an empty ring with the PPGTT on (Linux
/// `xcs_resume`). The status page and the ring must be mapped at `layout`'s
/// entries, and the page directory written, before this.
pub fn start<R: Regs + ?Sized>(
    awake: &mut Awake<'_, R>,
    clock: &dyn Clock,
    t: &Timeouts,
    layout: &Layout,
    ring: &mut Ring,
) -> Result<(), Refusal> {
    if !stop_ring(awake) {
        return Err(Refusal::RingRunning);
    }
    ring.rewind();
    awake.write(bcs::HWS_PGA, layout.status * PAGE as u32);
    awake.read(bcs::HWS_PGA);
    awake.write(bcs::HWSTAM, !0);
    // A synchronizing flush with a TLB invalidation; the ring must be idle.
    if awake.read(bcs::MI_MODE) & bcs::MODE_IDLE == 0 {
        return Err(Refusal::NotIdle);
    }
    awake.write(
        bcs::INSTPM,
        masked_on(bcs::INSTPM_TLB_INVALIDATE | bcs::INSTPM_SYNC_FLUSH),
    );
    awake
        .wait(clock, bcs::INSTPM, t.reset, |v| {
            v & bcs::INSTPM_SYNC_FLUSH == 0
        })
        .map_err(|_| Refusal::Timeout)?;
    awake.read(bcs::HEAD);
    awake.write(bcs::START, layout.ring * PAGE as u32);
    awake.write(bcs::PP_DIR_DCLV, bcs::DCLV_2G);
    awake.write(bcs::PP_DIR_BASE, layout.pp_dir_base());
    awake.write(bcs::MODE, masked_on(bcs::PPGTT_ENABLE));
    // The head may not take at the first write (Linux: "experimented ...
    // 50ms works best").
    let start = clock.now_us();
    loop {
        awake.write(bcs::HEAD, 0);
        if awake.read(bcs::HEAD) == 0 {
            break;
        }
        if clock.now_us().saturating_sub(start) > t.readback {
            return Err(Refusal::Timeout);
        }
    }
    awake.write(bcs::TAIL, 0);
    if awake.read(bcs::HEAD) != awake.read(bcs::TAIL) {
        return Err(Refusal::Timeout);
    }
    awake.write(bcs::CTL, (layout.ring_pages - 1) << 12 | bcs::CTL_VALID);
    awake
        .wait(clock, bcs::CTL, 5_000, |v| v & bcs::CTL_VALID != 0)
        .map_err(|_| Refusal::Timeout)?;
    awake.write(bcs::MI_MODE, masked_off(bcs::MODE_STOP));
    awake.read(bcs::MI_MODE);
    Ok(())
}

/// Hands the engine everything up to `tail`. The kernel makes its writes
/// to the ring's memory visible first.
pub fn submit<R: Regs + ?Sized>(awake: &mut Awake<'_, R>, tail: u32) {
    awake.write(bcs::TAIL, tail & bcs::TAIL_ADDR);
    awake.read(bcs::TAIL);
}

/// Stops the blitter and resets its domain alone (Linux `__intel_engine_
/// reset_bh`: stop, then `GDRST` twice). The ring must be started again
/// after, which also invalidates the TLB: a soft reset does not (PRM Vol 1
/// Part 4, 2.1.7.4).
pub fn reset<R: Regs + ?Sized>(
    awake: &mut Awake<'_, R>,
    clock: &dyn Clock,
    t: &Timeouts,
) -> Result<(), Refusal> {
    awake.write(bcs::MI_MODE, masked_on(bcs::MODE_STOP));
    // Linux goes on if the engine does not stop: the reset is the remedy.
    let _ = awake.wait(clock, bcs::MI_MODE, t.idle, |v| v & bcs::MODE_IDLE != 0);
    stop_ring(awake);
    for _ in 0..2 {
        awake.write(regs::GDRST, regs::GDRST_BLITTER);
        awake
            .wait(clock, regs::GDRST, t.reset, |v| {
                v & regs::GDRST_BLITTER == 0
            })
            .map_err(|_| Refusal::Timeout)?;
    }
    Ok(())
}
