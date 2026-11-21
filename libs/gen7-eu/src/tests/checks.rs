// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Every refusal: each test breaks one rule, and the instruction next to it
//! that keeps the rule is accepted, so the test shows the boundary.

use super::enc;
use crate::ExecSize::*;
use crate::{g, CondMod, Error, Flag, Imm, Inst, Message, OWords, Pred, PredCtrl, Qtr, Reg, Simd};

fn refused(inst: Inst) -> Error {
    match inst.encode() {
        Ok(words) => panic!("{inst:?} accepted as {words:08x?}"),
        Err(error) => error,
    }
}

fn mov8(dst: Reg, src: Reg) -> Inst {
    Inst::mov(E8, dst, src)
}

#[test]
fn register_out_of_range() {
    enc(mov8(g(127).ud(), g(0).ud()));
    assert_eq!(
        refused(mov8(g(128).ud(), g(0).ud())),
        Error::RegisterOutOfRange
    );
    assert_eq!(
        refused(mov8(g(2).ud(), g(255).ud())),
        Error::RegisterOutOfRange
    );
}

#[test]
fn subreg_out_of_range() {
    enc(Inst::mov(E1, g(2).ud().sub(7), Imm::ud(0)));
    let past = Inst::mov(E1, g(2).ud().sub(8), Imm::ud(0));
    assert_eq!(refused(past), Error::SubregOutOfRange);
    let src = Inst::mov(E1, g(2).ud(), g(3).uw().sub(16).scalar());
    assert_eq!(refused(src), Error::SubregOutOfRange);
}

#[test]
fn region_encoding() {
    for (v, w, h) in [(3, 8, 1), (8, 3, 1), (8, 8, 3), (64, 8, 1)] {
        let inst = mov8(g(2).ud(), g(3).ud().region(v, w, h));
        assert_eq!(refused(inst), Error::RegionEncoding, "<{v},{w},{h}>");
    }
}

#[test]
fn region_rules_of_part_2() {
    let src = |region: Reg| refused(mov8(g(2).ud(), region));
    // A: width above the execution size.
    assert_eq!(src(g(3).ud().region(16, 16, 1)), Error::RegionRule);
    // B: a full-width row with a stride needs vstride = width * hstride.
    assert_eq!(src(g(3).ud().region(4, 8, 1)), Error::RegionRule);
    // C: with no horizontal stride the vertical one is free.
    enc(mov8(g(2).ud(), g(3).ud().region(4, 8, 0)));
    // D: a width of 1 has no horizontal stride.
    assert_eq!(src(g(3).ud().region(1, 1, 1)), Error::RegionRule);
    // E: one channel of width 1 is a scalar.
    let e = Inst::mov(E1, g(2).ud(), g(3).ud().region(1, 1, 0));
    assert_eq!(refused(e), Error::RegionRule);
    // F: no strides at all means a width of 1.
    let f = Inst::mov(E4, g(2).ud(), g(3).ud().region(0, 4, 0));
    assert_eq!(refused(f), Error::RegionRule);
    // H: a row may not cross into the next register.
    assert_eq!(src(g(3).ud().sub(4)), Error::RegionRule);
    enc(Inst::mov(E4, g(2).ud(), g(3).ud().sub(4).region(4, 4, 1)));
}

#[test]
fn region_span() {
    // Sixteen dwords with a stride of 2 reach past two registers.
    let dst = Inst::mov(E16, g(2).ud().stride(2), Imm::ud(0));
    assert_eq!(refused(dst), Error::RegionSpan);
    enc(Inst::mov(E8, g(2).ud().stride(2), Imm::ud(0)));
    let src = Inst::mov(E16, g(2).uw(), g(3).ud().region(16, 8, 1));
    assert_eq!(refused(src), Error::RegionSpan);
    enc(Inst::mov(E16, g(2).uw(), g(3).ud()));
}

#[test]
fn dst_stride() {
    assert_eq!(
        refused(mov8(g(2).ud().stride(0), g(3).ud())),
        Error::DstStride
    );
    assert_eq!(
        refused(mov8(g(2).uw().stride(3), g(3).uw())),
        Error::DstStride
    );
    assert_eq!(
        refused(mov8(g(2).ud().scalar(), g(3).ud())),
        Error::DstStride
    );
}

#[test]
fn packed_byte_dst() {
    let add = Inst::add(E8, g(2).ub(), g(3).ub(), Imm::uw(1));
    assert_eq!(refused(add), Error::PackedByteDst);
    enc(Inst::add(E8, g(2).ub().stride(2), g(3).ub(), Imm::uw(1)));
    enc(Inst::mov(E8, g(2).ub(), g(3).ub()));
    // One byte is no packed region.
    enc(Inst::add(E1, g(2).ub(), g(3).ub().scalar(), Imm::uw(1)));
}

