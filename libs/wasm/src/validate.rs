// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Module validation before execution (stage 4, ADR 0019): the typing
//! algorithm from the specification appendix (typed operand stack, control
//! frames, polymorphic unreachable code) restricted to the subset the
//! interpreter executes. A module that does not validate is not
//! instantiated: out-of-range indices, unbalanced stacks and unsupported
//! instructions are rejected here, not halfway through execution.

use super::{
    skip_blocktype, skip_immediates, try_push, BlockEnds, BrTable, Error, Function, Module, Reader,
    SideTables, ValType, MAX_ARITY, MAX_FUNCTION_LOCALS,
};
use alloc::vec::Vec;

/// Maximum nested blocks inside a function.
const MAX_NESTING: usize = 1024;
/// Words of the per-`br_table` set of label depths already checked: a depth
/// is below the number of open frames (the function's and `MAX_NESTING`).
const DEPTH_WORDS: usize = (MAX_NESTING + 1).div_ceil(64);

/// Type on the validation stack: `None` is "unknown" (after unreachable
/// code), which matches anything.
type Slot = Option<ValType>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Func,
    Block,
    Loop,
    If,
    Else,
}

/// Control frame. Its types borrow the module's function types (or a
/// static one-type list): opening a block allocates nothing.
#[derive(Clone, Copy)]
struct Frame<'m> {
    kind: Kind,
    start: &'m [ValType],
    end: &'m [ValType],
    height: usize,
    unreachable: bool,
    /// Its entry in `SideTables::blocks` (`block` and `if`).
    block: Option<usize>,
}

struct Validator<'m, 'a> {
    module: &'m Module<'a>,
    /// Type (index) of each function, imported ones first.
    func_types: Vec<u32>,
    locals: Vec<ValType>,
    vals: Vec<Slot>,
    ctrls: Vec<Frame<'m>>,
    /// What execution looks up instead of decoding again (incident 0042).
    side: SideTables,
    /// Label depths of the current `br_table` already checked.
    checked: [u64; DEPTH_WORDS],
}

fn invalid(what: &'static str) -> Error {
    Error::Invalid(what)
}

const OUT_OF_MEMORY: Error = Error::OutOfMemory;

/// One-type result list of a block type such as `(result i32)`.
fn single(t: ValType) -> &'static [ValType] {
    match t {
        ValType::I32 => &[ValType::I32],
        ValType::I64 => &[ValType::I64],
        ValType::F32 => &[ValType::F32],
        ValType::F64 => &[ValType::F64],
    }
}

/// Type checks done by the validator (tests: validating a `br_table` is
/// not the number of its labels times their arity).
#[cfg(test)]
pub(crate) mod meter {
    std::thread_local! {
        pub static TYPE_CHECKS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
    }
}

impl<'m, 'a> Validator<'m, 'a> {
    fn push_val(&mut self, t: Slot) -> Result<(), Error> {
        try_push(&mut self.vals, t, OUT_OF_MEMORY)
    }

    fn pop_val(&mut self) -> Result<Slot, Error> {
        #[cfg(test)]
        meter::TYPE_CHECKS.with(|c| c.set(c.get() + 1));
        let frame = self.ctrls.last().ok_or(invalid("no frame"))?;
        if self.vals.len() == frame.height {
            return if frame.unreachable {
                Ok(None)
            } else {
                Err(invalid("empty stack"))
            };
        }
        self.vals.pop().ok_or(invalid("empty stack"))
    }

    fn pop_expect(&mut self, expect: ValType) -> Result<(), Error> {
        match self.pop_val()? {
            Some(actual) if actual != expect => Err(invalid("operand type")),
            _ => Ok(()),
        }
    }

    fn pop_vals(&mut self, types: &[ValType]) -> Result<(), Error> {
        for t in types.iter().rev() {
            self.pop_expect(*t)?;
        }
        Ok(())
    }

    fn push_vals(&mut self, types: &[ValType]) -> Result<(), Error> {
        for t in types {
            self.push_val(Some(*t))?;
        }
        Ok(())
    }

    fn push_ctrl(
        &mut self,
        kind: Kind,
        start: &'m [ValType],
        end: &'m [ValType],
        block: Option<usize>,
    ) -> Result<(), Error> {
        if self.ctrls.len() >= MAX_NESTING {
            return Err(invalid("nesting"));
        }
        let height = self.vals.len();
        self.push_vals(start)?;
        try_push(
            &mut self.ctrls,
            Frame {
                kind,
                start,
                end,
                height,
                unreachable: false,
                block,
            },
            OUT_OF_MEMORY,
        )
    }

