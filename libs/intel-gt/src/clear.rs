// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The residue clear at a change of client (ADR 0086, decision 3;
//! CVE-2019-14615). An EU thread starts with the registers the last thread
//! on its slot left, whatever program that was. Linux clears them before a
//! change of context with a binary kernel (`gen7_renderclear.c`,
//! `ivb.asm`): 128 media objects whose threads wait in a loop so that, it
//! hopes, every slot gets one; nothing checks that they did.
//!
//! Here the GPGPU walker runs groups as large as a half-slice holds, one
//! group a half-slice, and every thread writes the clear's number at its own
//! slot (from `sr0`: half-slice, EU and thread) in a marks page, so the
//! kernel counts the slots the clear reached. Each thread then writes two
//! blocks of zeros through the render cache, the 512 bytes Linux writes a
//! thread to push the last client's lines out, and zeroes every register
//! but g127, which holds its own header for the end. The program is
//! assembled here by `aricode-gen7-eu`.
//!
//! The shared local memory keeps residue too, and nothing here clears it:
//! it exists only while `L3CNTLREG2` turns it on, which the firmware leaves
//! off and nothing a client runs can change. [`slm_on`] says if it is on.

use aricode_abi::gpu::Dispatch;
use aricode_gen7_eu::{g, Asm, ExecSize::*, Imm, Message, Reg};

use crate::compute::{self, Descriptor};

/// The clear's memory, from its base in the PPGTT: the state page, the
/// marks page, then the sweep surface.
pub const STATE_PAGE: u32 = 0;
pub const MARKS_PAGE: u32 = 1;
pub const SWEEP_PAGE: u32 = 2;
/// The sweep surface: a 32-byte column for each of a slot's 8 threads,
/// 16 rows for each of its 32 EUs: 256 bytes by 512 rows.
pub const SWEEP_WIDTH: u32 = 8 * 32;
pub const SWEEP_HEIGHT: u32 = 32 * 16;
pub const SWEEP_PAGES: u32 = SWEEP_WIDTH * SWEEP_HEIGHT / 4096;
pub const PAGES: u32 = SWEEP_PAGE + SWEEP_PAGES;
/// The marks surface: a dword for each of 8 threads, a row for each of 32
/// EUs (half-slice and EU number, 5 bits of `sr0`).
pub const MARKS_WIDTH: u32 = 8 * 4;
pub const MARKS_HEIGHT: u32 = 32;

/// Where the pieces of the state page sit.
const CODE: u32 = 0x000;
const CODE_SLOTS: usize = 0xC00 / 16;
const BINDING_TABLE: u32 = 0xC00;
const SWEEP_STATE: u32 = 0xC40;
const MARKS_STATE: u32 = 0xC80;
const DESCRIPTOR: u32 = 0xD00;
const CURBE: u32 = 0xD40;

/// `GEN7_L3CNTLREG2` and its shared local memory enable (Mesa
/// `genxml/gen70.xml`).
pub const L3CNTLREG2: u32 = 0xB020;
const SLM_ENABLE: u32 = 1 << 0;

/// Whether the shared local memory is on, from `L3CNTLREG2`.
pub fn slm_on(l3cntlreg2: u32) -> bool {
    l3cntlreg2 & SLM_ENABLE != 0
}

/// Groups that fill every thread slot of an Ivy Bridge GPU of `threads`
/// threads: a group as large as a half-slice holds, one a half-slice; GT1
/// has one half-slice of 36 threads, GT2 two of 64 (PRM Vol 1 Part 1;
/// Linux `gen7_renderclear.c`). `(threads a group, groups)`.
pub fn groups(threads: u16) -> Option<(u16, u16)> {
    match threads {
        1..=64 => Some((threads, 1)),
        t if (65..=128).contains(&t) && t.is_multiple_of(2) => Some((t / 2, 2)),
        _ => None,
    }
}

