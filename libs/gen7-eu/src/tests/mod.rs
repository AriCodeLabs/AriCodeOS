// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Host tests. Field positions are written here as plain bit numbers read
//! from the PRM, not taken from `fields.rs`, so a wrong constant there
//! fails a test here.

mod asm;
mod checks;
mod fields;
mod igt;
mod linux;
mod mesa;

use crate::{Asm, Inst};

pub(super) fn enc(inst: Inst) -> [u32; 4] {
    match inst.encode() {
        Ok(words) => words,
        Err(error) => panic!("{inst:?} refused: {error:?}"),
    }
}

fn as_u128(words: [u32; 4]) -> u128 {
    words
        .iter()
        .rev()
        .fold(0u128, |acc, &word| acc << 32 | word as u128)
}

/// Bits `hi` to `lo` of an encoded instruction.
pub(super) fn bits(words: [u32; 4], hi: u32, lo: u32) -> u32 {
    let width = hi - lo + 1;
    ((as_u128(words) >> lo) & ((1u128 << width) - 1)) as u32
}

/// `changed` differs from `base` only inside bits `hi` to `lo`, which hold
/// `value`.
pub(super) fn assert_field(base: [u32; 4], changed: [u32; 4], hi: u32, lo: u32, value: u32) {
    let width = hi - lo + 1;
    let mask = ((1u128 << width) - 1) << lo;
    let diff = as_u128(base) ^ as_u128(changed);
    assert_eq!(
        diff & !mask,
        0,
        "bits outside {hi}:{lo} changed: {base:08x?} -> {changed:08x?}"
    );
    assert_eq!(
        bits(changed, hi, lo),
        value,
        "field {hi}:{lo} of {changed:08x?}"
    );
}

/// Assembles with `build` and compares with `expected`, skipping the
/// slots in `skip` (instructions outside this assembler's scope, stood in
/// for by `nop`).
pub(super) fn assert_program(expected: &[[u32; 4]], skip: &[usize], build: impl Fn(&mut Asm)) {
    let mut buf = std::vec![[0u32; 4]; expected.len()];
    let mut asm = Asm::new(&mut buf);
    build(&mut asm);
    let program = match asm.finish() {
        Ok(program) => program,
        Err(error) => panic!("refused: {error:?}"),
    };
    assert_eq!(program.len(), expected.len(), "instruction count");
    for (i, (got, want)) in program.iter().zip(expected).enumerate() {
        if !skip.contains(&i) {
            assert_eq!(got, want, "instruction {i}: {got:08x?} != {want:08x?}");
        }
    }
}