    fn pop_ctrl(&mut self) -> Result<Frame<'m>, Error> {
        let frame = *self.ctrls.last().ok_or(invalid("end without block"))?;
        self.pop_vals(frame.end)?;
        if self.vals.len() != frame.height {
            return Err(invalid("leftover values at block end"));
        }
        self.ctrls.pop();
        Ok(frame)
    }

    fn label_types(&self, depth: u32) -> Result<&'m [ValType], Error> {
        let idx = self
            .ctrls
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or(invalid("label out of range"))?;
        let frame = &self.ctrls[idx];
        Ok(if frame.kind == Kind::Loop {
            frame.start
        } else {
            frame.end
        })
    }

    fn unreachable(&mut self) -> Result<(), Error> {
        let frame = self.ctrls.last_mut().ok_or(invalid("no frame"))?;
        self.vals.truncate(frame.height);
        frame.unreachable = true;
        Ok(())
    }

    fn func_type(&self, index: u32) -> Result<&'m super::FuncType, Error> {
        let ti = *self
            .func_types
            .get(index as usize)
            .ok_or(invalid("function out of range"))?;
        self.module
            .types
            .get(ti as usize)
            .ok_or(invalid("type out of range"))
    }

    /// Block type: empty, one result, or a function type by index.
    fn block_type(&self, r: &mut Reader) -> Result<(&'m [ValType], &'m [ValType]), Error> {
        let b = *r.b.get(r.pos).ok_or(invalid("block type"))?;
        if b == 0x40 {
            r.pos += 1;
            return Ok((&[], &[]));
        }
        if matches!(b, 0x7C..=0x7F) {
            r.pos += 1;
            return Ok((&[], single(r_valtype(b)?)));
        }
        let ti = r.i32()?;
        let ty = self
            .module
            .types
            .get(ti as usize)
            .ok_or(invalid("block type out of range"))?;
        Ok((&ty.params, &ty.results))
    }

    /// Records a `block` or `if` starting at `start` (after its type):
    /// starts come out in increasing order, so execution finds them by
    /// binary search. Its `else` and `end` are filled in when they appear.
    fn open_block(&mut self, start: usize) -> Result<Option<usize>, Error> {
        let index = self.side.blocks.len();
        let entry = BlockEnds {
            start: start as u32,
            else_pos: 0,
            end: 0,
        };
        try_push(&mut self.side.blocks, entry, OUT_OF_MEMORY)?;
        Ok(Some(index))
    }

    fn memory(&self) -> Result<(), Error> {
        if self.module.memory_pages.is_none() {
            return Err(invalid("memory instruction without memory"));
        }
        Ok(())
    }

    fn function(&mut self, f: &Function) -> Result<(), Error> {
        let ty = self.func_type(f.index)?;
        // `locals` was reserved once for the most a function can have
        // (`validate`): filling it never allocates.
        self.locals.clear();
        self.locals.extend_from_slice(&ty.params);
        let mut r = Reader::new(self.module.bytes, f.locals_decl);
        for _ in 0..r.u32()? {
            let n = r.u32()? as usize;
            let t = r.valtype()?;
            if n > self.locals.capacity() - self.locals.len() {
                return Err(Error::Unsupported("too many locals"));
            }
            self.locals.resize(self.locals.len() + n, t);
        }
        if r.pos != f.code_start {
            return Err(invalid("locals declaration"));
        }
        self.vals.clear();
        self.ctrls.clear();
        self.push_ctrl(Kind::Func, &[], &ty.results, None)?;
        let code = &self.module.bytes[..f.code_end];
        let mut r = Reader::new(code, f.code_start);
        while !self.ctrls.is_empty() {
            let op = r.u8()?;
            self.instruction(op, &mut r)?;
        }
        if r.pos != f.code_end {
            return Err(invalid("code after function end"));
        }
        Ok(())
    }

    /// `br_table` (`r` after the opcode): every label in range, with the
    /// default's arity, accepting what is on the stack. Its depths go to
    /// `SideTables::br_depths`, so execution picks one without decoding the
    /// others (audit 2026-10-25 of incident 0042, H1). Each distinct depth
    /// is type-checked once: checking a label leaves the top of the stack
    /// with its types, so a second check of the same depth cannot fail
    /// (audit L3: checking every label made it labels x arity).
    fn br_table(&mut self, r: &mut Reader) -> Result<(), Error> {
        let pos = r.pos as u32;
        let n = r.u32()?;
        // Each label takes at least one byte: a count past the code is
        // malformed, not a reason to reserve memory.
        if n as usize > r.b.len() - r.pos {
            return Err(Error::Malformed("br_table"));
        }
        let first = self.side.br_depths.len();
        for _ in 0..=n {
            let depth = r.u32()?;
            self.label_types(depth)?;
            // In range: below the number of open frames, which fits.
            try_push(&mut self.side.br_depths, depth as u16, OUT_OF_MEMORY)?;
        }
        let entry = BrTable {
            pos,
            first: first as u32,
            n,
        };
        try_push(&mut self.side.br_tables, entry, OUT_OF_MEMORY)?;
        let default = self.label_types(self.side.br_depths[first + n as usize] as u32)?;
        self.pop_expect(ValType::I32)?;
        self.checked = [0; DEPTH_WORDS];
        for i in first..first + n as usize {
            let depth = self.side.br_depths[i] as usize;
            let types = self.label_types(depth as u32)?;
            if types.len() != default.len() {
                return Err(invalid("br_table with different arities"));
            }
            let (word, bit) = (depth / 64, 1u64 << (depth % 64));
            if self.checked[word] & bit != 0 {
                continue;
            }
            self.checked[word] |= bit;
            // Every label must accept what is on the stack.
            self.pop_vals(types)?;
            self.push_vals(types)?;
        }
        self.pop_vals(default)?;
        self.unreachable()
    }

    fn instruction(&mut self, op: u8, r: &mut Reader) -> Result<(), Error> {
        use ValType::{I32, I64};
        match op {
            0x00 => self.unreachable()?,
            0x01 => {}
            0x02 | 0x03 => {
                let (start, end) = self.block_type(r)?;
                self.pop_vals(start)?;
                if op == 0x02 {
                    let block = self.open_block(r.pos)?;
                    self.push_ctrl(Kind::Block, start, end, block)?;
                } else {
                    // A `br` to a loop goes to its start: execution never
                    // needs where it ends.
                    self.push_ctrl(Kind::Loop, start, end, None)?;
                }
            }
            0x04 => {
                let (start, end) = self.block_type(r)?;
                self.pop_expect(I32)?;
                self.pop_vals(start)?;
                let block = self.open_block(r.pos)?;
                self.push_ctrl(Kind::If, start, end, block)?;
            }
            0x05 => {
                let frame = self.pop_ctrl()?;
                if frame.kind != Kind::If {
                    return Err(invalid("else without if"));
                }
                if let Some(b) = frame.block {
                    self.side.blocks[b].else_pos = r.pos as u32;
                }
                self.push_ctrl(Kind::Else, frame.start, frame.end, frame.block)?;
            }
            0x0B => {
                let frame = self.pop_ctrl()?;
                if frame.kind == Kind::If && frame.start != frame.end {
                    return Err(invalid("if without else with results"));
                }
                if let Some(b) = frame.block {
                    self.side.blocks[b].end = r.pos as u32;
                }
                self.push_vals(frame.end)?;
            }
            0x0C => {
                let types = self.label_types(r.u32()?)?;
                self.pop_vals(types)?;
                self.unreachable()?;
            }
            0x0D => {
                let types = self.label_types(r.u32()?)?;
                self.pop_expect(I32)?;
                self.pop_vals(types)?;
                self.push_vals(types)?;
            }
            0x0E => self.br_table(r)?,
            0x0F => {
                let end = self.ctrls[0].end;
                self.pop_vals(end)?;
                self.unreachable()?;
            }
            0x10 => {
                let ty = self.func_type(r.u32()?)?;
                self.pop_vals(&ty.params)?;
                self.push_vals(&ty.results)?;
            }
            0x11 => {
                let ti = r.u32()?;
                if r.u8()? != 0 {
                    return Err(invalid("call_indirect: table"));
                }
                if self.module.table.is_empty() {
                    return Err(invalid("call_indirect without table"));
                }
                let ty = self
                    .module
                    .types
                    .get(ti as usize)
                    .ok_or(invalid("type out of range"))?;
                self.pop_expect(I32)?;
                self.pop_vals(&ty.params)?;
                self.push_vals(&ty.results)?;
            }
            0x1A => {
                self.pop_val()?;
            }
            0x1B => {
                self.pop_expect(I32)?;
                let a = self.pop_val()?;
                let b = self.pop_val()?;
                let t = match (a, b) {
                    (Some(x), Some(y)) if x != y => {
                        return Err(invalid("select with different types"))
                    }
                    (Some(x), _) | (_, Some(x)) => Some(x),
                    (None, None) => None,
                };
                self.push_val(t)?;
            }
            0x1C => {
                if r.u32()? != 1 {
                    return Err(invalid("select with several types"));
                }
                let t = r.valtype()?;
                self.pop_expect(I32)?;
                self.pop_expect(t)?;
                self.pop_expect(t)?;
                self.push_val(Some(t))?;
            }
            0x20..=0x22 => {
                let t = *self
                    .locals
                    .get(r.u32()? as usize)
                    .ok_or(invalid("local out of range"))?;
                match op {
                    0x20 => self.push_val(Some(t))?,
                    0x21 => self.pop_expect(t)?,
                    _ => {
                        self.pop_expect(t)?;
                        self.push_val(Some(t))?;
                    }
                }
            }
            0x23 | 0x24 => {
                let g = *self
                    .module
                    .globals
                    .get(r.u32()? as usize)
                    .ok_or(invalid("global out of range"))?;
                if op == 0x23 {
                    self.push_val(Some(g.ty))?;
                } else {
                    if !g.mutable {
                        return Err(invalid("global.set on an immutable global"));
                    }
                    self.pop_expect(g.ty)?;
                }
            }
            0x28..=0x35 => {
                self.memory()?;
                r.u32()?;
                r.u32()?;
                let t = match op {
                    0x2A | 0x2B => return Err(Error::Unsupported("floating point")),
                    0x28 | 0x2C..=0x2F => I32,
                    _ => I64,
                };
                self.pop_expect(I32)?;
                self.push_val(Some(t))?;
            }
            0x36..=0x3E => {
                self.memory()?;
                r.u32()?;
                r.u32()?;
                let t = match op {
                    0x38 | 0x39 => return Err(Error::Unsupported("floating point")),
                    0x36 | 0x3A | 0x3B => I32,
                    _ => I64,
                };
                self.pop_expect(t)?;
                self.pop_expect(I32)?;
            }
            0x3F => {
                self.memory()?;
                r.u8()?;
                self.push_val(Some(I32))?;
            }
            0x40 => {
                self.memory()?;
                r.u8()?;
                self.pop_expect(I32)?;
                self.push_val(Some(I32))?;
            }
            0x41 => {
                r.i32()?;
                self.push_val(Some(I32))?;
            }
            0x42 => {
                r.i64()?;
                self.push_val(Some(I64))?;
            }
            0x43 | 0x44 => return Err(Error::Unsupported("floating point")),
            0x45 => {
                self.pop_expect(I32)?;
                self.push_val(Some(I32))?;
            }
            0x46..=0x4F => {
                self.pop_expect(I32)?;
                self.pop_expect(I32)?;
                self.push_val(Some(I32))?;
            }
            0x50 => {
                self.pop_expect(I64)?;
                self.push_val(Some(I32))?;
            }
            0x51..=0x5A => {
                self.pop_expect(I64)?;
                self.pop_expect(I64)?;
                self.push_val(Some(I32))?;
            }
            0x67..=0x69 => {
                self.pop_expect(I32)?;
                self.push_val(Some(I32))?;
            }
            0x6A..=0x78 => {
                self.pop_expect(I32)?;
                self.pop_expect(I32)?;
                self.push_val(Some(I32))?;
            }
            0x79..=0x7B => {
                self.pop_expect(I64)?;
                self.push_val(Some(I64))?;
            }
            0x7C..=0x8A => {
                self.pop_expect(I64)?;
                self.pop_expect(I64)?;
                self.push_val(Some(I64))?;
            }
            0xA7 => {
                self.pop_expect(I64)?;
                self.push_val(Some(I32))?;
            }
            0xAC | 0xAD => {
                self.pop_expect(I32)?;
                self.push_val(Some(I64))?;
            }
            0xC0 | 0xC1 => {
                self.pop_expect(I32)?;
                self.push_val(Some(I32))?;
            }
            0xC2..=0xC4 => {
                self.pop_expect(I64)?;
                self.push_val(Some(I64))?;
            }
            0xFC => match r.u32()? {
                10 => {
                    self.memory()?;
                    r.u8()?;
                    r.u8()?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                }
                11 => {
                    self.memory()?;
                    r.u8()?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                }
                _ => return Err(Error::Unsupported("0xFC prefix")),
            },
            0x5B..=0x66 | 0x8B..=0xA6 | 0xA8..=0xAB | 0xAE..=0xBF => {
                return Err(Error::Unsupported("floating point"))
            }
            _ => return Err(Error::Unsupported("opcode")),
        }
        Ok(())
    }
}

