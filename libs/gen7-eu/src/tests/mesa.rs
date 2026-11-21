// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Mesa's gen7 assembler test vectors: `src/intel/compiler/elk/tests/gen7/
//! *.asm` and `*.expected` (Copyright Intel Corporation, MIT licence),
//! instructions Mesa's compiler emitted for Ivy Bridge. The `.expected`
//! bytes are written here as little-endian dwords; the Mesa line is quoted
//! above each case.

use super::{assert_program, enc};
use crate::ExecSize::*;
use crate::{
    g, Chan, CondMod, Flag, Imm, Inst, Message, OWords, Pred, Qtr, Reg, Simd, Swizzle, WriteMask,
};

fn check(expected: [u32; 4], inst: Inst) {
    let got = enc(inst);
    assert_eq!(got, expected, "{got:08x?} != {expected:08x?} for {inst:?}");
}

#[test]
fn mad() {
    let f = |nr: u8| g(nr).f().vec4();
    let s = |nr: u8, sub: u8| g(nr).f().sub(sub).scalar();
    // mad(8) g11<1>F g4.7<0,1,0>F g4.3<0,1,0>F g9<4,4,1>F { align16 1Q };
    check(
        [0x0060015b, 0x0b1e0000, 0xc0204e01, 0x02472008],
        Inst::mad(E8, g(11).f(), s(4, 7), s(4, 3), f(9)),
    );
    // mad(8) g25<1>F g6.7<0,1,0>F g6.3<0,1,0>F g21<4,4,1>F { align16 2Q };
    check(
        [0x0060115b, 0x191e0000, 0xc0206e01, 0x0547200c],
        Inst::mad(E8, g(25).f(), s(6, 7), s(6, 3), f(21)).qtr(Qtr::Q2),
    );
    // mad(8) g3<1>F -g2.4<0,1,0>F g6.0<0,1,0>F g2.0<0,1,0>F { align16 2Q };
    check(
        [0x0060115b, 0x031e0020, 0x00202801, 0x0080040c],
        Inst::mad(E8, g(3).f(), s(2, 4).negate(), s(6, 0), s(2, 0)).qtr(Qtr::Q2),
    );
    // mad.le.f0.0(8) g9<1>F g3<4,4,1>F g4.2<0,1,0>F g15<4,4,1>F { align16 1Q };
    check(
        [0x0660015b, 0x091e0000, 0x802031c8, 0x03c72008],
        Inst::mad(E8, g(9).f(), f(3), s(4, 2), f(15)).cmod(CondMod::LE, Flag::F0_0),
    );
    // mad(8) g53<1>F -g52<4,4,1>F g21<4,4,1>F -g21<4,4,1>F { align16 1Q };
    check(
        [0x0060015b, 0x351e0220, 0x390341c8, 0x0547202a],
        Inst::mad(E8, g(53).f(), f(52).negate(), f(21), f(21).negate()),
    );
    // mad.sat(8) g40<1>F g39<4,4,1>F g37<4,4,1>F g10<4,4,1>F { align16 1Q };
    check(
        [0x8060015b, 0x281e0000, 0x390271c8, 0x0287204a],
        Inst::mad(E8, g(40).f(), f(39), f(37), f(10)).sat(),
    );
    // mad(8) g2<1>F -g6<4,4,1>F (abs)g5<4,4,1>F g14.0<0,1,0>F { align16 1Q };
    check(
        [0x0060015b, 0x021e0060, 0x390061c8, 0x0380040a],
        Inst::mad(E8, g(2).f(), f(6).negate(), f(5).abs(), s(14, 0)),
    );
    // mad(8) g13<1>F g51.2<0,1,0>F -g51.3<0,1,0>F (abs)g2.0<0,1,0>F { align16 1Q };
    check(
        [0x0060015b, 0x0d1e0180, 0xc0233401, 0x00800466],
        Inst::mad(E8, g(13).f(), s(51, 2), s(51, 3).negate(), s(2, 0).abs()),
    );
    // mad.nz.f0.0(8) g16<1>F -g33.0<0,1,0>F g10<4,4,1>F g18<4,4,1>F { align16 2Q };
    check(
        [0x0260115b, 0x101e0020, 0x39021001, 0x04872014],
        Inst::mad(E8, g(16).f(), s(33, 0).negate(), f(10), f(18))
            .cmod(CondMod::NZ, Flag::F0_0)
            .qtr(Qtr::Q2),
    );
}

