// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The render engine's settings and commands on Ivy Bridge (ADR 0086). The
//! laptop's firmware applies none of the settings Linux applies on this
//! family (the read-only probe of dev build 55), so the kernel does, from
//! Linux's own tables (`intel_workarounds.c`: `ivb_gt_workarounds_init`,
//! `rcs_engine_wa_init`, `gen7_ctx_workarounds_init`; `intel_clock_gating.c`:
//! `ivb_init_clock_gating`), each named after Linux's workaround. Display
//! registers among them are left alone: the display is not this engine's.

use crate::power::Awake;
use crate::regs::{masked_off, masked_on};
use crate::Regs;

/// How a setting changes its register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A masked register: these bits set, the rest kept.
    MaskedOn(u32),
    /// A masked register: these bits cleared.
    MaskedOff(u32),
    /// A masked register: the field `mask` set to `value`.
    MaskedField { mask: u32, value: u32 },
    /// The whole register written.
    Write(u32),
    /// Read, these bits set, written back.
    Set(u32),
    /// Read, these bits cleared, written back.
    Clear(u32),
    /// Read, the field `mask` set to `value`, written back.
    Field { mask: u32, value: u32 },
}

/// One register setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub register: u32,
    pub change: Change,
}

const fn s(register: u32, change: Change) -> Setting {
    Setting { register, change }
}

/// Once for the GT, before any engine works (`ivb_gt_workarounds_init`).
pub const GT: [Setting; 4] = [
    // WaDisableRHWOOptimizationForRenderHang: GEN7_COMMON_SLICE_CHICKEN1.
    s(0x7010, Change::MaskedOff(1 << 10)),
    // WaApplyL3ControlAndL3ChickenMode: GEN7_L3CNTLREG1, L3_CHICKEN_MODE.
    s(0xB01C, Change::Write(0x3C47_FF8C)),
    s(0xB030, Change::Write(0x2000_0000)),
    // WaForceL3Serialization: GEN7_L3SQCREG4, URB read CAM match off.
    s(0xB034, Change::Clear(1 << 27)),
];

/// Once for the render engine, and again after it is reset, which clears
/// them (`rcs_engine_wa_init`, the Ivy Bridge and gen7 parts).
pub const RENDER: [Setting; 5] = [
    // WaDisableEarlyCull: _3D_CHICKEN3.
    s(0x2090, Change::MaskedOn(1 << 10)),
    // WaVSThreadDispatchOverride: GEN7_FF_THREAD_MODE, every stage by the
    // hardware.
    s(
        0x20A0,
        Change::Field {
            mask: 0x0007_7070,
            value: 0,
        },
    ),
    // WaBCSVCSTlbInvalidationMode: the render engine's mode register also
    // sets how the other engines' TLBs are invalidated.
    s(0x229C, Change::MaskedOn(1 << 13 | 1 << 11)),
    // GEN7_GT_MODE: WIZ hashing 16x4.
    s(
        0x7008,
        Change::MaskedField {
            mask: 1 << 9 | 1 << 7,
            value: 1 << 9,
        },
    ),
    // WaDisableAsyncFlipPerfMode: the render ring's MI_MODE.
    s(0x209C, Change::MaskedOn(1 << 14)),
];

/// Once, the GT's clock gating (`ivb_init_clock_gating`, a GT2 part).
pub const CLOCK_GATING: [Setting; 5] = [
    // WaDisableDopClockGating: GEN7_ROW_CHICKEN2, both half slices on GT2.
    s(0xE4F4, Change::MaskedOn(1 << 0)),
    s(0xF4F4, Change::MaskedOn(1 << 0)),
    // WaDisableRCZUnitClockGating: GEN6_UCGCTL2 bit 13.
    s(0x9404, Change::Write(1 << 13)),
    // WaCatErrorRejectionIssue: GEN7_SQ_CHICKEN_MBCUNIT_CONFIG.
    s(0x9030, Change::Set(1 << 11)),
    // GEN6_MBCUNIT_SNPCR: LLC snoops at medium priority.
    s(
        0x900C,
        Change::Field {
            mask: 3 << 21,
            value: 1 << 21,
        },
    ),
];

/// Kept in a context image under Linux; without one, sent from the ring
/// before each request, so neither RC6 nor a reset loses them
/// (`gen7_ctx_workarounds_init`). All masked, so a register load sets them.
pub const CONTEXT: [Setting; 3] = [
    // INSTPM: force ordering.
    s(0x20C0, Change::MaskedOn(1 << 7)),
    // WaDisable_RenderCache_OperationalFlush: CACHE_MODE_0.
    s(0x7000, Change::MaskedOff(1 << 0)),
    // WaDisable4x2SubspanOptimization: CACHE_MODE_1.
    s(0x7004, Change::MaskedOn(1 << 6)),
];

/// Applies `settings` through MMIO, the GT awake.
pub fn apply<R: Regs + ?Sized>(awake: &mut Awake<'_, R>, settings: &[Setting]) {
    for st in settings {
        let r = st.register;
        let value = match st.change {
            Change::MaskedOn(bits) => masked_on(bits),
            Change::MaskedOff(bits) => masked_off(bits),
            Change::MaskedField { mask, value } => mask << 16 | value,
            Change::Write(v) => v,
            Change::Set(bits) => awake.read(r) | bits,
            Change::Clear(bits) => awake.read(r) & !bits,
            Change::Field { mask, value } => awake.read(r) & !mask | value,
        };
        awake.write(r, value);
    }
}