fn r_valtype(b: u8) -> Result<ValType, Error> {
    Ok(match b {
        0x7F => ValType::I32,
        0x7E => ValType::I64,
        0x7D => ValType::F32,
        0x7C => ValType::F64,
        _ => return Err(invalid("value type")),
    })
}

/// How many entries the side tables of `module` will hold, from a quick
/// pass over its code, so they are reserved once and exactly (no doubling
/// in the runner's small heap). Only a hint: on code it cannot read it
/// stops, validation reports the real error, and a short count only means
/// the tables grow as they fill.
fn count_side_entries(module: &Module) -> (usize, usize, usize) {
    let (mut blocks, mut br_tables, mut br_depths) = (0usize, 0usize, 0usize);
    for f in &module.functions {
        let mut r = Reader::new(&module.bytes[..f.code_end], f.code_start);
        while r.pos < r.b.len() {
            let Ok(op) = r.u8() else { break };
            let read = match op {
                0x02 | 0x04 => {
                    blocks += 1;
                    skip_blocktype(&mut r)
                }
                0x03 => skip_blocktype(&mut r),
                0x0E => {
                    let mut peek = Reader::new(r.b, r.pos);
                    if let Ok(n) = peek.u32() {
                        br_tables += 1;
                        br_depths += (n as usize).min(r.b.len() - r.pos) + 1;
                    }
                    skip_immediates(op, &mut r)
                }
                _ => skip_immediates(op, &mut r),
            };
            if read.is_err() {
                break;
            }
        }
    }
    (blocks, br_tables, br_depths)
}