#[test]
fn mad_with_writemask_and_swizzle() {
    let xyzz = Swizzle::new(Chan::X, Chan::Y, Chan::Z, Chan::Z);
    let v = |nr: u8| g(nr).f().vec4().swizzle(xyzz);
    // mad(8) g18<1>.xyzF -g16<4,4,1>.xyzzF g11<4,4,1>.xyzzF g9<4,4,1>.xyzzF { align16 1Q };
    check(
        [0x0060015b, 0x120e0020, 0x29010148, 0x02452016],
        Inst::mad(
            E8,
            g(18)
                .f()
                .writemask(WriteMask::X | WriteMask::Y | WriteMask::Z),
            v(16).negate(),
            v(11),
            v(9),
        ),
    );
}

#[test]
fn mov() {
    // mov(8) g126<1>F g4<8,8,1>D { align1 1Q };
    check(
        [0x00600001, 0x2fc000bd, 0x008d0080, 0x00000000],
        Inst::mov(E8, g(126).f(), g(4).d()),
    );
    // mov(16) g120<1>F g124<8,8,1>F { align1 1H };
    check(
        [0x00800001, 0x2f0003bd, 0x008d0f80, 0x00000000],
        Inst::mov(E16, g(120).f(), g(124).f()),
    );
    // mov(8) g114<1>D 0D { align16 1Q };
    check(
        [0x00600101, 0x2e4f10e5, 0x00000000, 0x00000000],
        Inst::mov(E8, g(114).d(), Imm::d(0)).align16(),
    );
    // mov(8) g115<1>F 0x41880000F /* 17F */ { align16 1Q };
    check(
        [0x00600101, 0x2e6f73fd, 0x00000000, 0x41880000],
        Inst::mov(E8, g(115).f(), Imm::f(17.0)).align16(),
    );
    // mov(8) g113<1>UD g0<4>UD { align16 WE_all 1Q };
    check(
        [0x00600301, 0x2e2f0021, 0x006e0004, 0x00000000],
        Inst::mov(E8, g(113).ud(), g(0).ud().vec4())
            .align16()
            .no_mask(),
    );
    // mov.sat(8) g116<1>F -g6<4>D { align16 1Q };
    check(
        [0x80600101, 0x2e8f00bd, 0x006e40c4, 0x00000000],
        Inst::mov(E8, g(116).f(), g(6).d().vec4().negate())
            .align16()
            .sat(),
    );
    // mov(4) g114<1>F g2.3<8,2,4>F { align1 WE_all 1N };
    check(
        [0x00400201, 0x2e4003bd, 0x0087004c, 0x00000000],
        Inst::mov(E4, g(114).f(), g(2).f().sub(3).region(8, 2, 4)).no_mask(),
    );
    // mov(8) g2<1>F g6<8,4,1>UW { align1 1Q };
    check(
        [0x00600001, 0x2040013d, 0x008900c0, 0x00000000],
        Inst::mov(E8, g(2).f(), g(6).uw().region(8, 4, 1)),
    );
    // mov(8) g23<1>F g6<0,1,0>F { align1 2Q };
    check(
        [0x00601001, 0x22e003bd, 0x000000c0, 0x00000000],
        Inst::mov(E8, g(23).f(), g(6).f().scalar()).qtr(Qtr::Q2),
    );
    // mov(1) g21.2<1>UD 0x000003f2UD { align1 WE_all 3N };
    check(
        [0x00001201, 0x22a80061, 0x00000000, 0x000003f2],
        Inst::mov(E1, g(21).ud().sub(2), Imm::ud(0x3f2))
            .no_mask()
            .qtr(Qtr::Q2),
    );
    // mov(2) g113.4<1>UW g12<8,1,0>UW { align1 WE_all 1N };
    check(
        [0x00200201, 0x2e280129, 0x00800180, 0x00000000],
        Inst::mov(E2, g(113).uw().sub(4), g(12).uw().region(8, 1, 0)).no_mask(),
    );
    // mov(1) g123.14<1>UW f0.1<0,1,0>UW { align1 WE_all 1N };
    check(
        [0x00000201, 0x2f7c0109, 0x00000602, 0x00000000],
        Inst::mov(E1, g(123).uw().sub(14), Reg::f0().uw().sub(1).scalar()).no_mask(),
    );
    // mov(1) f1<1>UD g1.7<0,1,0>UD { align1 WE_all 1N };
    check(
        [0x00000201, 0x26200020, 0x0000003c, 0x00000000],
        Inst::mov(E1, Reg::f1().ud(), g(1).ud().sub(7).scalar()).no_mask(),
    );
    // mov.nz.f0.0(16) null<1>D g89<8,8,1>D { align1 1H };
    check(
        [0x02800001, 0x200000a4, 0x008d0b20, 0x00000000],
        Inst::mov(E16, Reg::null().d(), g(89).d()).cmod(CondMod::NZ, Flag::F0_0),
    );
    // mov(8) g27<1>UD g24.2<32,8,4>UB { align1 1Q };
    check(
        [0x00600001, 0x23600221, 0x00cf0302, 0x00000000],
        Inst::mov(E8, g(27).ud(), g(24).ub().sub(2).region(32, 8, 4)),
    );
    // mov(8) g3<1>D -g2<0,1,0>D { align1 1Q };
    check(
        [0x00600001, 0x206000a5, 0x00004040, 0x00000000],
        Inst::mov(E8, g(3).d(), g(2).d().scalar().negate()),
    );
    // mov(8) g2<1>D 0x00000000UD { align1 1Q };
    check(
        [0x00600001, 0x20400065, 0x00000000, 0x00000000],
        Inst::mov(E8, g(2).d(), Imm::ud(0)),
    );
}

