// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! An assembler for the execution units (EUs) of Intel Ivy Bridge graphics
//! (gen7, HD Graphics 4000), for the hand-written compute kernels of stage
//! B of ADR 0080.
//!
//! It writes the 128-bit native instruction format only (no compaction),
//! with direct register addressing, for the opcodes a small matrix multiply
//! needs: `mov`, `add`, `mul`, `mad`, `and`, `or`, `shl`, `shr`, `cmp`,
//! `jmpi`, `while`, `nop` and `send`/`sendc` with data port and thread
//! spawner messages. Every instruction is checked before it is written;
//! [`Error`] names the rule a refused one breaks.
//!
//! Written from Intel's Ivy Bridge PRM (2012): Vol 4 Part 3 for the
//! instruction set, Vol 4 Part 1 for the data port messages, Vol 2 Part 2
//! for the thread spawner. Field positions were checked against Mesa's
//! `elk` compiler (MIT), and the tests reproduce, bit for bit, kernels
//! that ran on this hardware: igt's gen7 GPGPU fill kernel and Linux's Ivy
//! Bridge clear kernel, plus Mesa's gen7 assembler test vectors.
//!
//! ```
//! use aricode_gen7_eu::{g, Asm, ExecSize::*, Imm, Message, Reg};
//!
//! let mut buf = [[0u32; 4]; 8];
//! let mut asm = Asm::new(&mut buf);
//! asm.mov(E8, g(112).ud(), g(0).ud())
//!     .add(E1, g(2).ud(), g(1).ud().scalar(), Imm::ud(1))
//!     .send(E8, Reg::null().ud(), g(112).ud(), Message::thread_spawner_eot());
//! let program = asm.finish().unwrap();
//! assert_eq!(program.len(), 3);
//! ```

#![no_std]

mod asm;
mod error;
mod fields;
mod inst;
mod reg;
mod send;

pub use asm::{Asm, AsmError, Fixup, Label};
pub use error::Error;
pub use inst::{CondMod, ExecSize, Inst, Pred, PredCtrl, Qtr};
pub use reg::{g, Chan, Flag, Imm, Reg, Src, Swizzle, Type, WriteMask, GRF_COUNT, REG_BYTES};
pub use send::{Message, OWords, Simd};

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;
