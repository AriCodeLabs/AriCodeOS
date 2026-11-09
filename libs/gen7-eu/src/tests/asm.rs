// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The builder: buffer, first error, labels and jumps.

use super::{bits, enc};
use crate::ExecSize::*;
use crate::{g, Asm, AsmError, Error, Flag, Imm, Inst, Pred};

#[test]
fn chained_calls_fill_the_buffer_in_order() {
    let mut buf = [[0u32; 4]; 4];
    let mut asm = Asm::new(&mut buf);
    assert!(asm.is_empty());
    asm.mov(E8, g(2).ud(), g(3).ud())
        .add(E8, g(4).ud(), g(2).ud(), Imm::ud(1));
    assert_eq!(asm.len(), 2);
    let program = asm.finish().unwrap();
    assert_eq!(program.len(), 2);
    assert_eq!(program[0], enc(Inst::mov(E8, g(2).ud(), g(3).ud())));
    assert_eq!(
        program[1],
        enc(Inst::add(E8, g(4).ud(), g(2).ud(), Imm::ud(1)))
    );
    assert_eq!(program.as_flattened().len(), 8);
}

#[test]
fn every_shorthand_matches_its_instruction() {
    let mut buf = [[0u32; 4]; 8];
    let mut asm = Asm::new(&mut buf);
    let v = g(9).f().vec4();
    asm.mul(E8, g(2).f(), g(3).f(), g(4).f())
        .and(E8, g(2).ud(), g(3).ud(), g(4).ud())
        .or(E8, g(2).ud(), g(3).ud(), g(4).ud())
        .shl(E8, g(2).ud(), g(3).ud(), Imm::ud(2))
        .shr(E8, g(2).ud(), g(3).ud(), Imm::ud(2))
        .mad(E8, g(2).f(), v, v, v)
        .nop();
    let program = asm.finish().unwrap();
    let expected = [
        Inst::mul(E8, g(2).f(), g(3).f(), g(4).f()),
        Inst::and(E8, g(2).ud(), g(3).ud(), g(4).ud()),
        Inst::or(E8, g(2).ud(), g(3).ud(), g(4).ud()),
        Inst::shl(E8, g(2).ud(), g(3).ud(), Imm::ud(2)),
        Inst::shr(E8, g(2).ud(), g(3).ud(), Imm::ud(2)),
        Inst::mad(E8, g(2).f(), v, v, v),
    ];
    for (got, inst) in program.iter().zip(expected) {
        assert_eq!(*got, enc(inst));
    }
    // nop: the opcode alone (PRM Vol 4 Part 3, 6.44; Mesa `elk_NOP`).
    assert_eq!(program[6], [0x7e, 0, 0, 0]);
}

#[test]
fn the_first_refusal_stops_the_program_with_its_index() {
    let mut buf = [[0u32; 4]; 4];
    let mut asm = Asm::new(&mut buf);
    asm.mov(E8, g(2).ud(), g(3).ud())
        .mov(E8, g(200).ud(), g(3).ud())
        .mov(E8, g(2).ud(), g(201).ud())
        .mov(E8, g(2).ud(), g(3).ud());
    assert_eq!(asm.len(), 1);
    let error = asm.finish().unwrap_err();
    assert_eq!(
        error,
        AsmError {
            index: 1,
            error: Error::RegisterOutOfRange
        }
    );
}

#[test]
fn buffer_full() {
    let mut buf = [[0u32; 4]; 1];
    let mut asm = Asm::new(&mut buf);
    asm.nop().nop();
    let error = asm.finish().unwrap_err();
    assert_eq!(
        error,
        AsmError {
            index: 1,
            error: Error::BufferFull
        }
    );
}

#[test]
fn jmpi_counts_from_the_next_instruction_in_64_bit_units() {
    let mut buf = [[0u32; 4]; 8];
    let mut asm = Asm::new(&mut buf);
    let top = asm.label();
    asm.nop().nop().jmpi(None, top);
    let skip = asm.jmpi_forward(Some(Pred::new(Flag::F0_1)));
    asm.nop().nop().bind(skip).nop();
    let program = asm.finish().unwrap();
    // Back from 3 (next) to 0: three instructions, six units.
    assert_eq!(program[2][3], -6i32 as u32);
    assert_eq!(bits(program[2], 9, 9), 1, "NoMask");
    assert_eq!(bits(program[2], 6, 0), 0x20);
    // Forward from 4 (next) to 6: two instructions, four units.
    assert_eq!(program[3][3], 4);
    assert_eq!(bits(program[3], 19, 16), 1);
    assert_eq!(bits(program[3], 89, 89), 1, "f0.1");
}

#[test]
fn while_counts_from_itself() {
    let mut buf = [[0u32; 4]; 4];
    let mut asm = Asm::new(&mut buf);
    let start = asm.label();
    asm.nop()
        .nop()
        .while_(E8, None, start)
        .while_(E1, None, start);
    let program = asm.finish().unwrap();
    // JIP in the low word, UIP 0 above it.
    assert_eq!(program[2][3], 0xfffc);
    assert_eq!(program[3][3], 0xfffa);
    assert_eq!(bits(program[2], 6, 0), 0x27);
}

#[test]
fn jump_out_of_range() {
    // The field holds -32768 to 32767 units: 16384 instructions back, 16383
    // forward.
    let mut buf = std::vec![[0u32; 4]; 16_386];
    let mut asm = Asm::new(&mut buf);
    let top = asm.label();
    for _ in 0..16_383 {
        asm.nop();
    }
    asm.jmpi(None, top);
    assert!(asm.finish().is_ok(), "-32768 units fit");

    let mut asm = Asm::new(&mut buf);
    let top = asm.label();
    for _ in 0..16_384 {
        asm.nop();
    }
    asm.jmpi(None, top);
    let error = asm.finish().unwrap_err();
    assert_eq!(error.error, Error::JumpOutOfRange);

    let mut asm = Asm::new(&mut buf);
    let top = asm.label();
    for _ in 0..16_385 {
        asm.nop();
    }
    asm.while_(E8, None, top);
    assert_eq!(asm.finish().unwrap_err().error, Error::JumpOutOfRange);

    let mut asm = Asm::new(&mut buf);
    let skip = asm.jmpi_forward(None);
    for _ in 0..16_385 {
        asm.nop();
    }
    asm.bind(skip);
    assert_eq!(asm.finish().unwrap_err().error, Error::JumpOutOfRange);
}

#[test]
fn a_later_jump_error_keeps_the_first_refusal() {
    let mut buf = std::vec![[0u32; 4]; 16_384];
    let mut asm = Asm::new(&mut buf);
    let top = asm.label();
    for _ in 0..16_385 {
        asm.nop();
    }
    // The buffer is full; the jump back would also be too far.
    asm.jmpi(None, top);
    let error = asm.finish().unwrap_err();
    assert_eq!(
        error,
        AsmError {
            index: 16_384,
            error: Error::BufferFull
        }
    );
}
