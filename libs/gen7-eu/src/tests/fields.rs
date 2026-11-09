// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Each field on its own: an instruction is encoded twice, differing in
//! one field, and only that field's bits may change. Bit numbers are the
//! PRM's (Vol 4 Part 3, 5.2.1).

use super::{assert_field, bits, enc};
use crate::ExecSize::*;
use crate::{
    g, Chan, CondMod, Flag, Imm, Inst, Message, OWords, Pred, PredCtrl, Qtr, Reg, Simd, Swizzle,
    WriteMask,
};

/// add(8) g2<1>UD g3<8,8,1>UD g4<8,8,1>UD.
fn add() -> Inst {
    Inst::add(E8, g(2).ud(), g(3).ud(), g(4).ud())
}

fn add_with(dst: Reg, src0: Reg, src1: Reg) -> Inst {
    Inst::add(E8, dst, src0, src1)
}

#[test]
fn opcode_is_bits_6_to_0() {
    let mul = Inst::mul(E8, g(2).ud(), g(3).ud(), g(4).ud());
    assert_field(enc(add()), enc(mul), 6, 0, 0x41);
    assert_eq!(bits(enc(add()), 6, 0), 0x40);
}

#[test]
fn access_mode_is_bit_8() {
    assert_eq!(bits(enc(add()), 8, 8), 0);
    let a16 = Inst::add(E8, g(2).ud(), g(3).ud().vec4(), g(4).ud().vec4()).align16();
    assert_eq!(bits(enc(a16), 8, 8), 1);
}

#[test]
fn mask_control_is_bit_9() {
    assert_field(enc(add()), enc(add().no_mask()), 9, 9, 1);
}

#[test]
fn quarter_control_is_bits_13_to_12() {
    assert_field(enc(add()), enc(add().qtr(Qtr::Q2)), 13, 12, 1);
    assert_field(enc(add()), enc(add().qtr(Qtr::Q3)), 13, 12, 2);
    assert_field(enc(add()), enc(add().qtr(Qtr::Q4)), 13, 12, 3);
    let wide = Inst::add(E16, g(2).ud(), g(4).ud(), g(6).ud());
    assert_field(enc(wide), enc(wide.qtr(Qtr::H2)), 13, 12, 2);
    assert_eq!(enc(wide.qtr(Qtr::H1)), enc(wide));
}

#[test]
fn predicate_control_and_inverse_are_bits_19_to_16_and_20() {
    let plain = add().pred(Pred::new(Flag::F0_0));
    assert_field(enc(add()), enc(plain), 19, 16, 1);
    let any8 = add().pred(Pred::new(Flag::F0_0).ctrl(PredCtrl::Any8H));
    assert_field(enc(add()), enc(any8), 19, 16, 8);
    let all16 = add().pred(Pred::new(Flag::F0_0).ctrl(PredCtrl::All16H));
    assert_field(enc(add()), enc(all16), 19, 16, 11);
    let inverse = add().pred(Pred::new(Flag::F0_0).inverse());
    assert_field(enc(plain), enc(inverse), 20, 20, 1);
}

#[test]
fn flag_register_is_bits_90_and_89() {
    let f0_0 = enc(add().pred(Pred::new(Flag::F0_0)));
    assert_field(f0_0, enc(add().pred(Pred::new(Flag::F0_1))), 90, 89, 1);
    assert_field(f0_0, enc(add().pred(Pred::new(Flag::F1_0))), 90, 89, 2);
    assert_field(f0_0, enc(add().pred(Pred::new(Flag::F1_1))), 90, 89, 3);
}

#[test]
fn exec_size_is_bits_23_to_21() {
    let codes = [(E1, 0), (E2, 1), (E4, 2), (E8, 3), (E16, 4)];
    for (exec, code) in codes {
        let inst = Inst::mov(exec, g(2).ud(), Imm::ud(0));
        assert_eq!(bits(enc(inst), 23, 21), code, "{exec:?}");
    }
}

#[test]
fn cond_modifier_is_bits_27_to_24_with_its_flag() {
    let codes = [
        (CondMod::Z, 1),
        (CondMod::NZ, 2),
        (CondMod::G, 3),
        (CondMod::GE, 4),
        (CondMod::L, 5),
        (CondMod::LE, 6),
        (CondMod::O, 8),
        (CondMod::U, 9),
    ];
    for (cmod, code) in codes {
        assert_field(enc(add()), enc(add().cmod(cmod, Flag::F0_0)), 27, 24, code);
    }
    let on_f1_1 = enc(add().cmod(CondMod::Z, Flag::F1_1));
    assert_field(enc(add().cmod(CondMod::Z, Flag::F0_0)), on_f1_1, 90, 89, 3);
}

