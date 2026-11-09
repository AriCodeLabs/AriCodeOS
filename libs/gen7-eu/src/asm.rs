// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The builder: instructions encoded one after another into a buffer the
//! caller owns, with labels for branches.
//!
//! Calls chain; the first instruction refused stops the program, and
//! [`Asm::finish`] reports it with its index, so a kernel is written as one
//! expression and checked once.

use crate::inst::{jump_units, patch_jmpi, ExecSize, Inst, Pred};
use crate::reg::{Reg, Src};
use crate::send::Message;
use crate::Error;

/// A place in the program a branch can go back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(usize);

/// A forward `jmpi` waiting for its target; [`Asm::bind`] gives it.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a forward jump needs its target bound"]
pub struct Fixup(usize);

/// The first instruction refused, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsmError {
    /// Its place in the program.
    pub index: usize,
    pub error: Error,
}

/// An assembler writing into `buf`, one `[u32; 4]` per instruction.
pub struct Asm<'a> {
    buf: &'a mut [[u32; 4]],
    len: usize,
    failed: Option<AsmError>,
}

impl<'a> Asm<'a> {
    pub fn new(buf: &'a mut [[u32; 4]]) -> Self {
        Asm {
            buf,
            len: 0,
            failed: None,
        }
    }

    /// Instructions written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn fail(&mut self, error: Error) {
        if self.failed.is_none() {
            self.failed = Some(AsmError {
                index: self.len,
                error,
            });
        }
    }

    /// Encodes `inst` as the next instruction.
    pub fn emit(&mut self, inst: Inst) -> &mut Self {
        if self.failed.is_some() {
            return self;
        }
        let Some(slot) = self.buf.get_mut(self.len) else {
            self.fail(Error::BufferFull);
            return self;
        };
        match inst.encode() {
            Ok(words) => {
                *slot = words;
                self.len += 1;
            }
            Err(error) => self.fail(error),
        }
        self
    }

    pub fn mov(&mut self, exec: ExecSize, dst: Reg, src: impl Into<Src>) -> &mut Self {
        self.emit(Inst::mov(exec, dst, src))
    }

    pub fn add(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> &mut Self {
        self.emit(Inst::add(exec, dst, src0, src1))
    }

    pub fn mul(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> &mut Self {
        self.emit(Inst::mul(exec, dst, src0, src1))
    }

    pub fn and(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> &mut Self {
        self.emit(Inst::and(exec, dst, src0, src1))
    }

    pub fn or(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> &mut Self {
        self.emit(Inst::or(exec, dst, src0, src1))
    }

    pub fn shl(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> &mut Self {
        self.emit(Inst::shl(exec, dst, src0, src1))
    }

    pub fn shr(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> &mut Self {
        self.emit(Inst::shr(exec, dst, src0, src1))
    }

    pub fn mad(&mut self, exec: ExecSize, dst: Reg, src0: Reg, src1: Reg, src2: Reg) -> &mut Self {
        self.emit(Inst::mad(exec, dst, src0, src1, src2))
    }

    pub fn send(&mut self, exec: ExecSize, dst: Reg, payload: Reg, msg: Message) -> &mut Self {
        self.emit(Inst::send(exec, dst, payload, msg))
    }

    /// An instruction that does nothing.
    pub fn nop(&mut self) -> &mut Self {
        self.emit(Inst::nop())
    }

    /// The place of the next instruction.
    pub fn label(&self) -> Label {
        Label(self.len)
    }

    /// Jumps to `target` when the predicate holds for the first channel, or
    /// always without one. A scalar branch: the loop of a kernel whose
    /// channels all agree.
    pub fn jmpi(&mut self, pred: Option<Pred>, target: Label) -> &mut Self {
        match jump_units(self.len + 1, target.0) {
            Ok(units) => self.emit(Inst::jmpi(pred, units)),
            Err(error) => {
                self.fail(error);
                self
            }
        }
    }

    /// A `jmpi` to a place not written yet; [`Asm::bind`] sets it.
    pub fn jmpi_forward(&mut self, pred: Option<Pred>) -> Fixup {
        let at = self.len;
        self.emit(Inst::jmpi(pred, 0));
        Fixup(at)
    }

    /// Points `fixup` at the next instruction.
    pub fn bind(&mut self, fixup: Fixup) -> &mut Self {
        if self.failed.is_some() {
            return self;
        }
        match jump_units(fixup.0 + 1, self.len) {
            Ok(units) => patch_jmpi(&mut self.buf[fixup.0], units),
            Err(error) => self.fail(error),
        }
        self
    }

    /// The end of a do-while loop that starts at `start`: the channels
    /// whose predicate holds go round again (PRM Vol 4 Part 3, 6.62). On
    /// gen7 the `do` is no instruction, only the label.
    pub fn while_(&mut self, exec: ExecSize, pred: Option<Pred>, start: Label) -> &mut Self {
        match jump_units(self.len, start.0) {
            Ok(units) => self.emit(Inst::while_(exec, pred, units)),
            Err(error) => {
                self.fail(error);
                self
            }
        }
    }

    /// The program, or the first instruction refused.
    pub fn finish(self) -> Result<&'a [[u32; 4]], AsmError> {
        match self.failed {
            Some(error) => Err(error),
            None => Ok(&self.buf[..self.len]),
        }
    }
}