/// Validates the whole module: indices of types, functions, table, globals,
/// exports and `start`, and the body of each function with the typing
/// algorithm. `Module::parse` calls it before returning the module and
/// keeps the side tables it builds on the way.
pub(crate) fn validate(module: &Module) -> Result<SideTables, Error> {
    let mut func_types = Vec::new();
    func_types
        .try_reserve_exact(module.imports.len() + module.functions.len())
        .map_err(|_| OUT_OF_MEMORY)?;
    for imp in &module.imports {
        if module.types.get(imp.type_index as usize).is_none() {
            return Err(invalid("import with type out of range"));
        }
        func_types.push(imp.type_index);
    }
    for f in &module.functions {
        if module.types.get(f.type_index as usize).is_none() {
            return Err(invalid("function with type out of range"));
        }
        func_types.push(f.type_index);
    }
    let total = func_types.len() as u32;
    for entry in module.table.iter().flatten() {
        if *entry >= total {
            return Err(invalid("table element out of range"));
        }
    }
    for e in &module.exports {
        let ok = match e.kind {
            0 => e.index < total,
            1 => module.table_limits.is_some() && e.index == 0,
            2 => module.memory_pages.is_some() && e.index == 0,
            3 => (e.index as usize) < module.globals.len(),
            _ => false,
        };
        if !ok {
            return Err(invalid("export out of range"));
        }
    }
    if let Some((min, max)) = module.memory_pages {
        if min > 65536 || max.is_some_and(|max| max < min || max > 65536) {
            return Err(invalid("memory limits"));
        }
    }
    if let Some((min, max)) = module.table_limits {
        if max.is_some_and(|max| max < min) {
            return Err(invalid("table limits"));
        }
    }
    if let Some(s) = module.start {
        let ti = *func_types
            .get(s as usize)
            .ok_or(invalid("start out of range"))?;
        let ty = &module.types[ti as usize];
        if !ty.params.is_empty() || !ty.results.is_empty() {
            return Err(invalid("start with params or results"));
        }
    }
    for g in &module.globals {
        if matches!(g.ty, ValType::F32 | ValType::F64) {
            return Err(Error::Unsupported("floating point"));
        }
        if g.init_ty != g.ty {
            return Err(invalid("global initializer type"));
        }
    }
    let mut side = SideTables::default();
    let (blocks, br_tables, br_depths) = count_side_entries(module);
    side.blocks
        .try_reserve_exact(blocks)
        .and_then(|_| side.br_tables.try_reserve_exact(br_tables))
        .and_then(|_| side.br_depths.try_reserve_exact(br_depths))
        .map_err(|_| OUT_OF_MEMORY)?;
    let mut locals = Vec::new();
    locals
        .try_reserve_exact(MAX_ARITY + MAX_FUNCTION_LOCALS)
        .map_err(|_| OUT_OF_MEMORY)?;
    let mut v = Validator {
        module,
        func_types,
        locals,
        vals: Vec::new(),
        ctrls: Vec::new(),
        side,
        checked: [0; DEPTH_WORDS],
    };
    for (i, f) in module.functions.iter().enumerate() {
        let f = Function {
            index: module.imports.len() as u32 + i as u32,
            ..*f
        };
        v.function(&f)?;
    }
    Ok(v.side)
}
