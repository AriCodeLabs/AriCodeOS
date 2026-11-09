// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Linux's Ivy Bridge clear kernel, which i915 runs on this GPU to wipe the
//! EU registers between contexts (CVE-2019-14615).
//!
//! The words are `ivb_clear_kernel` from Linux
//! `drivers/gpu/drm/i915/gt/shaders/clear_kernel/ivb.asm` and `ivb.c`
//! (Copyright 2020 Intel Corporation, MIT licence); the assembly is quoted
//! before each call. All 52 instructions are reproduced but one: slot 48,
//! `mov(16) g[a0]<1>UW`, is register-indirect, outside this assembler.

use super::assert_program;
use crate::ExecSize::*;
use crate::{g, CondMod, Flag, Imm, Inst, Message, Pred, Qtr, Reg};

const IVB_CLEAR_KERNEL: [[u32; 4]; 52] = [
    [0x00000001, 0x26020128, 0x00000024, 0x00000000],
    [0x00000040, 0x20280c21, 0x00000028, 0x00000001],
    [0x01000010, 0x20000c20, 0x0000002c, 0x00000000],
    [0x00010220, 0x34001c00, 0x00001400, 0x0000002c],
    [0x00600001, 0x20600061, 0x00000000, 0x00000000],
    [0x00000008, 0x20601c85, 0x00000e00, 0x0000000c],
    [0x00000005, 0x20601ca5, 0x00000060, 0x00000001],
    [0x00000008, 0x20641c85, 0x00000e00, 0x0000000d],
    [0x00000005, 0x20641ca5, 0x00000064, 0x00000003],
    [0x00000041, 0x207424a5, 0x00000064, 0x00000034],
    [0x00000040, 0x206014a5, 0x00000060, 0x00000074],
    [0x00000008, 0x20681c85, 0x00000e00, 0x00000008],
    [0x00000005, 0x20681ca5, 0x00000068, 0x0000000f],
    [0x00000041, 0x20701ca5, 0x00000060, 0x00000010],
    [0x00000040, 0x206814a5, 0x00000068, 0x00000070],
    [0x00600001, 0x20a00061, 0x00000000, 0x00000000],
    [0x00000005, 0x206c1c85, 0x00000e00, 0x00000007],
    [0x00000041, 0x206c1ca5, 0x0000006c, 0x00000004],
    [0x00600001, 0x20800021, 0x008d0000, 0x00000000],
    [0x00000001, 0x20800021, 0x0000006c, 0x00000000],
    [0x00000001, 0x20840021, 0x00000068, 0x00000000],
    [0x00000001, 0x20880061, 0x00000000, 0x00000003],
    [0x00000005, 0x208c0d21, 0x00000086, 0xffffffff],
    [0x05600032, 0x20a00fa1, 0x008d0080, 0x02190001],
    [0x00000040, 0x20a01ca5, 0x000000a0, 0x00000001],
    [0x05600032, 0x20a00fa1, 0x008d0080, 0x040a8001],
    [0x02000040, 0x20281c21, 0x00000028, 0xffffffff],
    [0x00010220, 0x34001c00, 0x00001400, 0xfffffffc],
    [0x00000001, 0x26020128, 0x00000024, 0x00000000],
    [0x00000001, 0x220010e4, 0x00000000, 0x00000000],
    [0x00000001, 0x220831ec, 0x00000000, 0x007f007f],
    [0x00600001, 0x20400021, 0x008d0000, 0x00000000],
    [0x00600001, 0x2fe00021, 0x008d0000, 0x00000000],
    [0x00200001, 0x20400121, 0x00450020, 0x00000000],
    [0x00000001, 0x20480061, 0x00000000, 0x000f000f],
    [0x00000005, 0x204c0d21, 0x00000046, 0xffffffef],
    [0x00800001, 0x20600061, 0x00000000, 0x00000000],
    [0x00800001, 0x20800061, 0x00000000, 0x00000000],
    [0x00800001, 0x20a00061, 0x00000000, 0x00000000],
    [0x00800001, 0x20c00061, 0x00000000, 0x00000000],
    [0x00800001, 0x20e00061, 0x00000000, 0x00000000],
    [0x00800001, 0x21000061, 0x00000000, 0x00000000],
    [0x00800001, 0x21200061, 0x00000000, 0x00000000],
    [0x00800001, 0x21400061, 0x00000000, 0x00000000],
    [0x05600032, 0x20000fa0, 0x008d0040, 0x120a8000],
    [0x00000040, 0x20402d21, 0x00000020, 0x00100010],
    [0x05600032, 0x20000fa0, 0x008d0040, 0x120a8000],
    [0x02000040, 0x22083d8c, 0x00000208, 0xffffffff],
    [0x00800001, 0xa0000109, 0x00000602, 0x00000000],
    [0x00000040, 0x22001c84, 0x00000200, 0x00000020],
    [0x00010220, 0x34001c00, 0x00001400, 0xfffffff8],
    [0x07600032, 0x20000fa0, 0x008d0fe0, 0x82000010],
];