#[test]
fn saturate_is_bit_31() {
    let mul = Inst::mul(E8, g(2).f(), g(3).f(), g(4).f());
    assert_field(enc(mul), enc(mul.sat()), 31, 31, 1);
}

#[test]
fn types_are_bits_36_to_34_41_to_39_46_to_44() {
    let types = [
        (Reg::ud as fn(Reg) -> Reg, 0),
        (Reg::d, 1),
        (Reg::uw, 2),
        (Reg::w, 3),
        (Reg::ub, 4),
        (Reg::b, 5),
        (Reg::f, 7),
    ];
    // Four channels with a destination stride of 4 suit every type.
    let src = g(3).ud().vec4();
    let base = enc(Inst::mov(E4, g(2).ud().stride(4), src));
    for (ty, code) in types {
        let dst = Inst::mov(E4, ty(g(2)).stride(4), src);
        assert_field(base, enc(dst), 36, 34, code);
        let src0 = Inst::mov(E4, g(2).ud().stride(4), ty(g(3)).vec4());
        assert_field(base, enc(src0), 41, 39, code);
    }
    let base = enc(Inst::mul(E8, g(2).d(), g(3).d(), g(4).ud()));
    let src1 = enc(Inst::mul(E8, g(2).d(), g(3).d(), g(4).w()));
    assert_field(base, src1, 46, 44, 3);
}

#[test]
fn register_files_are_bits_33_to_32_38_to_37_43_to_42() {
    let base = enc(Inst::mov(E8, g(0).ud(), g(0).ud()));
    let null_dst = enc(Inst::mov(E8, Reg::null().ud(), g(0).ud()));
    assert_field(base, null_dst, 33, 32, 0);
    assert_eq!(bits(base, 33, 32), 1);
    let null_src = enc(Inst::mov(E8, g(0).ud(), Reg::null().ud()));
    assert_field(base, null_src, 38, 37, 0);
    assert_eq!(bits(base, 38, 37), 1);
    let imm = enc(Inst::add(E8, g(2).ud(), g(3).ud(), Imm::ud(0)));
    assert_eq!(bits(imm, 43, 42), 3);
    assert_eq!(bits(enc(add()), 43, 42), 1);
}

#[test]
fn destination_region_is_bits_62_to_48() {
    let base = enc(add());
    let reg = add_with(g(100).ud(), g(3).ud(), g(4).ud());
    assert_field(base, enc(reg), 60, 53, 100);
    let sub = add_with(g(2).ud().sub(3), g(3).ud(), g(4).ud());
    assert_field(base, enc(sub), 52, 48, 12);
    let stride = add_with(g(2).ud().stride(2), g(3).ud(), g(4).ud());
    assert_field(base, enc(stride), 62, 61, 2);
    let stride4 = Inst::add(
        E4,
        g(2).ud().stride(4),
        g(3).ud().region(4, 4, 1),
        Imm::ud(0),
    );
    assert_eq!(bits(enc(stride4), 62, 61), 3);
}

#[test]
fn arf_numbers_name_the_register_kind() {
    let dst = |reg: Reg| bits(enc(Inst::mov(E1, reg, Imm::ud(0))), 60, 53);
    assert_eq!(dst(Reg::null().ud()), 0x00);
    assert_eq!(dst(Reg::a0().ud()), 0x10);
    assert_eq!(dst(Reg::acc0().ud()), 0x20);
    assert_eq!(dst(Reg::f0().ud()), 0x30);
    assert_eq!(dst(Reg::f1().ud()), 0x31);
    assert_eq!(dst(Reg::sr0().ud()), 0x70);
    assert_eq!(dst(Reg::ip().ud()), 0xA0);
}

#[test]
fn src0_is_dword_2() {
    let base = enc(add());
    let reg = add_with(g(2).ud(), g(77).ud(), g(4).ud());
    assert_field(base, enc(reg), 76, 69, 77);
    let sub = add_with(g(2).ud(), g(3).ud().sub(5).region(0, 1, 0), g(4).ud());
    assert_eq!(bits(enc(sub), 68, 64), 20);
    let abs = add_with(g(2).d(), g(3).d().abs(), g(4).d());
    let neg = add_with(g(2).d(), g(3).d().negate(), g(4).d());
    let both = add_with(g(2).d(), g(3).d().negate().abs(), g(4).d());
    let plain = enc(add_with(g(2).d(), g(3).d(), g(4).d()));
    assert_field(plain, enc(abs), 78, 77, 1);
    assert_field(plain, enc(neg), 78, 77, 2);
    assert_field(plain, enc(both), 78, 77, 3);
    let region = add_with(g(2).ud(), g(3).uw().region(16, 8, 2), g(4).ud());
    let r = enc(region);
    assert_eq!(
        (bits(r, 88, 85), bits(r, 84, 82), bits(r, 81, 80)),
        (5, 3, 2)
    );
    assert_eq!(
        (bits(base, 88, 85), bits(base, 84, 82), bits(base, 81, 80)),
        (4, 3, 1)
    );
}