#[test]
fn add() {
    // add(1) g11.4<1>UD g11<0,1,0>UD 0x00000001UD { align1 1N };
    check(
        [0x00000040, 0x21700c21, 0x00000160, 0x00000001],
        Inst::add(E1, g(11).ud().sub(4), g(11).ud().scalar(), Imm::ud(1)),
    );
    // add(8) g17<1>F g6<0>F g7.4<0>F { align16 1Q };
    check(
        [0x00600140, 0x222f77bd, 0x000e00c4, 0x000e00f4],
        Inst::add(
            E8,
            g(17).f(),
            g(6).f().vstride(0),
            g(7).f().sub(4).vstride(0),
        )
        .align16(),
    );
    // add(8) g46<1>F g42<8,8,1>F -g4.4<0,1,0>F { align1 1Q };
    check(
        [0x00600040, 0x25c077bd, 0x008d0540, 0x00004090],
        Inst::add(E8, g(46).f(), g(42).f(), g(4).f().sub(4).scalar().negate()),
    );
    // add(16) g12<1>F g9<8,8,1>F 0xbd4ccccdF /* -0.05F */ { align1 1H };
    check(
        [0x00800040, 0x21807fbd, 0x008d0120, 0xbd4ccccd],
        Inst::add(E16, g(12).f(), g(9).f(), Imm::f(-0.05)),
    );
    // (+f0.0) add(8) g16<1>D -g16<4>D 31D { align16 1Q };
    check(
        [0x00610140, 0x220f1ca5, 0x006e4204, 0x0000001f],
        Inst::add(E8, g(16).d(), g(16).d().vec4().negate(), Imm::d(31))
            .align16()
            .pred(Pred::new(Flag::F0_0)),
    );
    // add(8) g4.1<2>UW g4.1<16,8,2>UW g16<16,8,2>UW { align1 1Q };
    check(
        [0x00600040, 0x40822529, 0x00ae0082, 0x00ae0200],
        Inst::add(
            E8,
            g(4).uw().sub(1).stride(2),
            g(4).uw().sub(1).region(16, 8, 2),
            g(16).uw().region(16, 8, 2),
        ),
    );
    // add.sat(16) g120<1>F g2<8,8,1>F g18<8,8,1>F { align1 1H };
    check(
        [0x80800040, 0x2f0077bd, 0x008d0040, 0x008d0240],
        Inst::add(E16, g(120).f(), g(2).f(), g(18).f()).sat(),
    );
}

