// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! igt's gen7 GPGPU fill kernel, which `gem_gpgpu_fill` runs on Ivy Bridge.
//!
//! The words are `gen7_gpgpu_kernel` from igt-gpu-tools `lib/gpgpu_fill.c`
//! (Copyright 2015 Intel Corporation, MIT licence), assembled by
//! intel-gen4asm from `lib/i915/shaders/gpgpu/gpgpu_fill.gxa`, whose lines
//! are quoted before each call.

use super::assert_program;
use crate::ExecSize::*;
use crate::{g, Imm, Inst, Message, Reg};

const GEN7_GPGPU_KERNEL: [[u32; 4]; 10] = [
    [0x00400001, 0x20200231, 0x00000020, 0x00000000],
    [0x00000041, 0x20400c21, 0x00000004, 0x00000010],
    [0x00000001, 0x20440021, 0x00000018, 0x00000000],
    [0x00600001, 0x20800021, 0x008d0000, 0x00000000],
    [0x00200001, 0x20800021, 0x00450040, 0x00000000],
    [0x00000001, 0x20880061, 0x00000000, 0x0000000f],
    [0x00800001, 0x20a00021, 0x00000020, 0x00000000],
    [0x05800031, 0x24001ca8, 0x00000080, 0x060a8000],
    [0x00600001, 0x2e000021, 0x008d0000, 0x00000000],
    [0x07800031, 0x20001ca8, 0x00000e00, 0x82000010],
];

#[test]
fn igt_gen7_gpgpu_fill_kernel_bit_for_bit() {
    let media_block_write = Message::media_block_write(0, 2).unwrap().desc_as_d();
    let eot = Message::thread_spawner_eot().desc_as_d();
    assert_program(&GEN7_GPGPU_KERNEL, &[], |asm| {
        // mov(4) COLOR<1>UB COLORUB: replicate the colour byte.
        asm.mov(E4, g(1).ub(), g(1).ub().scalar())
            // mul(1) ORIG_X X 0x10UD: x offset of the thread group.
            .mul(E1, g(2).ud(), g(0).ud().sub(1).scalar(), Imm::ud(0x10))
            // mov(1) ORIG_Y Y.
            .mov(E1, g(2).ud().sub(1), g(0).ud().sub(6).scalar())
            // mov(8) g4.0<1>UD g0.0<8,8,1>UD: header from g0.
            .mov(E8, g(4).ud(), g(0).ud())
            // mov(2) g4.0<1>UD ORIG.
            .mov(E2, g(4).ud(), g(2).ud().region(2, 2, 1))
            // mov(1) g4.8<1>UD 0xfUD: block of 16 bytes by 1 row.
            .mov(E1, g(4).ud().sub(2), Imm::ud(0xf))
            // mov(16) g5.0<1>UD COLORUD: the data.
            .mov(E16, g(5).ud(), g(1).ud().scalar())
            // send(16) 4 acc0<1>UW null write(0, 0, 10, 12) mlen 3 rlen 0.
            .emit(Inst::send(
                E16,
                Reg::acc0().uw(),
                g(4).d().scalar(),
                media_block_write,
            ))
            // mov(8) g112.0<1>UD g0.0<8,8,1>UD.
            .mov(E8, g(112).ud(), g(0).ud())
            // send(16) 112 null<1>UW null thread_spawner(0, 0, 1) mlen 1 {EOT}.
            .emit(Inst::send(E16, Reg::null().uw(), g(112).d().scalar(), eot));
    });
}