#[test]
fn src1_is_dword_3() {
    let base = enc(add());
    let reg = add_with(g(2).ud(), g(3).ud(), g(99).ud());
    assert_field(base, enc(reg), 108, 101, 99);
    let sub = add_with(g(2).ud(), g(3).ud(), g(4).ud().sub(7).region(0, 1, 0));
    assert_eq!(bits(enc(sub), 100, 96), 28);
    let plain = enc(add_with(g(2).d(), g(3).d(), g(4).d()));
    let abs = add_with(g(2).d(), g(3).d(), g(4).d().abs());
    let neg = add_with(g(2).d(), g(3).d(), g(4).d().negate());
    assert_field(plain, enc(abs), 110, 109, 1);
    assert_field(plain, enc(neg), 110, 109, 2);
    let region = add_with(g(2).ud(), g(3).ud(), g(4).ud().region(4, 4, 1));
    let r = enc(region);
    assert_eq!(
        (bits(r, 120, 117), bits(r, 116, 114), bits(r, 113, 112)),
        (3, 2, 1)
    );
}

#[test]
fn immediates_fill_dword_3() {
    let inst = |imm: Imm| enc(Inst::add(E8, g(2).ud(), g(3).ud(), imm));
    let ud = inst(Imm::ud(0x1234_5678));
    assert_eq!(ud[3], 0x1234_5678);
    assert_eq!(bits(ud, 46, 44), 0);
    let d = inst(Imm::d(-2));
    assert_eq!((d[3], bits(d, 46, 44)), (0xffff_fffe, 1));
    let uw = inst(Imm::uw(0xabcd));
    assert_eq!((uw[3], bits(uw, 46, 44)), (0xabcd_abcd, 2));
    let w = inst(Imm::w(-3));
    assert_eq!((w[3], bits(w, 46, 44)), (0xfffd_fffd, 3));
    let f = enc(Inst::add(E8, g(2).f(), g(3).f(), Imm::f(1.0)));
    assert_eq!((f[3], bits(f, 46, 44)), (0x3f80_0000, 7));
}

#[test]
fn a_lone_immediate_lends_its_type_to_src1() {
    let mov = enc(Inst::mov(E1, Reg::a0().w(), Imm::w(5)));
    assert_eq!(bits(mov, 38, 37), 3, "src0 file");
    assert_eq!(bits(mov, 41, 39), 3, "src0 type");
    assert_eq!(bits(mov, 43, 42), 0, "src1 file");
    assert_eq!(bits(mov, 46, 44), 3, "src1 type");
    let reg = enc(Inst::mov(E8, g(2).w().stride(2), g(3).w().region(8, 8, 1)));
    assert_eq!(bits(reg, 46, 42), 0, "no src1 behind a register");
}

#[test]
fn align16_destination_has_writemask_and_bit_4_of_the_offset() {
    let a16 = |dst: Reg| enc(Inst::mov(E8, dst, g(3).f().vec4()).align16());
    let base = a16(g(2).f());
    assert_eq!(bits(base, 51, 48), 0xF);
    assert_eq!(bits(base, 62, 61), 1, "hstride programmed as 1");
    assert_field(
        base,
        a16(g(2).f().writemask(WriteMask::Y | WriteMask::W)),
        51,
        48,
        0xA,
    );
    assert_field(base, a16(g(2).f().sub(4)), 52, 52, 1);
}