#[test]
fn logic_and_shift() {
    // and(1) g12.2<1>UD g0.2<0,1,0>UD 0x0000f000UD { align1 WE_all 1N };
    check(
        [0x00000205, 0x21880c21, 0x00000008, 0x0000f000],
        Inst::and(
            E1,
            g(12).ud().sub(2),
            g(0).ud().sub(2).scalar(),
            Imm::ud(0xf000),
        )
        .no_mask(),
    );
    // and.nz.f0.0(8) null<1>UD g24<8,8,1>UD g25<8,8,1>UD { align1 1Q };
    check(
        [0x02600005, 0x20000420, 0x008d0300, 0x008d0320],
        Inst::and(E8, Reg::null().ud(), g(24).ud(), g(25).ud()).cmod(CondMod::NZ, Flag::F0_0),
    );
    // and(16) g20<1>UD g0.1<0,1,0>UW 0x07ffUW { align1 1H };
    check(
        [0x00800005, 0x22802d21, 0x00000002, 0x07ff07ff],
        Inst::and(E16, g(20).ud(), g(0).uw().sub(1).scalar(), Imm::uw(0x07ff)),
    );
    // (+f0.0) or(16) g8<1>UD g8<8,8,1>UD 0x3f800000UD { align1 1H };
    check(
        [0x00810006, 0x21000c21, 0x008d0100, 0x3f800000],
        Inst::or(E16, g(8).ud(), g(8).ud(), Imm::ud(0x3f80_0000)).pred(Pred::new(Flag::F0_0)),
    );
    // or.nz.f0.0(16) null<1>UD g20<8,8,1>UD g26<8,8,1>UD { align1 1H };
    check(
        [0x02800006, 0x20000420, 0x008d0280, 0x008d0340],
        Inst::or(E16, Reg::null().ud(), g(20).ud(), g(26).ud()).cmod(CondMod::NZ, Flag::F0_0),
    );
    // or(1) g113.21<1>UB g59<0,1,0>UB g59.16<0,1,0>UB { align1 WE_all 1N };
    check(
        [0x00000206, 0x2e354631, 0x00000760, 0x00000770],
        Inst::or(
            E1,
            g(113).ub().sub(21),
            g(59).ub().scalar(),
            g(59).ub().sub(16).scalar(),
        )
        .no_mask(),
    );
    // shl(1) g12.2<1>UD g12.2<0,1,0>UD 0x0000000cUD { align1 WE_all 1N };
    check(
        [0x00000209, 0x21880c21, 0x00000188, 0x0000000c],
        Inst::shl(
            E1,
            g(12).ud().sub(2),
            g(12).ud().sub(2).scalar(),
            Imm::ud(12),
        )
        .no_mask(),
    );
    // shl(16) g68<1>D g23<8,8,1>D g7.6<0,1,0>UD { align1 1H };
    check(
        [0x00800009, 0x288004a5, 0x008d02e0, 0x000000f8],
        Inst::shl(E16, g(68).d(), g(23).d(), g(7).ud().sub(6).scalar()),
    );
}

#[test]
fn mul() {
    // mul(16) g120<1>F g6<8,8,1>F g10<8,8,1>F { align1 1H };
    check(
        [0x00800041, 0x2f0077bd, 0x008d00c0, 0x008d0140],
        Inst::mul(E16, g(120).f(), g(6).f(), g(10).f()),
    );
    // mul(8) g3<1>D g2<0,1,0>D 36W { align1 1Q };
    check(
        [0x00600041, 0x20603ca5, 0x00000040, 0x00240024],
        Inst::mul(E8, g(3).d(), g(2).d().scalar(), Imm::w(36)),
    );
    // mul(2) g113.3<1>UD g35<8,2,4>UD 0x0005UW { align1 WE_all 1N };
    check(
        [0x00200241, 0x2e2c2c21, 0x00870460, 0x00050005],
        Inst::mul(
            E2,
            g(113).ud().sub(3),
            g(35).ud().region(8, 2, 4),
            Imm::uw(5),
        )
        .no_mask(),
    );
    // mul.l.f0.0(16) g32<1>F g2<8,8,1>F 0x42700000F /* 60F */ { align1 1H };
    check(
        [0x05800041, 0x24007fbd, 0x008d0040, 0x42700000],
        Inst::mul(E16, g(32).f(), g(2).f(), Imm::f(60.0)).cmod(CondMod::L, Flag::F0_0),
    );
    // mul.sat(8) g116<1>F g6<4>F 0x3b800000F /* 0.00390625F */ { align16 1Q };
    check(
        [0x80600141, 0x2e8f7fbd, 0x006e00c4, 0x3b800000],
        Inst::mul(E8, g(116).f(), g(6).f().vec4(), Imm::f(0.003_906_25))
            .align16()
            .sat(),
    );
}