#[test]
fn align16_subreg() {
    let a16 = |dst: Reg, src: Reg| refused(Inst::mov(E8, dst, src).align16());
    assert_eq!(a16(g(2).f().sub(1), g(3).f().vec4()), Error::Align16Subreg);
    assert_eq!(a16(g(2).f(), g(3).f().vec4().sub(2)), Error::Align16Subreg);
    enc(Inst::mov(E8, g(2).f().sub(4), g(3).f().vec4().sub(4)).align16());
}

#[test]
fn align16_region() {
    // The Align1 default <8,8,1> has no Align16 form.
    assert_eq!(
        refused(mov8(g(2).f(), g(3).f()).align16()),
        Error::Align16Region
    );
    let mad = Inst::mad(E8, g(2).f(), g(3).f(), g(4).f().vec4(), g(5).f().vec4());
    assert_eq!(refused(mad), Error::Align16Region);
}

#[test]
fn align16_simd16() {
    let wide = Inst::mov(E16, g(2).f(), g(4).f().vec4()).align16();
    assert_eq!(refused(wide), Error::Align16Simd16);
    let v = g(4).f().vec4();
    assert_eq!(
        refused(Inst::mad(E16, g(2).f(), v, v, v)),
        Error::Align16Simd16
    );
    enc(Inst::mov(E16, g(2).uw(), g(4).uw().vec4()).align16());
}

#[test]
fn empty_writemask() {
    use crate::WriteMask;
    let none = g(2).f().writemask(WriteMask::NONE);
    let mov = Inst::mov(E8, none, g(3).f().vec4()).align16();
    assert_eq!(refused(mov), Error::EmptyWriteMask);
    let v = g(4).f().vec4();
    assert_eq!(refused(Inst::mad(E8, none, v, v, v)), Error::EmptyWriteMask);
    // The null register may have no channel enabled.
    let null = Reg::null().f().writemask(WriteMask::NONE);
    enc(Inst::mov(E8, null, g(3).f().vec4()).align16());
}

#[test]
fn predicate_mode() {
    let any4 = Pred::new(Flag::F0_0).ctrl(PredCtrl::Any4H);
    let a16 = Inst::mov(E8, g(2).f(), g(3).f().vec4()).align16();
    assert_eq!(refused(a16.pred(any4)), Error::PredicateMode);
    enc(a16.pred(Pred::new(Flag::F0_0)));
    enc(mov8(g(2).f(), g(3).f()).pred(any4));
}

#[test]
fn quarter_control() {
    let wide = Inst::mov(E16, g(2).ud(), g(4).ud());
    assert_eq!(refused(wide.qtr(Qtr::Q2)), Error::QuarterControl);
    assert_eq!(
        refused(mov8(g(2).ud(), g(3).ud()).qtr(Qtr::H2)),
        Error::QuarterControl
    );
    enc(wide.qtr(Qtr::H2));
    enc(mov8(g(2).ud(), g(3).ud()).qtr(Qtr::Q4));
}

#[test]
fn flag_conflict() {
    let inst = mov8(g(2).ud(), g(3).ud()).pred(Pred::new(Flag::F0_0));
    assert_eq!(
        refused(inst.cmod(CondMod::Z, Flag::F1_0)),
        Error::FlagConflict
    );
    enc(inst.cmod(CondMod::Z, Flag::F0_0));
}

#[test]
fn cond_mod_not_allowed() {
    let msg = Message::oword_block_read(0, OWords::Two).unwrap();
    let send = Inst::send(E8, g(20).ud(), g(10).ud(), msg);
    assert_eq!(
        refused(send.cmod(CondMod::Z, Flag::F0_0)),
        Error::CondModNotAllowed
    );
    enc(send);
}

#[test]
fn saturate_not_allowed() {
    let and = Inst::and(E8, g(2).ud(), g(3).ud(), Imm::ud(1));
    assert_eq!(refused(and.sat()), Error::SaturateNotAllowed);
    let or = Inst::or(E8, g(2).ud(), g(3).ud(), Imm::ud(1));
    assert_eq!(refused(or.sat()), Error::SaturateNotAllowed);
    let cmp = Inst::cmp(E8, CondMod::L, Flag::F0_0, g(2).f(), g(3).f(), Imm::f(0.0));
    assert_eq!(refused(cmp.sat()), Error::SaturateNotAllowed);
    let msg = Message::oword_block_read(0, OWords::Two).unwrap();
    let send = Inst::send(E8, g(20).ud(), g(10).ud(), msg);
    assert_eq!(refused(send.sat()), Error::SaturateNotAllowed);
    enc(Inst::shl(E8, g(2).ud(), g(3).ud(), Imm::ud(1)).sat());
}