/// `MI_LOAD_REGISTER_IMM` loading masked settings from the ring: the header
/// then a register and a value each. `None` for a setting a load cannot
/// make (one that reads its register first) or too many for `out`.
pub fn load_masked(settings: &[Setting], out: &mut [u32]) -> Option<usize> {
    let n = settings.len();
    if n == 0 || out.len() < 1 + 2 * n {
        return None;
    }
    // Length: two dwords a register, less the header's two (Linux
    // `MI_LOAD_REGISTER_IMM(x)`).
    out[0] = 0x22 << 23 | (2 * n as u32 - 1);
    for (i, st) in settings.iter().enumerate() {
        out[1 + 2 * i] = st.register;
        out[2 + 2 * i] = match st.change {
            Change::MaskedOn(bits) => masked_on(bits),
            Change::MaskedOff(bits) => masked_off(bits),
            Change::MaskedField { mask, value } => mask << 16 | value,
            _ => return None,
        };
    }
    Some(1 + 2 * n)
}

/// `PIPE_CONTROL` (PRM Vol 2 Part 1; Linux `GFX_OP_PIPE_CONTROL(4)`).
pub const PIPE_CONTROL: u32 = 0x3 << 29 | 0x3 << 27 | 0x2 << 24 | 2;
pub const PC_GLOBAL_GTT: u32 = 1 << 24;
pub const PC_CS_STALL: u32 = 1 << 20;
pub const PC_TLB_INVALIDATE: u32 = 1 << 18;
pub const PC_MEDIA_STATE_CLEAR: u32 = 1 << 16;
pub const PC_QW_WRITE: u32 = 1 << 14;
pub const PC_RENDER_TARGET_FLUSH: u32 = 1 << 12;
pub const PC_INSTRUCTION_INVALIDATE: u32 = 1 << 11;
pub const PC_TEXTURE_INVALIDATE: u32 = 1 << 10;
pub const PC_FLUSH_ENABLE: u32 = 1 << 7;
pub const PC_DC_FLUSH: u32 = 1 << 5;
pub const PC_VF_INVALIDATE: u32 = 1 << 4;
pub const PC_CONST_INVALIDATE: u32 = 1 << 3;
pub const PC_STATE_INVALIDATE: u32 = 1 << 2;
pub const PC_STALL_AT_SCOREBOARD: u32 = 1 << 1;
pub const PC_DEPTH_FLUSH: u32 = 1 << 0;

/// A stall before a state cache invalidation, which needs one before it
/// (Linux `gen7_stall_cs`).
pub const fn stall() -> [u32; 4] {
    [PIPE_CONTROL, PC_CS_STALL | PC_STALL_AT_SCOREBOARD, 0, 0]
}

/// Flushes every cache and invalidates every read cache and the TLB,
/// storing to `scratch` in the global GTT, as the stall and the post-sync
/// write the hardware asks for (Linux `gen7_emit_flush_rcs`, both modes).
/// Preceded by [`stall`].
pub const fn flush_invalidate(scratch: u32) -> [u32; 4] {
    [
        PIPE_CONTROL,
        PC_CS_STALL
            | PC_QW_WRITE
            | PC_GLOBAL_GTT
            | PC_RENDER_TARGET_FLUSH
            | PC_DEPTH_FLUSH
            | PC_DC_FLUSH
            | PC_FLUSH_ENABLE
            | PC_TLB_INVALIDATE
            | PC_INSTRUCTION_INVALIDATE
            | PC_TEXTURE_INVALIDATE
            | PC_VF_INVALIDATE
            | PC_CONST_INVALIDATE
            | PC_STATE_INVALIDATE
            | PC_MEDIA_STATE_CLEAR,
        scratch,
        0,
    ]
}

/// The end of a render request: every cache flushed, then `seqno` stored
/// at `address` in the global GTT, then an interrupt (Linux
/// `gen7_emit_breadcrumb_rcs`).
pub const fn breadcrumb(address: u32, seqno: u32) -> [u32; 6] {
    [
        PIPE_CONTROL,
        PC_RENDER_TARGET_FLUSH
            | PC_DEPTH_FLUSH
            | PC_DC_FLUSH
            | PC_FLUSH_ENABLE
            | PC_QW_WRITE
            | PC_GLOBAL_GTT
            | PC_CS_STALL,
        address,
        seqno,
        crate::cmd::USER_INTERRUPT,
        crate::cmd::NOOP,
    ]
}

/// Dwords of a render request around its body: a stall, a flush and
/// invalidation, the context settings, the end, and a no-op to keep whole
/// quadwords.
pub const REQUEST_EXTRA: usize = 4 + 4 + 7 + 6 + 1;

/// A render request: `body` (commands the kernel wrote) between a full flush
/// and invalidation, with the context settings loaded, and the store of
/// `seqno` at `seqno_at` (both addresses in the global GTT, `scratch` a dword
/// the flush may write). Returns the dwords written to `out`, a whole number
/// of quadwords, or `None` if `out` is too small.
pub fn request(
    scratch: u32,
    seqno_at: u32,
    seqno: u32,
    body: &[u32],
    out: &mut [u32],
) -> Option<usize> {
    fn put(out: &mut [u32], n: &mut usize, words: &[u32]) -> Option<()> {
        out.get_mut(*n..*n + words.len())?.copy_from_slice(words);
        *n += words.len();
        Some(())
    }
    let mut n = 0;
    put(out, &mut n, &stall())?;
    put(out, &mut n, &flush_invalidate(scratch))?;
    let mut lri = [0u32; 7];
    load_masked(&CONTEXT, &mut lri)?;
    put(out, &mut n, &lri)?;
    put(out, &mut n, body)?;
    put(out, &mut n, &breadcrumb(seqno_at, seqno))?;
    if n % 2 == 1 {
        put(out, &mut n, &[crate::cmd::NOOP])?;
    }
    Some(n)
}