#[test]
fn cmp() {
    // cmp.ge.f0.0(8) g33<1>F g32<8,8,1>F 0x3189705fF /* 4e-09F */ { align1 1Q };
    check(
        [0x04600010, 0x24207fbd, 0x008d0400, 0x3189705f],
        Inst::cmp(
            E8,
            CondMod::GE,
            Flag::F0_0,
            g(33).f(),
            g(32).f(),
            Imm::f(4e-9),
        ),
    );
    // cmp.l.f0.0(8) g6<1>F g4<8,8,1>F g51<0,1,0>F { align1 2Q };
    check(
        [0x05601010, 0x20c077bd, 0x008d0080, 0x00000660],
        Inst::cmp(
            E8,
            CondMod::L,
            Flag::F0_0,
            g(6).f(),
            g(4).f(),
            g(51).f().scalar(),
        )
        .qtr(Qtr::Q2),
    );
    // cmp.l.f0.0(8) g10<1>UD g9<4>UD g1<0>UD { align16 1Q };
    check(
        [0x05600110, 0x214f0421, 0x006e0124, 0x000e0024],
        Inst::cmp(
            E8,
            CondMod::L,
            Flag::F0_0,
            g(10).ud(),
            g(9).ud().vec4(),
            g(1).ud().vstride(0),
        )
        .align16(),
    );
    // cmp.le.f0.0(8) g33<1>F g31<8,8,1>F 0x3727c5acF /* 1e-05F */ { align1 1Q };
    check(
        [0x06600010, 0x24207fbd, 0x008d03e0, 0x3727c5ac],
        Inst::cmp(
            E8,
            CondMod::LE,
            Flag::F0_0,
            g(33).f(),
            g(31).f(),
            Imm::f(1e-5),
        ),
    );
}

#[test]
fn while_loops() {
    // LABEL0:
    // while(8)          JIP: LABEL0 { align1 1Q };
    // while(16)         JIP: LABEL0 { align1 1H };
    // while(8)          JIP: LABEL0 { align16 1Q };  (Align16: stood in for)
    // (-f0.0) while(8)  JIP: LABEL0 { align1 1Q };
    // (-f0.0) while(16) JIP: LABEL0 { align1 1H };
    let expected = [
        [0x00600027, 0x20003c84, 0x008d0000, 0x00000000],
        [0x00800027, 0x20003c84, 0x008d0000, 0x0000fffe],
        [0x00600127, 0x200f3c84, 0x006e0004, 0x0000fffc],
        [0x00710027, 0x20003c84, 0x008d0000, 0x0000fffa],
        [0x00910027, 0x20003c84, 0x008d0000, 0x0000fff8],
    ];
    let not_f0 = Some(Pred::new(Flag::F0_0).inverse());
    assert_program(&expected, &[2], |asm| {
        let start = asm.label();
        asm.while_(E8, None, start)
            .while_(E16, None, start)
            .nop()
            .while_(E8, not_f0, start)
            .while_(E16, not_f0, start);
    });
}

