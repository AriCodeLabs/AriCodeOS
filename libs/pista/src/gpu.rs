// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Gen7 EU bloom/colour kernel. Geometry remains on the CPU; this is a real
//! compute pass over the rendered scene, before the HUD. No kernel changes.
use aricode_abi::gpu::Dispatch;
use aricode_gen7_eu::{g, Asm, CondMod, ExecSize::*, Flag, Imm, Inst, Message, Pred, Reg};
use aricode_intel_gt::compute::{self, Descriptor};

pub const INPUT: u64 = 0x10_0000;
pub const OUTPUT: u64 = 0x50_0000;
pub const STATE: u64 = 0x90_0000;
pub const STATE_BYTES: usize = 8192;
const TABLE: usize = 0x1000;
const SURFACES: usize = 0x1040;
const DESC: usize = 0x1100;

pub fn program(code: &mut [[u32; 4]]) -> Option<usize> {
    let read = Message::media_block_read(0, 1).ok()?;
    let write = Message::media_block_write(1, 1).ok()?;
    let mut a = Asm::new(code);
    // Eight pixels per thread. Source has a replicated four-pixel border.
    a.shl(E1, g(1).ud(), g(0).ud().sub(1).scalar(), Imm::ud(5))
        .add(E1, g(1).ud(), g(1).ud().scalar(), Imm::ud(16))
        .add(E1, g(1).ud().sub(1), g(0).ud().sub(6).scalar(), Imm::ud(4));
    for (i, (dx, dy)) in crate::effects::TAPS.into_iter().enumerate() {
        a.mov(E8, g(2).ud(), g(0).ud())
            .add(E1, g(2).ud(), g(1).ud().scalar(), Imm::ud((dx * 4) as u32))
            .add(
                E1,
                g(2).ud().sub(1),
                g(1).ud().sub(1).scalar(),
                Imm::ud(dy as u32),
            )
            .mov(E1, g(2).ud().sub(2), Imm::ud(31))
            .send(E8, g(8 + i as u8).ud(), g(2).ud(), read);
    }
    // g20 = write header; g21 = packed result. g22..25 are channel scratch.
    a.mov(E8, g(21).ud(), Imm::ud(0));
    for (shift, scale) in [(16, 244), (8, 240), (0, 236)] {
        a.mov(E8, g(22).ud(), Imm::ud(0));
        for i in 0..5u8 {
            a.shr(E8, g(23).ud(), g(8 + i).ud(), Imm::ud(shift))
                .and(E8, g(23).ud(), g(23).ud(), Imm::ud(255))
                .mul(E8, g(23).ud(), g(23).ud(), g(23).ud())
                .shr(E8, g(23).ud(), g(23).ud(), Imm::ud(8))
                .mul(E8, g(23).ud(), g(23).ud(), g(23).ud())
                .shr(E8, g(23).ud(), g(23).ud(), Imm::ud(8));
            if i == 0 {
                a.shl(E8, g(23).ud(), g(23).ud(), Imm::ud(2));
            }
            a.add(E8, g(22).ud(), g(22).ud(), g(23).ud());
        }
        a.shr(E8, g(22).ud(), g(22).ud(), Imm::ud(5))
            .shr(E8, g(24).ud(), g(8).ud(), Imm::ud(shift))
            .and(E8, g(24).ud(), g(24).ud(), Imm::ud(255))
            .mul(E8, g(24).ud(), g(24).ud(), Imm::ud(scale))
            .shr(E8, g(24).ud(), g(24).ud(), Imm::ud(8))
            .add(E8, g(24).ud(), g(24).ud(), g(22).ud())
            .emit(Inst::cmp(
                E8,
                CondMod::G,
                Flag::F0_0,
                Reg::null().ud(),
                g(24).ud(),
                Imm::ud(255),
            ))
            .emit(Inst::mov(E8, g(24).ud(), Imm::ud(255)).pred(Pred::new(Flag::F0_0)))
            .shl(E8, g(24).ud(), g(24).ud(), Imm::ud(shift))
            .or(E8, g(21).ud(), g(21).ud(), g(24).ud());
    }
    a.mov(E8, g(20).ud(), g(0).ud())
        .shl(E1, g(20).ud(), g(0).ud().sub(1).scalar(), Imm::ud(5))
        .mov(E1, g(20).ud().sub(1), g(0).ud().sub(6).scalar())
        .mov(E1, g(20).ud().sub(2), Imm::ud(31))
        .send(E8, Reg::null().ud(), g(20).ud(), write)
        .mov(E8, g(127).ud(), g(0).ud())
        .send(
            E8,
            Reg::null().ud(),
            g(127).ud(),
            Message::thread_spawner_eot(),
        );
    a.finish().ok().map(|p| p.len())
}

pub fn setup(state: &mut [u32], width: u32, height: u32, threads: u16) -> Option<Dispatch> {
    if width == 0
        || width > 960
        || !width.is_multiple_of(8)
        || height == 0
        || height > 540
        || threads == 0
        || state.len() < STATE_BYTES / 4
    {
        return None;
    }
    let mut code = [[0; 4]; 256];
    let n = program(&mut code)?;
    if n * 16 > TABLE {
        return None;
    }
    state.fill(0);
    for (i, inst) in code[..n].iter().enumerate() {
        state[i * 4..i * 4 + 4].copy_from_slice(inst);
    }
    for (i, (address, w, h)) in [(INPUT, width + 8, height + 8), (OUTPUT, width, height)]
        .into_iter()
        .enumerate()
    {
        let surface =
            compute::surface_2d(compute::FORMAT_R8_UNORM, address as u32, w * 4, h, w * 4)?;
        let offset = SURFACES + i * 32;
        state[offset / 4..offset / 4 + 8].copy_from_slice(&surface);
        state[TABLE / 4 + i] = offset as u32;
    }
    let desc = Descriptor {
        binding_table: TABLE as u32,
        binding_entries: 2,
        ..Descriptor::default()
    }
    .encode()?;
    state[DESC / 4..DESC / 4 + 8].copy_from_slice(&desc);
    let d = Dispatch {
        surface_base: STATE as u32,
        dynamic_base: STATE as u32,
        instruction_base: STATE as u32,
        max_threads: threads,
        descriptor_offset: DESC as u32,
        descriptors: 1,
        simd: 8,
        group: [1, 1, 1],
        groups: [width / 8, height, 1],
        right_mask: 255,
        bottom_mask: u32::MAX,
        ..Dispatch::default()
    };
    compute::validate(&d, threads).ok()?;
    Some(d)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn compute_program_and_both_quality_modes_fit_the_gen7_contract() {
        let mut code = [[0; 4]; 256];
        assert!(program(&mut code).unwrap() * 16 < TABLE);
        for threads in [36, 128] {
            for (w, h) in [(960, 540), (480, 270), (16, 8)] {
                let mut state = [0; STATE_BYTES / 4];
                let d = setup(&mut state, w, h, threads).unwrap();
                assert_eq!(d.groups, [w / 8, h, 1]);
                assert!(compute::emit(&d, threads).is_ok());
            }
        }
    }
    #[test]
    fn unsupported_sizes_are_rejected_before_submission() {
        let mut state = [0; STATE_BYTES / 4];
        for (w, h) in [(0, 540), (961, 540), (959, 540), (960, 541)] {
            assert!(setup(&mut state, w, h, 36).is_none());
        }
    }
}