#[test]
fn align16_source_has_swizzle_in_two_nibbles() {
    let wzyx = Swizzle::new(Chan::W, Chan::Z, Chan::Y, Chan::X);
    let a16 = |src0: Reg, src1: Reg| enc(Inst::add(E8, g(2).f(), src0, src1).align16());
    let base = a16(g(3).f().vec4(), g(4).f().vec4());
    assert_eq!((bits(base, 67, 64), bits(base, 83, 80)), (0x4, 0xE), "xyzw");
    let s0 = a16(g(3).f().vec4().swizzle(wzyx), g(4).f().vec4());
    assert_eq!((bits(s0, 67, 64), bits(s0, 83, 80)), (0xB, 0x1));
    let s1 = a16(g(3).f().vec4(), g(4).f().vec4().swizzle(wzyx));
    assert_eq!((bits(s1, 99, 96), bits(s1, 115, 112)), (0xB, 0x1));
    let sub0 = a16(g(3).f().vec4().sub(4), g(4).f().vec4());
    assert_field(base, sub0, 68, 68, 1);
    let sub1 = a16(g(3).f().vec4(), g(4).f().vec4().sub(4));
    assert_field(base, sub1, 100, 100, 1);
    let replicated = a16(g(3).f().vstride(0), g(4).f().vec4());
    assert_field(base, replicated, 88, 85, 0);
}

/// mad(8) g10<1>F g11<4,4,1>F g12<4,4,1>F g13<4,4,1>F.
fn mad() -> Inst {
    Inst::mad(
        E8,
        g(10).f(),
        g(11).f().vec4(),
        g(12).f().vec4(),
        g(13).f().vec4(),
    )
}

fn mad_with(dst: Reg, s: [Reg; 3]) -> Inst {
    Inst::mad(E8, dst, s[0], s[1], s[2])
}

#[test]
fn three_source_destination() {
    let v = |nr| g(nr).f().vec4();
    let base = enc(mad());
    assert_eq!(bits(base, 8, 8), 1, "always Align16");
    assert_eq!(bits(base, 45, 42), 0, "float types");
    let srcs = [v(11), v(12), v(13)];
    assert_field(base, enc(mad_with(g(100).f(), srcs)), 63, 56, 100);
    assert_field(base, enc(mad_with(g(10).f().sub(5), srcs)), 55, 53, 5);
    let mask = g(10).f().writemask(WriteMask::X | WriteMask::Z);
    assert_field(base, enc(mad_with(mask, srcs)), 52, 49, 0x5);
    assert_field(base, enc(mad().sat()), 31, 31, 1);
    let flagged = enc(mad().cmod(CondMod::G, Flag::F1_1));
    assert_eq!(bits(flagged, 27, 24), 3);
    assert_eq!((bits(flagged, 34, 34), bits(flagged, 33, 33)), (1, 1));
    assert_eq!(
        bits(flagged, 90, 89),
        bits(base, 90, 89),
        "not the two-source place"
    );
}

#[test]
fn three_source_sources() {
    let v = |nr| g(nr).f().vec4();
    let base = enc(mad());
    let zyxw = Swizzle::new(Chan::Z, Chan::Y, Chan::X, Chan::W);
    // (reg, subreg, swizzle, rep, modifier) for each source.
    let layout = [
        ((83, 76), (75, 73), (72, 65), 64, (37, 36)),
        ((104, 97), (96, 94), (93, 86), 85, (39, 38)),
        ((125, 118), (117, 115), (114, 107), 106, (41, 40)),
    ];
    for (i, (reg, sub, swz, rep, modifier)) in layout.into_iter().enumerate() {
        let with = |r: Reg| {
            let mut s = [v(11), v(12), v(13)];
            s[i] = r;
            enc(mad_with(g(10).f(), s))
        };
        assert_field(base, with(v(120)), reg.0, reg.1, 120);
        assert_field(base, with(v(11 + i as u8).sub(6)), sub.0, sub.1, 6);
        assert_field(
            base,
            with(v(11 + i as u8).swizzle(zyxw)),
            swz.0,
            swz.1,
            0xC6,
        );
        let scalar = v(11 + i as u8).vstride(0);
        assert_field(base, with(scalar), rep, rep, 1);
        assert_field(base, with(v(11 + i as u8).abs()), modifier.0, modifier.1, 1);
        assert_field(
            base,
            with(v(11 + i as u8).negate()),
            modifier.0,
            modifier.1,
            2,
        );
    }
}

fn send(msg: Message) -> [u32; 4] {
    enc(Inst::send(E8, g(20).ud(), g(10).ud(), msg))
}