/// Assembles the clear's EU program into `code`; its instructions.
pub fn program(code: &mut [[u32; 4]]) -> Option<usize> {
    let sweep = Message::media_block_write(0, 8).ok()?;
    let mark = Message::media_block_write(1, 1).ok()?;
    let mut asm = Asm::new(code);
    // g127: this thread's header, for the messages and the end.
    asm.mov(E8, g(127).ud(), g(0).ud())
        // g126.0: the slot's column in bytes (thread number * 4);
        // g126.1: its row (half-slice and EU number); g126.2: the clear's
        // number, from the CURBE (g1.0).
        .and(E1, g(126).ud(), Reg::sr0().ud().scalar(), Imm::ud(7))
        .shl(E1, g(126).ud(), g(126).ud().scalar(), Imm::ud(2))
        .shr(E1, g(126).ud().sub(1), Reg::sr0().ud().scalar(), Imm::ud(8))
        .and(
            E1,
            g(126).ud().sub(1),
            g(126).ud().sub(1).scalar(),
            Imm::ud(0x1F),
        )
        .mov(E1, g(126).ud().sub(2), g(1).ud().scalar());
    // Every register from g1 to g125 to zero.
    asm.mov(E8, g(1).ud(), Imm::ud(0));
    for r in (2..126).step_by(2) {
        asm.mov(E16, g(r).ud(), Imm::ud(0));
    }
    // Two blocks of 16 by 16 zero bytes in the sweep surface, at x = thread
    // * 32 (+ 16), y = row * 16; g3 to g10 are the zeros.
    asm.mov(E8, g(2).ud(), g(127).ud())
        .shl(E1, g(2).ud(), g(126).ud().scalar(), Imm::ud(3))
        .shl(
            E1,
            g(2).ud().sub(1),
            g(126).ud().sub(1).scalar(),
            Imm::ud(4),
        )
        .mov(E1, g(2).ud().sub(2), Imm::ud(0x000F_000F))
        .send(E8, Reg::null().ud(), g(2).ud(), sweep)
        .add(E1, g(2).ud(), g(2).ud().scalar(), Imm::ud(16))
        .send(E8, Reg::null().ud(), g(2).ud(), sweep);
    // The mark: the clear's number, 4 bytes at the slot.
    asm.mov(E8, g(11).ud(), g(127).ud())
        .mov(E1, g(11).ud(), g(126).ud().scalar())
        .mov(E1, g(11).ud().sub(1), g(126).ud().sub(1).scalar())
        .mov(E1, g(11).ud().sub(2), Imm::ud(3))
        .mov(E8, g(12).ud(), g(126).ud().sub(2).scalar())
        .send(E8, Reg::null().ud(), g(11).ud(), mark);
    // What held the slot, the number and the headers, to zero too; the
    // hardware has read each message's registers when it took it.
    asm.mov(E16, g(2).ud(), Imm::ud(0))
        .mov(E16, g(10).ud(), Imm::ud(0))
        .mov(E16, g(12).ud(), Imm::ud(0))
        .mov(E8, g(126).ud(), Imm::ud(0))
        .send(
            E8,
            Reg::null().ud(),
            g(127).ud(),
            Message::thread_spawner_eot(),
        );
    asm.finish().ok().map(|p| p.len())
}

/// Writes the state page of a clear whose memory starts at `base` in the
/// PPGTT, numbered `number`, for a GPU of `threads` threads; the dispatch
/// that runs it.
pub fn prepare(state: &mut [u32], base: u32, number: u32, threads: u16) -> Option<Dispatch> {
    let (per_group, half_slices) = groups(threads)?;
    if state.len() < 1024 {
        return None;
    }
    state[..1024].fill(0);
    let mut code = [[0u32; 4]; CODE_SLOTS];
    let n = program(&mut code)?;
    for (i, inst) in code[..n].iter().enumerate() {
        let at = (CODE / 4) as usize + 4 * i;
        state[at..at + 4].copy_from_slice(inst);
    }
    let page = |p: u32| base + p * 4096;
    let sweep = compute::surface_2d(
        compute::FORMAT_R8_UNORM,
        page(SWEEP_PAGE),
        SWEEP_WIDTH,
        SWEEP_HEIGHT,
        SWEEP_WIDTH,
    )?;
    let marks = compute::surface_2d(
        compute::FORMAT_R8_UNORM,
        page(MARKS_PAGE),
        MARKS_WIDTH,
        MARKS_HEIGHT,
        MARKS_WIDTH,
    )?;
    let put = |state: &mut [u32], at: u32, words: &[u32]| {
        state[(at / 4) as usize..(at / 4) as usize + words.len()].copy_from_slice(words);
    };
    put(state, SWEEP_STATE, &sweep);
    put(state, MARKS_STATE, &marks);
    put(state, BINDING_TABLE, &[SWEEP_STATE, MARKS_STATE]);
    let descriptor = Descriptor {
        kernel: CODE,
        binding_table: BINDING_TABLE,
        binding_entries: 2,
        curbe_regs: 1,
        slm_blocks: 0,
        barrier: false,
    }
    .encode()?;
    put(state, DESCRIPTOR, &descriptor);
    put(state, CURBE, &[number]);
    let at = page(STATE_PAGE);
    Some(Dispatch {
        general_base: 0,
        surface_base: at,
        dynamic_base: at,
        instruction_base: at,
        scratch: 0,
        max_threads: threads,
        urb_entries: 0,
        urb_entry_regs: 0,
        curbe_regs: 1,
        curbe_offset: CURBE,
        curbe_len: 32,
        descriptor_offset: DESCRIPTOR,
        descriptors: 1,
        descriptor: 0,
        simd: 16,
        group: [per_group as u8, 1, 1],
        start: [0, 0, 0],
        groups: [half_slices as u32, 1, 1],
        right_mask: 0xFFFF,
        bottom_mask: 0xFFFF_FFFF,
    })
}

/// The slots that marked the marks page with `number`.
pub fn covered(marks: &[u32], number: u32) -> u32 {
    let n = (MARKS_WIDTH / 4 * MARKS_HEIGHT) as usize;
    marks.iter().take(n).filter(|&&m| m == number).count() as u32
}
