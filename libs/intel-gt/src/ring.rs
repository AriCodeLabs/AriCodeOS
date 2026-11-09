// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The engines' rings, which only the kernel writes (ADR 0083, 0086). Each
//! blitter request holds one client batch's checked commands, then stores
//! its number in the status page. The bring-up, the stop and the reset follow Linux's
//! `xcs_resume`, `intel_engine_stop_cs` and `gen6_hw_domain_reset`, with the
//! limits of PRM Vol 1 Part 4, 2.1.

use crate::cmd;
use crate::gtt::{Layout, PAGE};
use crate::power::Awake;
use crate::regs::{self, bcs, masked_off, masked_on, Engine};
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
/// Dwords of one request besides its commands: a TLB invalidation (4), so
/// the commands see the PPGTT as it is now; flush and store (3); the
/// repeated stores; a flush (3); the interrupt and a no-op; and one no-op
/// more when the commands are an odd number, to end on a quadword.
pub const REQUEST_EXTRA: usize = 4 + 3 + 3 * STORE_REPEATS + 3 + 2 + 1;

/// Whether a request numbered `seqno` has finished, given the number last
/// stored. Numbers wrap.
pub fn passed(stored: u32, seqno: u32) -> bool {
    stored.wrapping_sub(seqno) as i32 >= 0
}

/// The kernel's side of the ring: its size and where the next command goes.
/// The ring's memory is given to each call. The PRM asks for uncached
/// memory (Vol 1 Part 4, 2.1.9); the kernel maps it through the LLC with the
/// CPU writing back, as Linux does on this family's shared LLC.
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

    /// Writes a request of `body` (checked blitter commands, no
    /// `MI_BATCH_BUFFER_END`) that then stores `seqno`; returns the new tail
    /// for `TAIL`, or `None` if the ring has no room for it yet. A request
    /// that would cross the ring's end starts again at 0, the rest filled
    /// with no-ops.
    pub fn request(&mut self, mem: &mut [u32], head: u32, body: &[u32], seqno: u32) -> Option<u32> {
        let mut before = [cmd::NOOP; 4];
        before[0] = cmd::FLUSH_DW
            | cmd::FLUSH_DW_STORE_DWORD
            | cmd::FLUSH_DW_STORE_INDEX
            | cmd::FLUSH_DW_INVALIDATE_TLB;
        before[1] = SCRATCH_ADDR | cmd::FLUSH_DW_GLOBAL_GTT;
        before[2] = 0;
        let mut after = [cmd::NOOP; REQUEST_EXTRA - 4];
        let mut n = 0;
        let mut put = |w: u32| {
            after[n] = w;
            n += 1;
        };
        // An odd body leaves the rest one dword off a quadword.
        if body.len() % 2 == 1 {
            put(cmd::NOOP);
        }
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
        self.emit_parts(mem, head, &[&before, body, &after[..n]])
    }

    /// Writes `words`, a whole number of quadwords (TAIL moves in them),
    /// and returns the new tail, or `None` if the ring has no room for them
    /// yet. Words that would cross the ring's end start again at 0, the rest
    /// filled with no-ops.
    pub fn emit(&mut self, mem: &mut [u32], head: u32, words: &[u32]) -> Option<u32> {
        self.emit_parts(mem, head, &[words])
    }

    /// [`Ring::emit`] of `parts` one after another, as one piece.
    fn emit_parts(&mut self, mem: &mut [u32], head: u32, parts: &[&[u32]]) -> Option<u32> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if !len.is_multiple_of(2) {
            return None;
        }
        let need = (len * 4) as u32;
        let to_end = self.bytes - self.tail;
        let wrap = if need > to_end { to_end } else { 0 };
        if need >= self.bytes
            || self.space(head) < need + wrap
            || mem.len() * 4 < self.bytes as usize
        {
            return None;
        }
        let mut at = (self.tail / 4) as usize;
        if wrap != 0 {
            mem[at..(self.bytes / 4) as usize].fill(cmd::NOOP);
            at = 0;
        }
        for p in parts {
            mem[at..at + p.len()].copy_from_slice(p);
            at += p.len();
        }
        self.tail = (at * 4) as u32 % self.bytes;
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
fn stop_ring<R: Regs + ?Sized>(awake: &mut Awake<'_, R>, e: &Engine) -> bool {
    let tail = awake.read(e.tail());
    awake.write(e.head(), tail);
    awake.read(e.head());
    awake.write(e.ctl(), 0);
    awake.read(e.ctl());
    awake.write(e.head(), 0);
    awake.write(e.tail(), 0);
    awake.read(e.head()) & bcs::HEAD_ADDR == 0
}