#[test]
fn send_puts_sfid_descriptor_and_eot_in_place() {
    let read = Message::oword_block_read(7, OWords::Two).unwrap();
    let words = send(read);
    assert_eq!(bits(words, 6, 0), 0x31);
    assert_eq!(bits(words, 27, 24), 10, "data cache");
    assert_eq!(bits(words, 43, 42), 3, "descriptor is an immediate");
    assert_eq!(bits(words, 46, 44), 0, "of type UD");
    assert_eq!(bits(words, 124, 121), 1, "mlen");
    assert_eq!(bits(words, 120, 116), 1, "rlen");
    assert_eq!(bits(words, 115, 115), 1, "header");
    assert_eq!(bits(words, 127, 127), 0, "no EOT");
    assert_field(words, send(read.desc_as_d()), 46, 44, 1);
    let sendc = enc(Inst::sendc(E8, g(20).ud(), g(10).ud(), read));
    assert_field(words, sendc, 6, 0, 0x32);
    let eot = enc(Inst::send(
        E8,
        Reg::null().ud(),
        g(112).ud(),
        Message::thread_spawner_eot(),
    ));
    assert_eq!(bits(eot, 127, 127), 1);
    assert_eq!(bits(eot, 27, 24), 7, "thread spawner");
}

#[test]
fn oword_block_descriptors() {
    // (size, block size code, registers of data)
    let sizes = [
        (OWords::OneLow, 0, 1),
        (OWords::OneHigh, 1, 1),
        (OWords::Two, 2, 1),
        (OWords::Four, 3, 2),
        (OWords::Eight, 4, 4),
    ];
    for (size, code, regs) in sizes {
        let read = send(Message::oword_block_read(3, size).unwrap());
        let desc = read[3];
        assert_eq!(
            desc,
            1 << 25 | regs << 20 | 1 << 19 | code << 8 | 3,
            "{size:?}"
        );
        let write = enc(Inst::send(
            E8,
            Reg::null().ud(),
            g(10).ud(),
            Message::oword_block_write(3, size).unwrap(),
        ));
        let desc = write[3];
        assert_eq!(desc, (1 + regs) << 25 | 1 << 19 | 8 << 14 | code << 8 | 3);
    }
}

#[test]
fn untyped_surface_descriptors() {
    let read = Message::untyped_surface_read(9, Simd::Simd8, 3).unwrap();
    // mlen 1, rlen 3, no header, type 5, SIMD8 (2), alpha masked (8).
    assert_eq!(
        send(read)[3],
        1 << 25 | 3 << 20 | 5 << 14 | 2 << 12 | 8 << 8 | 9
    );
    let write = Message::untyped_surface_write(9, Simd::Simd16, 2).unwrap();
    let words = enc(Inst::send(E16, Reg::null().ud(), g(10).ud(), write));
    // mlen 2 + 2 * 2, type 13, SIMD16 (1), blue and alpha masked (0xC).
    assert_eq!(words[3], 6 << 25 | 13 << 14 | 1 << 12 | 0xC << 8 | 9);
}

#[test]
fn media_block_and_thread_spawner_descriptors() {
    let read = Message::media_block_read(2, 4).unwrap();
    assert_eq!(send(read)[3], 1 << 25 | 4 << 20 | 1 << 19 | 4 << 14 | 2);
    assert_eq!(bits(send(read), 27, 24), 5, "render cache");
    let write = Message::media_block_write(2, 3).unwrap();
    let words = enc(Inst::send(E8, Reg::null().ud(), g(10).ud(), write));
    assert_eq!(words[3], 4 << 25 | 1 << 19 | 10 << 14 | 2);
    let eot = enc(Inst::send(
        E8,
        Reg::null().ud(),
        g(127).ud(),
        Message::thread_spawner_eot(),
    ));
    // EOT, mlen 1, resource select (bit 4): the URB handle stays.
    assert_eq!(eot[3], 1 << 31 | 1 << 25 | 1 << 4);
}

#[test]
fn region_encodings() {
    // Byte sources keep every region inside one register.
    for (v, code) in [(0, 0), (1, 1), (2, 2), (4, 3), (8, 4), (16, 5), (32, 6)] {
        let inst = Inst::mov(E2, g(2).uw(), g(3).ub().region(v, 1, 0));
        assert_eq!(bits(enc(inst), 88, 85), code, "vstride {v}");
    }
    for (w, code) in [(1, 0), (2, 1), (4, 2), (8, 3), (16, 4)] {
        let h = if w == 1 { 0 } else { 1 };
        let inst = Inst::mov(E16, g(2).uw(), g(3).ub().region(w, w, h));
        assert_eq!(bits(enc(inst), 84, 82), code, "width {w}");
    }
    // A full row: vstride = width * hstride, or anything with no stride.
    for (v, h, code) in [(4, 0, 0), (4, 1, 1), (8, 2, 2), (16, 4, 3)] {
        let inst = Inst::mov(E4, g(2).uw(), g(3).ub().region(v, 4, h));
        assert_eq!(bits(enc(inst), 81, 80), code, "hstride {h}");
    }
}