#[test]
fn send_untyped_surface() {
    let f1 = Pred::new(Flag::F1_0);
    let untyped_write =
        |bti, simd, channels| Message::untyped_surface_write(bti, simd, channels).unwrap();
    let untyped_read =
        |bti, simd, channels| Message::untyped_surface_read(bti, simd, channels).unwrap();
    let null = Reg::null().uw();
    // (+f1.0) send(16) null<1>UW g11<8,8,1>UD 0x14035003
    //     data MsgDesc: ( DC untyped surface write, 3, 16) mlen 10 rlen 0 { align1 1H };
    check(
        [0x0a810031, 0x20000c28, 0x048d0160, 0x14035003],
        Inst::send(E16, null, g(11).ud(), untyped_write(3, Simd::Simd16, 4)).pred(f1),
    );
    // (+f1.0) send(16) null<1>UW g4<8,8,1>UD 0x08035e01
    //     data MsgDesc: ( DC untyped surface write, 1, 30) mlen 4 rlen 0 { align1 1H };
    check(
        [0x0a810031, 0x20000c28, 0x048d0080, 0x08035e01],
        Inst::send(E16, null, g(4).ud(), untyped_write(1, Simd::Simd16, 1)).pred(f1),
    );
    // (+f1.0) send(8) null<1>UW g11<8,8,1>UD 0x0a036002
    //     data MsgDesc: ( DC untyped surface write, 2, 32) mlen 5 rlen 0 { align1 1Q };
    check(
        [0x0a610031, 0x20000c28, 0x048d0160, 0x0a036002],
        Inst::send(E8, null, g(11).ud(), untyped_write(2, Simd::Simd8, 4)).pred(f1),
    );
    // (+f1.0) send(8) null<1>UW g3<8,8,1>UD 0x04036e01
    //     data MsgDesc: ( DC untyped surface write, 1, 46) mlen 2 rlen 0 { align1 1Q };
    check(
        [0x0a610031, 0x20000c28, 0x048d0060, 0x04036e01],
        Inst::send(E8, null, g(3).ud(), untyped_write(1, Simd::Simd8, 1)).pred(f1),
    );
    // (+f1.0) send(8) null<1>UW g3<8,8,1>UD 0x06036c01
    //     data MsgDesc: ( DC untyped surface write, 1, 44) mlen 3 rlen 0 { align1 1Q };
    check(
        [0x0a610031, 0x20000c28, 0x048d0060, 0x06036c01],
        Inst::send(E8, null, g(3).ud(), untyped_write(1, Simd::Simd8, 2)).pred(f1),
    );
    // send(16) g120<1>UW g2<8,8,1>UD 0x04815001
    //     data MsgDesc: ( DC untyped surface read, 1, 16) mlen 2 rlen 8 { align1 1H };
    check(
        [0x0a800031, 0x2f000c29, 0x008d0040, 0x04815001],
        Inst::send(
            E16,
            g(120).uw(),
            g(2).ud(),
            untyped_read(1, Simd::Simd16, 4),
        ),
    );
    // send(16) g12<1>UW g40<8,8,1>UD 0x04215e02
    //     data MsgDesc: ( DC untyped surface read, 2, 30) mlen 2 rlen 2 { align1 1H };
    check(
        [0x0a800031, 0x21800c29, 0x008d0500, 0x04215e02],
        Inst::send(
            E16,
            g(12).uw(),
            g(40).ud(),
            untyped_read(2, Simd::Simd16, 1),
        ),
    );
    // send(8) g124<1>UW g2<8,8,1>UD 0x02416001
    //     data MsgDesc: ( DC untyped surface read, 1, 32) mlen 1 rlen 4 { align1 1Q };
    check(
        [0x0a600031, 0x2f800c29, 0x008d0040, 0x02416001],
        Inst::send(E8, g(124).uw(), g(2).ud(), untyped_read(1, Simd::Simd8, 4)),
    );
    // send(8) g2<1>UW g94<8,8,1>UD 0x02116e01
    //     data MsgDesc: ( DC untyped surface read, 1, 46) mlen 1 rlen 1 { align1 1Q };
    check(
        [0x0a600031, 0x20400c29, 0x008d0bc0, 0x02116e01],
        Inst::send(E8, g(2).uw(), g(94).ud(), untyped_read(1, Simd::Simd8, 1)),
    );
}

#[test]
fn send_oword_block_write() {
    // send(8) null<1>UW g126<8,8,1>UD 0x040a02ff
    //     data MsgDesc: ( DC OWORD block write, 255, 2) mlen 2 rlen 0 { align1 1Q };
    check(
        [0x0a600031, 0x20000c28, 0x008d0fc0, 0x040a02ff],
        Inst::send(
            E8,
            Reg::null().uw(),
            g(126).ud(),
            Message::oword_block_write(255, OWords::Two).unwrap(),
        ),
    );
}