/// Starts `e` on an empty ring with the PPGTT on (Linux `xcs_resume`, for
/// every engine). Its status page and ring must be mapped at `layout`'s
/// entries, and the page directory written, before this.
pub fn start<R: Regs + ?Sized>(
    awake: &mut Awake<'_, R>,
    clock: &dyn Clock,
    t: &Timeouts,
    e: &Engine,
    layout: &Layout,
    ring: &mut Ring,
) -> Result<(), Refusal> {
    if !stop_ring(awake, e) {
        return Err(Refusal::RingRunning);
    }
    ring.rewind();
    let (status, at) = layout.placement(e);
    awake.write(e.hws_pga, status * PAGE as u32);
    awake.read(e.hws_pga);
    awake.write(e.hwstam(), !0);
    // A synchronizing flush with a TLB invalidation; the ring must be idle.
    if awake.read(e.mi_mode()) & bcs::MODE_IDLE == 0 {
        return Err(Refusal::NotIdle);
    }
    awake.write(
        e.instpm(),
        masked_on(bcs::INSTPM_TLB_INVALIDATE | bcs::INSTPM_SYNC_FLUSH),
    );
    awake
        .wait(clock, e.instpm(), t.reset, |v| {
            v & bcs::INSTPM_SYNC_FLUSH == 0
        })
        .map_err(|_| Refusal::Timeout)?;
    awake.read(e.head());
    awake.write(e.start(), at * PAGE as u32);
    awake.write(e.pp_dir_dclv(), bcs::DCLV_2G);
    awake.write(e.pp_dir_base(), layout.pp_dir_base());
    awake.write(e.mode(), masked_on(bcs::PPGTT_ENABLE));
    // The head may not take at the first write (Linux: "experimented ...
    // 50ms works best").
    let start = clock.now_us();
    loop {
        awake.write(e.head(), 0);
        if awake.read(e.head()) == 0 {
            break;
        }
        if clock.now_us().saturating_sub(start) > t.readback {
            return Err(Refusal::Timeout);
        }
    }
    awake.write(e.tail(), 0);
    if awake.read(e.head()) != awake.read(e.tail()) {
        return Err(Refusal::Timeout);
    }
    awake.write(e.ctl(), (layout.ring_pages - 1) << 12 | bcs::CTL_VALID);
    awake
        .wait(clock, e.ctl(), 5_000, |v| v & bcs::CTL_VALID != 0)
        .map_err(|_| Refusal::Timeout)?;
    awake.write(e.mi_mode(), masked_off(bcs::MODE_STOP));
    awake.read(e.mi_mode());
    Ok(())
}

/// Hands `e` everything up to `tail`. The kernel makes its writes to the
/// ring's memory visible first.
pub fn submit<R: Regs + ?Sized>(awake: &mut Awake<'_, R>, e: &Engine, tail: u32) {
    awake.write(e.tail(), tail & bcs::TAIL_ADDR);
    awake.read(e.tail());
}

/// Stops `e` and resets its domain alone (Linux `__intel_engine_reset_bh`:
/// stop, then `GDRST` twice). The ring must be started again after, which
/// also invalidates the TLB: a soft reset does not (PRM Vol 1 Part 4,
/// 2.1.7.4). A render reset also clears the render engine's settings and
/// the blitter's TLB mode bits, which live in its mode register (ADR 0086):
/// they must be applied again.
pub fn reset<R: Regs + ?Sized>(
    awake: &mut Awake<'_, R>,
    clock: &dyn Clock,
    t: &Timeouts,
    e: &Engine,
) -> Result<(), Refusal> {
    awake.write(e.mi_mode(), masked_on(bcs::MODE_STOP));
    // Linux goes on if the engine does not stop: the reset is the remedy.
    let _ = awake.wait(clock, e.mi_mode(), t.idle, |v| v & bcs::MODE_IDLE != 0);
    stop_ring(awake, e);
    for _ in 0..2 {
        awake.write(regs::GDRST, e.reset);
        awake
            .wait(clock, regs::GDRST, t.reset, |v| v & e.reset == 0)
            .map_err(|_| Refusal::Timeout)?;
    }
    Ok(())
}