/// The register-indirect `mov` the assembler does not write.
const INDIRECT_SLOT: usize = 48;

#[test]
fn linux_ivb_clear_kernel_bit_for_bit() {
    let f0 = Some(Pred::new(Flag::F0_0));
    let f0_1 = Reg::f0().uw().sub(1);
    let read = Message::media_block_read(1, 1).unwrap();
    let write = Message::media_block_write(1, 1).unwrap();
    let clear = Message::media_block_write(0, 8).unwrap();
    assert_program(&IVB_CLEAR_KERNEL, &[INDIRECT_SLOT], |asm| {
        // mov(1) f0.1<1>UW g1.2<0,1,0>UW: the "clear GRF" value.
        asm.mov(E1, f0_1, g(1).uw().sub(2).scalar())
            // add(1) g1.2<1>UD g1.2<0,1,0>UD 0x1UD: delay count + 1.
            .add(E1, g(1).ud().sub(2), g(1).ud().sub(2).scalar(), Imm::ud(1))
            // cmp.z.f0.0(1) null<1>UD g1.3<0,1,0>UD 0x0UD.
            .emit(Inst::cmp(
                E1,
                CondMod::Z,
                Flag::F0_0,
                Reg::null().ud(),
                g(1).ud().sub(3).scalar(),
                Imm::ud(0),
            ));
        // (+f0.0) jmpi(1) 44D: skip the instrumentation.
        let skip = asm.jmpi_forward(f0);
        // mov(8) g3<1>UD 0x0UD.
        asm.mov(E8, g(3).ud(), Imm::ud(0))
            // shr(1) g3<1>D sr0<0,1,0>D 12D; and(1) g3<1>D g3<0,1,0>D 1D.
            .shr(E1, g(3).d(), Reg::sr0().d().scalar(), Imm::d(12))
            .and(E1, g(3).d(), g(3).d().scalar(), Imm::d(1))
            // shr(1) g3.1<1>D sr0<0,1,0>D 13D; and(1) g3.1 g3.1 3D.
            .shr(E1, g(3).d().sub(1), Reg::sr0().d().scalar(), Imm::d(13))
            .and(E1, g(3).d().sub(1), g(3).d().sub(1).scalar(), Imm::d(3))
            // mul(1) g3.5<1>D g3.1<0,1,0>D g1.10<0,1,0>UW.
            .mul(
                E1,
                g(3).d().sub(5),
                g(3).d().sub(1).scalar(),
                g(1).uw().sub(10).scalar(),
            )
            // add(1) g3<1>D g3<0,1,0>D g3.5<0,1,0>D.
            .add(E1, g(3).d(), g(3).d().scalar(), g(3).d().sub(5).scalar())
            // shr(1) g3.2<1>D sr0<0,1,0>D 8D; and(1) g3.2 g3.2 15D.
            .shr(E1, g(3).d().sub(2), Reg::sr0().d().scalar(), Imm::d(8))
            .and(E1, g(3).d().sub(2), g(3).d().sub(2).scalar(), Imm::d(15))
            // mul(1) g3.4<1>D g3<0,1,0>D 16D.
            .mul(E1, g(3).d().sub(4), g(3).d().scalar(), Imm::d(16))
            // add(1) g3.2<1>D g3.2<0,1,0>D g3.4<0,1,0>D.
            .add(
                E1,
                g(3).d().sub(2),
                g(3).d().sub(2).scalar(),
                g(3).d().sub(4).scalar(),
            )
            // mov(8) g5<1>UD 0x0UD.
            .mov(E8, g(5).ud(), Imm::ud(0))
            // and(1) g3.3<1>D sr0<0,1,0>D 7D; mul(1) g3.3 g3.3 4D.
            .and(E1, g(3).d().sub(3), Reg::sr0().d().scalar(), Imm::d(7))
            .mul(E1, g(3).d().sub(3), g(3).d().sub(3).scalar(), Imm::d(4))
            // mov(8) g4<1>UD g0<8,8,1>UD: message header from g0.
            .mov(E8, g(4).ud(), g(0).ud())
            // mov(1) g4<1>UD g3.3<0,1,0>UD; mov(1) g4.1<1>UD g3.2<0,1,0>UD.
            .mov(E1, g(4).ud(), g(3).ud().sub(3).scalar())
            .mov(E1, g(4).ud().sub(1), g(3).ud().sub(2).scalar())
            // mov(1) g4.2<1>UD 0x3UD: 1 row of 4 bytes.
            .mov(E1, g(4).ud().sub(2), Imm::ud(3))
            // and(1) g4.3<1>UD g4.3<0,1,0>UW 0xffffffffUD.
            .and(
                E1,
                g(4).ud().sub(3),
                g(4).uw().sub(3).scalar(),
                Imm::ud(u32::MAX),
            )
            // sendc(8) g5<1>UD g4<8,8,1>F 0x02190001: media block read.
            .emit(Inst::sendc(E8, g(5).ud(), g(4).f(), read))
            // add(1) g5<1>D g5<0,1,0>D 1D.
            .add(E1, g(5).d(), g(5).d().scalar(), Imm::d(1))
            // sendc(8) g5<1>UD g4<8,8,1>F 0x040a8001: media block write.
            .emit(Inst::sendc(E8, g(5).ud(), g(4).f(), write))
            .bind(skip);
        // add.nz.f0.0(1) g1.2<1>UD g1.2<0,1,0>UD -1D: the delay loop.
        let delay = asm.label();
        asm.emit(
            Inst::add(E1, g(1).ud().sub(2), g(1).ud().sub(2).scalar(), Imm::d(-1))
                .cmod(CondMod::NZ, Flag::F0_0),
        )
        // (+f0.0) jmpi(1) -4D.
        .jmpi(f0, delay)
        // mov(1) f0.1<1>UW g1.2<0,1,0>UW.
        .mov(E1, f0_1, g(1).uw().sub(2).scalar())
        // mov(1) a0<1>D 0D; mov(1) a0.4<1>W 127W: GRF loop count.
        .mov(E1, Reg::a0().d(), Imm::d(0))
        .mov(E1, Reg::a0().w().sub(4), Imm::w(127))
        // mov(8) g2<1>UD g0<8,8,1>UD; mov(8) g127<1>UD g0<8,8,1>UD.
        .mov(E8, g(2).ud(), g(0).ud())
        .mov(E8, g(127).ud(), g(0).ud())
        // mov(2) g2<1>UD g1<2,2,1>UW.
        .mov(E2, g(2).ud(), g(1).uw().region(2, 2, 1))
        // mov(1) g2.2<1>UD 0x000f000fUD: a 16x16 block.
        .mov(E1, g(2).ud().sub(2), Imm::ud(0x000f_000f))
        // and(1) g2.3<1>UD g2.3<0,1,0>UW 0xffffffefUD.
        .and(
            E1,
            g(2).ud().sub(3),
            g(2).uw().sub(3).scalar(),
            Imm::ud(0xffff_ffef),
        );
        // mov(16) g3..g10<1>UD 0x0UD {1H}: eight zeroed pairs.
        for reg in 3..=10 {
            asm.emit(Inst::mov(E16, g(reg).ud(), Imm::ud(0)).qtr(Qtr::H1));
        }
        // sendc(8) null<1>UD g2<8,8,1>F 0x120a8000: media block write.
        asm.emit(Inst::sendc(E8, Reg::null().ud(), g(2).f(), clear))
            // add(1) g2<1>UD g1<0,1,0>UW 0x0010UW.
            .add(E1, g(2).ud(), g(1).uw().scalar(), Imm::uw(0x10))
            .emit(Inst::sendc(E8, Reg::null().ud(), g(2).f(), clear));
        // add.nz.f0.0(1) a0.4<1>W a0.4<0,1,0>W -1W.
        let clear_loop = asm.label();
        asm.emit(
            Inst::add(
                E1,
                Reg::a0().w().sub(4),
                Reg::a0().w().sub(4).scalar(),
                Imm::w(-1),
            )
            .cmod(CondMod::NZ, Flag::F0_0),
        )
        // mov(16) g[a0]<1>UW f0.1<0,1,0>UW: register-indirect, stood in for.
        .nop()
        // add(1) a0<1>D a0<0,1,0>D 32D.
        .add(E1, Reg::a0().d(), Reg::a0().d().scalar(), Imm::d(32))
        // (+f0.0) jmpi(1) -8D.
        .jmpi(f0, clear_loop)
        // sendc(8) null<1>UD g127<8,8,1>F thread_spawner mlen 1 {EOT}.
        .emit(Inst::sendc(
            E8,
            Reg::null().ud(),
            g(127).f(),
            Message::thread_spawner_eot(),
        ));
    });
}