#[test]
fn source_modifier() {
    assert_eq!(
        refused(mov8(g(2).d().negate(), g(3).d())),
        Error::SourceModifier
    );
    let msg = Message::oword_block_read(0, OWords::Two).unwrap();
    let send = Inst::send(E8, g(20).ud(), g(10).ud().abs(), msg);
    assert_eq!(refused(send), Error::SourceModifier);
    let v = g(4).f().vec4();
    assert_eq!(
        refused(Inst::mad(E8, g(2).f().negate(), v, v, v)),
        Error::SourceModifier
    );
}

#[test]
fn accumulator_not_src0() {
    let inst = Inst::add(E8, g(2).f(), g(3).f(), Reg::acc0().f());
    assert_eq!(refused(inst), Error::AccumulatorNotSrc0);
    enc(Inst::add(E8, g(2).f(), Reg::acc0().f(), g(3).f()));
}

#[test]
fn file() {
    let v = g(4).f().vec4();
    let arf = Reg::acc0().f().vec4();
    assert_eq!(refused(Inst::mad(E8, g(2).f(), arf, v, v)), Error::File);
    assert_eq!(
        refused(Inst::mad(E8, Reg::null().f(), v, v, v)),
        Error::File
    );
    let msg = Message::oword_block_read(0, OWords::Two).unwrap();
    let send = Inst::send(E8, g(20).ud(), Reg::null().ud(), msg);
    assert_eq!(refused(send), Error::File);
}

#[test]
fn operand_type() {
    let and = Inst::and(E8, g(2).f(), g(3).ud(), Imm::ud(1));
    assert_eq!(refused(and), Error::Type);
    let shl = Inst::shl(E8, g(2).ud(), g(3).ud(), Imm::f(1.0));
    assert_eq!(refused(shl), Error::Type);
    let or = Inst::or(E8, g(2).ud(), g(3).f(), g(4).ud());
    assert_eq!(refused(or), Error::Type);
    let shr = Inst::shr(E8, g(2).ud(), g(3).ud(), g(4).f());
    assert_eq!(refused(shr), Error::Type);
    let v = g(4).f().vec4();
    let d = g(4).d().vec4();
    assert_eq!(refused(Inst::mad(E8, g(2).f(), v, v, d)), Error::Type);
    assert_eq!(refused(Inst::mad(E8, g(2).d(), v, v, v)), Error::Type);
}

#[test]
fn eot_payload() {
    let eot = Message::thread_spawner_eot();
    let low = Inst::send(E8, Reg::null().ud(), g(111).ud(), eot);
    assert_eq!(refused(low), Error::EotPayload);
    enc(Inst::send(E8, Reg::null().ud(), g(112).ud(), eot));
}

#[test]
fn eot_response() {
    let eot = Message::thread_spawner_eot();
    let dst = Inst::send(E8, g(5).ud(), g(120).ud(), eot);
    assert_eq!(refused(dst), Error::EotResponse);
}

#[test]
fn send_response() {
    let read = Message::oword_block_read(0, OWords::Four).unwrap();
    let null = Inst::send(E8, Reg::null().ud(), g(10).ud(), read);
    assert_eq!(refused(null), Error::SendResponse);
    let acc = Inst::send(E8, Reg::acc0().ud(), g(10).ud(), read);
    assert_eq!(refused(acc), Error::SendResponse);
    let past = Inst::send(E8, g(127).ud(), g(10).ud(), read);
    assert_eq!(refused(past), Error::SendResponse);
    enc(Inst::send(E8, g(126).ud(), g(10).ud(), read));
}

#[test]
fn message_length() {
    let write = Message::oword_block_write(0, OWords::Two).unwrap();
    let past = Inst::send(E8, Reg::null().ud(), g(127).ud(), write);
    assert_eq!(refused(past), Error::MessageLength);
    enc(Inst::send(E8, Reg::null().ud(), g(126).ud(), write));
    assert_eq!(Message::media_block_write(0, 0), Err(Error::MessageLength));
    assert_eq!(Message::media_block_write(0, 15), Err(Error::MessageLength));
    assert!(Message::media_block_write(0, 14).is_ok());
    assert_eq!(Message::media_block_read(0, 0), Err(Error::MessageLength));
    assert_eq!(Message::media_block_read(0, 17), Err(Error::MessageLength));
    assert!(Message::media_block_read(0, 16).is_ok());
}

#[test]
fn message_parameter() {
    for channels in [0, 5] {
        let read = Message::untyped_surface_read(0, Simd::Simd8, channels);
        assert_eq!(read, Err(Error::MessageParameter));
        let write = Message::untyped_surface_write(0, Simd::Simd16, channels);
        assert_eq!(write, Err(Error::MessageParameter));
    }
    // Shared local memory takes no OWord block messages; stateless does.
    let slm = Message::oword_block_read(254, OWords::Two);
    assert_eq!(slm, Err(Error::MessageParameter));
    let slm = Message::oword_block_write(254, OWords::Two);
    assert_eq!(slm, Err(Error::MessageParameter));
    assert!(Message::oword_block_write(255, OWords::Two).is_ok());
}
